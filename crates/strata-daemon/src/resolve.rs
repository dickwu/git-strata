//! §5.5 `resolve_move` and `chain_resolve`: the two human resolutions.
//!
//! `resolve_move` never asks the `Confirmer` (cli already asked the human)
//! and calls `Sessions::resolve_failed_move` without the repo lock.
//! `chain_resolve` classifies the publish chain under the repo lock before
//! any compare-and-swap, writes the chain and the anchor in one ref
//! transaction, and pushes only for `Restore`, only fast-forward.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::{Value, json};
use strata_apfs::{IoSide, classify_io_error};
use strata_deps::{ChainState, PublishChain};
use strata_gitio::{Git, RefUpdate};
use strata_publish::{PUSH_TIMEOUT, anchor_ref, chain_ref};
use strata_session::{MoveResolution, ResolutionReport};
use strata_store::PublishPhase;
use strata_types::{Action, Code, CommitOid, PushOutcome, RepoName, SessionId, StrataError};

use crate::confirm_flow::{needs_confirm, recheck, require_validated};
use crate::fetch_after::observe_if_moved;
use crate::locks::Locks;
use crate::startup::Core;
use crate::{ChainChoice, DaemonWarning, Result};

/// Exactly one of `abandon` / `accept`; anything else is no choice (the
/// RPC layer already answers it `Failed`).
pub fn move_choice(abandon: bool, accept: bool) -> Option<MoveResolution> {
    match (abandon, accept) {
        (true, false) => Some(MoveResolution::Abandon),
        (false, true) => Some(MoveResolution::Accept),
        _ => None,
    }
}

/// Whether a publish of `session` is running (its `publishing` flag).
fn publishing(core: &Core, session: &SessionId) -> Result<bool> {
    Ok(core
        .store
        .open_publishes()?
        .iter()
        .any(|r| r.phase == PublishPhase::Running && r.group.batches.iter().any(|b| &b.session == session)))
}

/// Wait for `session`'s publishing flag to clear, holding no queue and no
/// lock, at most the lock wait (then `Busy`, nothing written).
pub fn wait_not_publishing(core: &Core, session: &SessionId) -> Result<()> {
    wait_not_publishing_until(core, session, Instant::now() + core.locks.wait())
}

fn wait_not_publishing_until(core: &Core, session: &SessionId, deadline: Instant) -> Result<()> {
    wait_flag_until(session, deadline, || publishing(core, session))
}

fn publishing_busy(session: &SessionId) -> StrataError {
    StrataError::new(Code::Busy, format!("session {session} is publishing"))
        .with_detail(json!({ "reason": "publishing", "session": session.as_str() }))
}

fn wait_flag_until(session: &SessionId, deadline: Instant, mut flag: impl FnMut() -> Result<bool>) -> Result<()> {
    while flag()? {
        if Instant::now() >= deadline {
            return Err(publishing_busy(session));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Run `f` under `session`'s queue place once its `publishing` flag is
/// clear (daemon.md §3: rm and resolve_move wait for the flag, holding no
/// queue, repo lock or build lock while they wait).
///
/// [实现时补的决定] (review M5) The flag is checked again under the queue
/// place: a publish that began (or reached its lock-free rebase gap) between
/// the wait and the queue would otherwise still be raced. If it is set
/// again, the queue place is given back and the wait starts over; the whole
/// wait is bounded by one lock wait, then `Busy` (`reason: "publishing"`),
/// nothing written.
///
/// [实现时补的决定] (review-2 N5) "The whole wait" is one absolute
/// deadline (start + lock wait) shared by every flag wait and every queue
/// take of the loop (`Locks::with_sessions_until`); a later round's queue
/// take is no longer unbounded. Expiry in a flag wait is `Busy`
/// `publishing`; expiry in a queue take is `Busy` `lock_wait` (naming the
/// holder). Either way nothing is written.
///
/// [实现时补的决定] (review-3 R5) The deadline is checked once more under
/// the queue place, right before `f`: a flag that cleared during the last
/// poll, or a free queue taken with a zero bound after expiry, no longer
/// lets `f` start late. That expiry is `Busy` `lock_wait` with
/// `deadline: true`; nothing is written. Once `f` runs it is not bounded.
pub fn with_queue_not_publishing<T>(core: &Core, session: &SessionId, f: impl FnOnce() -> Result<T>) -> Result<T> {
    guarded_until(&core.locks, session, Instant::now() + core.locks.wait(), || publishing(core, session), f)
}

/// The loop of [`with_queue_not_publishing`] with the flag as a closure
/// and an explicit deadline (public for the helper-level tests).
#[doc(hidden)]
pub fn guarded_until<T>(
    locks: &Locks,
    session: &SessionId,
    deadline: Instant,
    mut flag: impl FnMut() -> Result<bool>,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let mut f = Some(f);
    loop {
        wait_flag_until(session, deadline, &mut flag)?;
        let r = locks.with_sessions_until(std::slice::from_ref(session), deadline, || {
            if flag()? {
                return Ok(None);
            }
            if Instant::now() >= deadline {
                return Err(StrataError::new(Code::Busy, format!("the wait for session {session} ran out before the step could start"))
                    .with_detail(json!({ "reason": "lock_wait", "session": session.as_str(), "deadline": true })));
            }
            match f.take() {
                Some(f) => f().map(Some),
                None => Err(StrataError::new(Code::Unknown, "the guarded step already ran")),
            }
        })?;
        if let Some(v) = r {
            return Ok(v);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// §5.5 `resolve_move`. No TTY check, no `Confirmer`. `resolve` is
/// `Sessions::resolve_failed_move` (it takes the repo lock itself); it runs
/// holding only the session queue. `after_accept` resets the Tracker and
/// watches again; `Abandon` keeps the tree and the Tracker.
pub fn resolve_move(
    core: &Core,
    session: &SessionId,
    repo: &RepoName,
    choice: MoveResolution,
    resolve: &dyn Fn(&SessionId, &RepoName, MoveResolution) -> Result<ResolutionReport>,
    after_accept: &mut dyn FnMut(),
) -> Result<ResolutionReport> {
    with_queue_not_publishing(core, session, || {
        crate::handlers::writing();
        let r = resolve(session, repo, choice)?;
        if choice == MoveResolution::Accept {
            after_accept();
        }
        Ok(r)
    })
}

/// What `chain_resolve` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChainResolved {
    pub repo: RepoName,
    pub branch: String,
    pub choice: ChainChoice,
    /// The chain tip and the anchor afterwards.
    pub chain: String,
    pub anchor: String,
    pub pushed: bool,
    /// `ObserveFailed` after the lock, if observing `main` failed twice.
    pub warnings: Vec<Value>,
}

fn invalid(reason: &str, message: impl Into<String>) -> StrataError {
    StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": reason }))
}

fn unknown(reason: &str, e: &StrataError) -> StrataError {
    StrataError::new(Code::Unknown, format!("{}; the refs may have changed", e.message))
        .with_detail(json!({ "reason": reason, "cause": e.code.as_str() }))
}

fn rewound_or_diverged(c: Option<PublishChain>) -> Result<(PublishChain, CommitOid)> {
    match c {
        Some(c) if matches!(c.state, ChainState::Rewound | ChainState::Diverged) => {
            let top = c.top.clone().ok_or_else(|| invalid("not_rewound_or_diverged", "the publish chain is empty"))?;
            Ok((c, top))
        }
        c => Err(invalid(
            "not_rewound_or_diverged",
            format!("the publish chain is {:?}, not Rewound or Diverged", c.map(|c| c.state)),
        )),
    }
}

/// The one chain-and-anchor transaction; once it is called, any failure is
/// `Unknown`.
fn transaction(git: &Git, updates: &[RefUpdate]) -> Result<()> {
    crate::handlers::writing();
    git.ref_transaction(updates).map_err(|e| unknown("ref_transaction", &e))
}

/// The start of `AcceptOriginReplay`'s range: the anchor advanced in
/// memory along a `Diverged` chain, the current anchor for `Rewound`.
pub fn replay_base(git: &Git, chain: &PublishChain, c: &CommitOid) -> Result<Option<CommitOid>> {
    let Some(a) = chain.anchor.clone() else {
        return Ok(None);
    };
    let base = if chain.state == ChainState::Diverged {
        match git.merge_base(&chain.origin, c)? {
            Some(m) if m != a && git.is_ancestor(&a, &m)? => m,
            _ => a,
        }
    } else {
        a
    };
    Ok(git.is_ancestor(&base, c)?.then_some(base))
}

/// A staging tree attached at `tip` under `<root>/daemon/push/`, removed
/// afterwards whatever `f` returned.
///
/// Review M6 (design.md §9 invariant 6: an uncertain push keeps its scene,
/// is not replayed and is reported). [实现时补的决定] Before anything is
/// exported the directory gets `push.json` (`repo`, `commit`): a durable
/// record of the attempt. When `uncertain(&result)` holds (a push whose
/// outcome is unknown) the directory is kept, an `UnknownPush` warning is
/// raised and an error result carries `detail.scene`; a restart reports
/// every kept scene again (`push_scenes`). Otherwise the directory is
/// removed. A later definite `Pushed` of a commit that contains the
/// scene's commit settles it (`settle_push_scenes`).
///
/// [实现时补的决定] (review-2 N4) The record also names the remote and the
/// branch the push went to (`remote`, `branch`); only a definite push to the
/// same repo, remote and branch settles it.
///
/// [实现时补的决定] (review-3 R4) It also names the repository's identity
/// (`repo_id`, the realpath of its common git dir) and the push destination
/// (`push_url`, `dest` read before the push; null when unreadable). Only a
/// definite push whose `PushDest` equals both exactly settles it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn with_staging<T>(
    core: &Core,
    git: &Git,
    repo: &RepoName,
    branch: &str,
    tip: &CommitOid,
    dest: Option<&PushDest>,
    uncertain: impl Fn(&Result<T>) -> bool,
    f: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = core.config.paths.root.join(PUSH_SCENES).join(format!("{}-{stamp}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| classify_io_error(&e, IoSide::Write, &dir))?;
    let intent = dir.join(SCENE_FILE);
    let body = json!({
        "repo": repo.as_str(), "commit": tip.as_str(), "remote": SCENE_REMOTE, "branch": branch,
        "repo_id": dest.map(|d| d.repo_id.get().to_string_lossy().into_owned()),
        "push_url": dest.map(|d| d.push_url.as_str()),
    })
    .to_string();
    if let Err(e) = write_scene_record(&dir, &intent, body.as_bytes()) {
        let _ = fs::remove_dir_all(&dir);
        return Err(e);
    }
    let r = (|| {
        let wt = dir.join(repo.as_str());
        let gd = dir.join(format!(".git-{repo}"));
        git.export_tree(&git.tree_of(tip)?, &wt)?;
        git.attach_staging(&wt, &gd, tip)?;
        f(&wt)
    })();
    if uncertain(&r) {
        core.warnings.add(DaemonWarning::UnknownPush { repo: repo.clone(), commit: tip.clone(), scene: dir.clone() });
        return r.map_err(|mut e| {
            let mut d = e.detail.take().unwrap_or_else(|| json!({}));
            if let Some(m) = d.as_object_mut() {
                m.insert("scene".into(), json!(dir.to_string_lossy()));
            }
            e.with_detail(d)
        });
    }
    let _ = fs::remove_dir_all(&dir);
    r
}

/// Review P1-13 (D34): the attempt record must be on the medium before
/// anything is pushed, or a power loss after the push leaves no scene and
/// the unknown push is never reported. Temp file → write → F_FULLFSYNC →
/// rename → F_FULLFSYNC the scene directory and its parent (the scene
/// directory itself was just created). Any failure is returned and the
/// caller pushes nothing.
fn write_scene_record(dir: &Path, intent: &Path, body: &[u8]) -> Result<()> {
    use std::io::Write;
    let io = |e: std::io::Error, p: &Path| classify_io_error(&e, IoSide::Write, p);
    let tmp = dir.join(format!(".{SCENE_FILE}.tmp"));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| io(e, &tmp))?;
        f.write_all(body).map_err(|e| io(e, &tmp))?;
        sync_path_file(&f, &tmp)?;
    }
    fs::rename(&tmp, intent).map_err(|e| io(e, intent))?;
    for d in [Some(dir), dir.parent()].into_iter().flatten() {
        let f = fs::File::open(d).map_err(|e| io(e, d))?;
        sync_path_file(&f, d)?;
    }
    Ok(())
}

/// F_FULLFSYNC, falling back to fsync where the volume does not support it.
fn sync_path_file(f: &fs::File, shown: &Path) -> Result<()> {
    #[cfg(feature = "test-seams")]
    if crate::seams::scene_sync_fails(shown) {
        return Err(classify_io_error(&std::io::Error::from_raw_os_error(libc::EIO), IoSide::Write, shown));
    }
    match strata_apfs::full_fsync(f) {
        Ok(()) => Ok(()),
        Err(e) if e.code == Code::Unsupported => f.sync_all().map_err(|e| classify_io_error(&e, IoSide::Write, shown)),
        Err(e) => Err(e),
    }
}

/// Where standalone and restore pushes stage (under `paths.root`).
pub(crate) const PUSH_SCENES: &str = "daemon/push";
/// The attempt record inside a push directory.
const SCENE_FILE: &str = "push.json";
/// The remote every standalone and restore push goes to.
pub(crate) const SCENE_REMOTE: &str = "origin";

/// Review M6: every kept push scene under `root` (`repo`, `commit`, the
/// directory), for the startup report. A directory without a readable
/// record is skipped (nothing was pushed from it: the record is written
/// before the export).
pub fn push_scenes(root: &Path) -> Vec<(RepoName, CommitOid, std::path::PathBuf)> {
    scene_records(root).into_iter().map(|s| (s.repo, s.commit, s.dir)).collect()
}

/// Review-3 R4: where a standalone or restore push goes — the repository's
/// identity (`Git::repo_id`) and origin's effective push URL
/// (`Git::push_url`, local config only). [实现时补的决定]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PushDest {
    repo_id: strata_types::RepoId,
    push_url: String,
}

/// The destination a push to origin from `git` goes to now; `None` when
/// origin's push URL cannot be read (such a push fails, and its scene, if
/// any, is never settled automatically).
impl PushDest {
    /// The push URL the push is sent to (review-4 F1).
    pub(crate) fn url(&self) -> &str {
        &self.push_url
    }
}

/// Review P1-15: under the repo lock, before any staging, origin's push
/// destination read now (`dest`) must have the push URL the human approved
/// (`approved`); otherwise `NotConfirmed` (`remote_changed`) and nothing is
/// staged or pushed. An unreadable URL is the same refusal (now `""`).
pub(crate) fn check_approved_url(approved: &str, dest: Option<&PushDest>) -> Result<()> {
    match dest {
        Some(d) if d.push_url == approved => Ok(()),
        d => Err(strata_publish::remote_changed(approved, d.map(PushDest::url).unwrap_or(""))),
    }
}

pub(crate) fn push_dest(git: &Git) -> Option<PushDest> {
    let push_url = git.push_url(SCENE_REMOTE).ok()?;
    Some(PushDest { repo_id: git.repo_id(), push_url })
}

/// One kept scene's record; `remote` / `branch` are `None` in a record
/// written before review-2 N4, `repo_id` / `push_url` in one written before
/// review-3 R4 (neither is ever settled automatically).
struct SceneRecord {
    repo: RepoName,
    commit: CommitOid,
    remote: Option<String>,
    branch: Option<String>,
    repo_id: Option<String>,
    push_url: Option<String>,
    dir: std::path::PathBuf,
}

fn scene_records(root: &Path) -> Vec<SceneRecord> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root.join(PUSH_SCENES)) else { return out };
    for e in rd.flatten() {
        let dir = e.path();
        let Ok(text) = fs::read_to_string(dir.join(SCENE_FILE)) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let repo = v["repo"].as_str().and_then(|r| serde_json::from_value::<RepoName>(json!(r)).ok());
        let commit = v["commit"].as_str().and_then(|c| serde_json::from_value::<CommitOid>(json!(c)).ok());
        let text_of = |k: &str| v[k].as_str().map(str::to_string);
        if let (Some(repo), Some(commit)) = (repo, commit) {
            out.push(SceneRecord {
                repo,
                commit,
                remote: text_of("remote"),
                branch: text_of("branch"),
                repo_id: text_of("repo_id"),
                push_url: text_of("push_url"),
                dir,
            });
        }
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// Review M6: after a definite `Pushed` of `pushed` to `branch` of
/// `repo`'s origin, the kept scenes of the same repo, remote and branch
/// (review-2 N4) whose commit `pushed` contains are settled — that branch
/// has the commit now — so their directory and warning go. Scenes of other
/// branches, of commits not contained, and records without a remote and
/// branch stay [实现时补的决定].
///
/// [实现时补的决定] (review-3 R4) `dest` is the destination read before
/// the push and a scene settles only when its recorded `repo_id` and
/// `push_url` equal it exactly (byte-for-byte strings: an alias of the same
/// remote does not settle). A record without them stays.
///
/// [实现时补的决定] (review-4 F1) The push was sent to exactly
/// `dest.push_url` (gitio `push_*_to`, pinned against URL rewriting).
/// `dest` `None` (no readable push URL) → nothing settles.
///
/// [实现时补的决定] (review-5 N1) Safety net after the push, as before
/// review-4: origin's push destination read now must still equal `dest`,
/// and `dest.push_url` must still be a URL git pushes to as given
/// (`Git::push_url_exact`); otherwise nothing settles (the scenes stay,
/// reported).
pub(crate) fn settle_push_scenes(core: &Core, git: &Git, repo: &RepoName, branch: &str, pushed: &CommitOid, dest: Option<&PushDest>) {
    let Some(dest) = dest else { return };
    if push_dest(git).as_ref() != Some(dest) || !git.push_url_exact(&dest.push_url).unwrap_or(false) {
        return;
    }
    let repo_id = dest.repo_id.get().to_string_lossy();
    for s in scene_records(&core.config.paths.root) {
        let same = s.repo == *repo
            && s.remote.as_deref() == Some(SCENE_REMOTE)
            && s.branch.as_deref() == Some(branch)
            && s.repo_id.as_deref() == Some(&*repo_id)
            && s.push_url.as_deref() == Some(dest.push_url.as_str());
        if !same || !git.is_ancestor(&s.commit, pushed).unwrap_or(false) {
            continue;
        }
        if fs::remove_dir_all(&s.dir).is_ok() {
            let gone = DaemonWarning::UnknownPush { repo: s.repo, commit: s.commit, scene: s.dir };
            core.warnings.remove(|w| *w == gone);
        }
    }
}

/// `deps::observe_main` for `tip` of `repo`: its tree exported to a
/// directory under `<root>/daemon/observe/`, removed afterwards. Takes the
/// repo lock itself (through deps).
pub fn observe_tip(core: &Core, repo: &RepoName, tip: &CommitOid) -> Result<()> {
    let git = core.git(repo)?;
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = core.config.paths.root.join("daemon/observe").join(format!("{}-{stamp}", std::process::id()));
    let r = (|| {
        let tree = git.tree_of(tip)?;
        // The export needs its parent; nothing else creates `daemon/observe`.
        let parent = dir.parent().expect("observe dir has a parent");
        fs::create_dir_all(parent).map_err(|e| classify_io_error(&e, IoSide::Write, parent))?;
        git.export_tree(&tree, &dir)?;
        let ctx = strata_deps::Ctx {
            store: &core.store,
            paths: &core.config.paths,
            repo,
            cfg: core.repo_cfg(repo)?,
            git,
            env: &core.env,
            locks: &core.locks,
        };
        strata_deps::observe_main(&ctx, &tree, tip, &dir).map(|_| ())
    })();
    let _ = fs::remove_dir_all(&dir);
    r
}

/// §5.5 `chain_resolve`. `observe` is called (after the lock, at most
/// twice) with the new `main_tip` when it moved.
pub fn chain_resolve(
    core: &Core,
    repo: &RepoName,
    choice: ChainChoice,
    shown: Option<&CommitOid>,
    observe: &mut dyn FnMut(&CommitOid) -> Result<()>,
) -> Result<ChainResolved> {
    let cfg = core.repo_cfg(repo)?;
    let git = core.git(repo)?;
    let branch = cfg.publish.branch.clone();
    let (chain_name, anchor_name) = (chain_ref(&branch), anchor_ref(&branch));

    // 2. Read once without the lock; no fetch when it is not a fork.
    let (_, seen) = rewound_or_diverged(strata_deps::publish_chain(git, &branch)?)?;

    // 3. Restore asks the human like push, holding nothing.
    // Review P1-15: the approval names origin's push URL read now.
    let confirm = choice == ChainChoice::Restore && needs_confirm(core.pinned(repo)?);
    let url = if confirm { git.push_url(SCENE_REMOTE)? } else { String::new() };
    let action = Action::PushDeploy { repo: repo.clone(), branch: branch.clone(), url: url.clone() };
    let token = if confirm {
        Some(require_validated(&core.confirmer, action.clone())?)
    } else {
        None
    };
    if token.is_some() {
        crate::after_confirm(&core.config.paths.root);
    }

    let mut tips = (None, None);
    let out = core.locks.with_repo(repo, || {
        tips.0 = strata_deps::main_tip(cfg, git).ok().flatten();
        // 4. Fetch, read again, classify before any compare-and-swap.
        git.fetch("origin", &branch)?;
        let (chain, c) = rewound_or_diverged(strata_deps::publish_chain(git, &branch)?)?;
        let (o, a) = (chain.origin.clone(), chain.anchor.clone());
        let mut done = ChainResolved {
            repo: repo.clone(),
            branch: branch.clone(),
            choice,
            chain: c.as_str().to_string(),
            anchor: a.as_ref().map(|a| a.as_str().to_string()).unwrap_or_default(),
            pushed: false,
            warnings: Vec::new(),
        };
        match choice {
            ChainChoice::Restore => {
                if !git.is_ancestor(&o, &c)? {
                    return Err(invalid("not_fast_forward", format!("origin/{branch} is not an ancestor of the publish chain")));
                }
                if let Some(t) = &token {
                    recheck(t, &action)?;
                    // Review H2: bound to what the cli showed, and to the
                    // daemon's own read before the prompt.
                    if c != seen || shown != Some(&c) {
                        return Err(StrataError::new(Code::NotConfirmed, "the publish chain moved after the confirmation")
                            .with_detail(json!({
                                "reason": "chain_moved",
                                "confirmed": shown.map(|s| s.as_str()),
                                "now": c.as_str(),
                            })));
                    }
                }
                // 8. Push the chain tip. Review M7: `Pushed` only once one
                // transaction has checked the chain and moved the tracking
                // ref and the anchor together (gitio `push_recorded`, as
                // publish's `push_published`); a transaction that does not
                // commit is `Unknown` and nothing local moved.
                let verify = [RefUpdate::verify(&chain_name, Some(c.clone()))];
                let uncertain = |r: &Result<PushOutcome>| r.as_ref().is_err_and(|e| e.code == Code::Unknown);
                let dest = push_dest(git);
                if token.is_some() {
                    check_approved_url(&url, dest.as_ref())?;
                }
                let outcome = with_staging(core, git, repo, &branch, &c, dest.as_ref(), uncertain, |wt| {
                    crate::handlers::writing();
                    // Review-4 F1: to exactly the recorded destination.
                    let sent = match &dest {
                        Some(d) => git.push_recorded_to(wt, "origin", &d.push_url, &chain_name, &branch, &anchor_name, &verify, PUSH_TIMEOUT)?,
                        None => git.push_recorded(wt, "origin", &chain_name, &branch, &anchor_name, &verify, PUSH_TIMEOUT)?,
                    };
                    match sent {
                        PushOutcome::Unknown => Err(StrataError::new(Code::Unknown, "the push outcome is unknown; not pushed again")
                            .with_detail(json!({ "reason": "push_unknown", "branch": branch }))),
                        o => Ok(o),
                    }
                })?;
                match outcome {
                    PushOutcome::Pushed => {}
                    PushOutcome::Refused(msg) => {
                        return Err(StrataError::new(Code::PushRefused, format!("origin refused the push: {msg}"))
                            .with_detail(json!({ "reason": "push_refused", "branch": branch })));
                    }
                    PushOutcome::Unknown => {
                        return Err(StrataError::new(Code::Unknown, "the push outcome is unknown; not pushed again")
                            .with_detail(json!({ "reason": "push_unknown", "branch": branch })));
                    }
                }
                done.pushed = true;
                done.anchor = c.as_str().to_string();
                // Review-2 N4: a definite restore push settles kept scenes too.
                settle_push_scenes(core, git, repo, &branch, &c, dest.as_ref());
            }
            ChainChoice::AcceptOriginDrop => {
                transaction(git, &[RefUpdate::set(&chain_name, o.clone(), Some(c.clone())), RefUpdate::set(&anchor_name, o.clone(), a)])?;
                done.chain = o.as_str().to_string();
                done.anchor = o.as_str().to_string();
            }
            ChainChoice::AcceptOriginReplay => {
                let Some(base) = replay_base(git, &chain, &c)? else {
                    return Err(invalid("no_replay_range", "no replay range: the chain has no anchor on it"));
                };
                // A Conflict here comes before any ref write: the refs are unchanged.
                let tip = git.replay_commits(&base, &c, &o)?;
                transaction(git, &[RefUpdate::set(&chain_name, tip.clone(), Some(c.clone())), RefUpdate::set(&anchor_name, o.clone(), a)])?;
                done.chain = tip.as_str().to_string();
                done.anchor = o.as_str().to_string();
            }
        }
        tips.1 = strata_deps::main_tip(cfg, git).ok().flatten();
        Ok(done)
    });
    // 9. After the lock: observe when main moved; failure is a warning.
    let mut done = out?;
    if let Some(after) = tips.1.clone()
        && let Some(e) = observe_if_moved(tips.0.as_ref(), Some(&after), &mut || observe(&after))
    {
        done.warnings.push(json!({ "kind": "ObserveFailed", "repo": repo.as_str(), "code": e.code.as_str(), "message": e.message }));
    }
    Ok(done)
}

// ---- push: the headless replay of a Diverged chain ----------------------

/// The upper bound on the replayed range a headless `push` takes (and on
/// the commits it maps for the session rebase). A longer range is left to
/// the human (`chain`).
const REPLAY_LIMIT: usize = 512;

/// What a headless `push` replayed (publish.md D2a): the old chain `from`
/// re-applied as `to` on origin's `onto`; `map` pairs every replayed old
/// commit with its copy, oldest first. Set only once the chain-and-anchor
/// transaction committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replayed {
    pub from: CommitOid,
    pub to: CommitOid,
    pub onto: CommitOid,
    pub map: Vec<(CommitOid, CommitOid)>,
    pub checks_ran: u32,
}

impl Replayed {
    pub fn json(&self) -> Value {
        json!({
            "from": self.from.as_str(),
            "to": self.to.as_str(),
            "onto": self.onto.as_str(),
            "commits": self.map.len(),
            "checks_ran": self.checks_ran,
        })
    }
}

/// `exclusive..tip` along first parents, oldest first, at most
/// [`REPLAY_LIMIT`].
fn range_oldest_first(git: &Git, exclusive: &CommitOid, tip: &CommitOid) -> Result<Vec<CommitOid>> {
    let (mut v, more) = git.commits_between(exclusive, tip, REPLAY_LIMIT)?;
    if more {
        return Err(StrataError::new(Code::InvalidInput, "the unpushed chain is too long to replay headless; a human runs `git-strata chain`")
            .with_detail(json!({ "reason": "replay_too_long", "limit": REPLAY_LIMIT })));
    }
    v.reverse();
    Ok(v.into_iter().map(|(c, _)| c).collect())
}

/// The repo's own sealed deps set for `wt` (by its inputs) cloned into
/// it, as publish's staging does (review H3), so the checks find `vendor`
/// / `node_modules`. A dir the tree already tracks is kept. No install
/// runs here: a set that is not sealed is `InvalidInput`
/// (`replay_deps_unsealed`), nothing written.
fn clone_deps_for_checks(core: &Core, git: &Git, rc: &strata_types::RepoConfig, repo: &RepoName, wt: &Path) -> Result<()> {
    if rc.deps_dirs.is_empty() || rc.checks.is_empty() {
        return Ok(());
    }
    let ctx = strata_deps::Ctx { store: &core.store, paths: &core.config.paths, repo, cfg: rc, git, env: &core.env, locks: &core.locks };
    let (_, id) = strata_deps::key_of(&ctx, wt)?;
    let Some(mut set) = strata_deps::find_set(&core.store, repo, &id)? else {
        return Err(StrataError::new(
            Code::InvalidInput,
            "the replayed tree needs a deps set that is not built yet; nothing was pushed (a human runs `git-strata chain`, or a session on the new origin builds it first)",
        )
        .with_detail(json!({ "reason": "replay_deps_unsealed", "repo": repo.as_str(), "set": id.as_str() })));
    };
    set.dirs.retain(|d| fs::symlink_metadata(wt.join(d.as_str())).is_err());
    if set.dirs.is_empty() {
        return Ok(());
    }
    strata_deps::clone_into(&core.store, &set, wt)
}

fn replay_conflict(mut e: StrataError, repo: &RepoName, branch: &str, chain: &PublishChain, c: &CommitOid) -> StrataError {
    if e.code != Code::Conflict {
        return e;
    }
    let d = e.detail.get_or_insert_with(|| json!({}));
    if let Some(m) = d.as_object_mut() {
        m.insert("reason".into(), json!("replay_conflict"));
        m.insert("repo".into(), json!(repo.as_str()));
        m.insert("branch".into(), json!(branch));
        m.insert("chain".into(), json!(c.as_str()));
        m.insert("origin".into(), json!(chain.origin.as_str()));
        m.insert("anchor".into(), json!(chain.anchor.as_ref().map(|a| a.as_str())));
        m.insert("pushed".into(), json!(false));
    }
    e.message = format!(
        "{}; the unpushed chain does not replay cleanly onto origin/{branch}: nothing was written or pushed (a human resolves it with `git-strata chain`)",
        e.message
    );
    e
}

/// §5.5 `push` of a repo that needs no confirmation (publish.md D2a
/// [实现时补的决定], 2026-10-26). The caller holds the repo lock.
///
/// Fetch first. `Leads`: push as before. `Diverged` (origin gained
/// commits the chain lacks; the anchor is still on origin, so nothing a
/// human removed comes back): replay the unpushed range (`replay_base`, as
/// `chain --accept-origin replay`) onto origin's tip, objects only; a
/// piece that does not apply is `Conflict` (`replay_conflict`) with every
/// ref unchanged. Then, in the staging layout of the new tip, the repo's
/// checks run (its sealed deps set cloned in); a failing check is
/// `CheckFailed`, refs unchanged. Then one transaction moves the chain
/// (expecting the old top) and the anchor (to origin), and the recording
/// push sends the new tip once: a fast-forward of origin to the recorded
/// URL, never forced, never retried. `Rewound` is refused as publish
/// refuses it (`chain_rewound`): that stays the human's. `*replayed` is set
/// once the transaction committed, whatever the push did.
pub(crate) fn push_replaying(
    core: &Core,
    git: &Git,
    rc: &strata_types::RepoConfig,
    repo: &RepoName,
    replayed: &mut Option<Replayed>,
) -> Result<CommitOid> {
    let branch = rc.publish.branch.as_str();
    git.fetch("origin", branch)?;
    let chain = strata_deps::publish_chain(git, branch)?;
    let mut plan = None;
    let tip = match &chain {
        Some(ch) if ch.state == ChainState::Leads && ch.top.is_some() => ch.top.clone().expect("leads has a top"),
        Some(ch) if ch.state == ChainState::Diverged && ch.top.is_some() => {
            let c = ch.top.clone().expect("diverged has a top");
            let Some(base) = replay_base(git, ch, &c)? else {
                strata_publish::chain::refuse_unpublishable(git, branch)?;
                return Err(invalid("no_replay_range", "no replay range: the chain has no anchor on it"));
            };
            let old = range_oldest_first(git, &base, &c)?;
            // A Conflict here comes before any ref write: the refs are unchanged.
            let new = git.replay_commits(&base, &c, &ch.origin).map_err(|e| replay_conflict(e, repo, branch, ch, &c))?;
            plan = Some((c, ch.origin.clone(), ch.anchor.clone(), old, new.clone()));
            new
        }
        Some(ch) if ch.state == ChainState::Rewound => {
            strata_publish::chain::refuse_unpublishable(git, branch)?;
            return Err(invalid("chain_rewound", format!("the publish chain of {branch} is rewound")));
        }
        c => {
            return Err(StrataError::new(Code::NoCommit, format!("the publish chain of {repo} has no commit to push")).with_detail(json!({
                "repo": repo.as_str(),
                "branch": branch,
                "state": c.as_ref().map(|c| format!("{:?}", c.state)),
            })));
        }
    };
    let (chain_name, anchor_name) = (chain_ref(branch), anchor_ref(branch));
    // Review M6: an unknown push keeps its scene; a transaction that did not
    // commit sent nothing, so it does not.
    let uncertain = |r: &Result<PushOutcome>| {
        r.as_ref().is_err_and(|e| e.code == Code::Unknown && reason_of(e) != Some("ref_transaction"))
    };
    let dest = push_dest(git);
    let r = with_staging(core, git, repo, branch, &tip, dest.as_ref(), uncertain, |wt| {
        if let Some((c, o, a, old, new)) = &plan {
            clone_deps_for_checks(core, git, rc, repo, wt)?;
            let ran = strata_publish::check(rc, wt)?;
            let copies = range_oldest_first(git, o, new)?;
            if copies.len() != old.len() {
                return Err(invalid("replay_mismatch", "the replayed range does not match the chain; nothing was written"));
            }
            transaction(git, &[RefUpdate::set(&chain_name, new.clone(), Some(c.clone())), RefUpdate::set(&anchor_name, o.clone(), a.clone())])?;
            *replayed = Some(Replayed {
                from: c.clone(),
                to: new.clone(),
                onto: o.clone(),
                map: old.iter().cloned().zip(copies).collect(),
                checks_ran: ran.ran,
            });
        }
        crate::handlers::writing();
        strata_publish::push_to(git, rc, repo, wt, None, dest.as_ref().map(PushDest::url))
    });
    if matches!(r, Ok(PushOutcome::Pushed)) {
        settle_push_scenes(core, git, repo, branch, &tip, dest.as_ref());
    }
    r.map(|_| tip)
}

fn reason_of(e: &StrataError) -> Option<&str> {
    e.detail.as_ref().and_then(|d| d.get("reason")).and_then(Value::as_str)
}
