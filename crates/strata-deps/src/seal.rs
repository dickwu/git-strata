//! §5.3: the build lock, attempts, validation, sync and sealing shared by
//! `adopt` and `build`.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use strata_apfs::{FileKind, FileMeta, RenameMode};
use strata_store::{AttemptKind, AttemptState, DepsOrigin};
use strata_types::{Code, DepsSet, DepsSetId, RepoPath, StrataError};

use crate::layout::{Layout, Staging, parent, reason, rp, unknown};
use crate::seams::{self, Sync};
use crate::{Ctx, Result};

/// A held build lock; dropping it closes the file and releases the flock.
pub(crate) struct BuildLock {
    _file: File,
}

const POLL_START: Duration = Duration::from_millis(50);
const POLL_MAX: Duration = Duration::from_secs(1);

/// §5.3 step 2: open (or create) `<repo>/.locks/<id>.lock` rooted at
/// `<root>/deps` and take `flock(LOCK_EX)` by polling, bounded by
/// `ctx.env.timeout`.
pub(crate) fn lock_build(ctx: &Ctx, lay: &Layout, id: &DepsSetId) -> Result<BuildLock> {
    let locks = lay.locks_rel();
    match strata_apfs::mkdir_rooted(&lay.deps, &locks, 0o755, true) {
        Ok(()) => {}
        Err(e) if e.code == Code::Exists => {
            let path = lay.deps.join(locks.as_str());
            return Err(StrataError::new(
                Code::NotDirectory,
                format!("{} is not a directory", path.display()),
            )
            .with_detail(json!({ "path": path })));
        }
        Err(e) => return Err(e),
    }
    let rel = lay.lock_rel(id);
    let file = match strata_apfs::open_rooted_file(&lay.deps, &rel) {
        Ok(f) => f,
        Err(e) if e.code == Code::NoSuchPath => {
            match strata_apfs::create_file_rooted(&lay.deps, &rel, 0o644) {
                Ok(f) => f,
                Err(e) if e.code == Code::Exists => strata_apfs::open_rooted_file(&lay.deps, &rel)?,
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
    };
    let lock_path = lay.deps.join(rel.as_str());
    let start = Instant::now();
    let mut interval = POLL_START;
    loop {
        // SAFETY: flock on a descriptor we own.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(BuildLock { _file: file });
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) => {}
            _ => {
                return Err(strata_apfs::classify_io_error(
                    &err,
                    strata_apfs::IoSide::Write,
                    &lock_path,
                ));
            }
        }
        let waited = start.elapsed();
        if waited >= ctx.env.timeout {
            return Err(StrataError::new(Code::Busy, format!("build lock {} is held", lock_path.display())).with_detail(
                json!({ "reason": "build_lock_busy", "lock": lock_path, "waited": waited.as_millis() as u64 }),
            ));
        }
        std::thread::sleep(interval.min(ctx.env.timeout - waited));
        interval = (interval * 2).min(POLL_MAX);
    }
}

fn interrupted(a: u64) -> StrataError {
    unknown(
        "interrupted",
        format!("deps attempt {a} was interrupted"),
        json!({ "attempt": a }),
    )
}

/// §5.3 step 3, under the build lock. `Some(set)`: already sealed.
pub(crate) fn after_lock(
    ctx: &Ctx,
    lay: &Layout,
    id: &DepsSetId,
    kind: AttemptKind,
) -> Result<Option<DepsSet>> {
    if let Some(rec) = ctx.store.deps_set(ctx.repo, id)? {
        return Ok(Some(rec.set));
    }
    for a in ctx.store.deps_attempts(Some(ctx.repo))? {
        if a.set == *id && a.state == AttemptState::Running {
            ctx.store.end_deps_attempt(
                a.id,
                AttemptState::Failed,
                &interrupted(a.id),
                SystemTime::now(),
            )?;
        }
    }
    let set_rel = rp(format!("{}/{}", ctx.repo.as_str(), id.as_str()));
    if exists(&lay.deps, &set_rel)? {
        let mut found: Option<DepsSet> = None;
        let mut f = || -> Result<()> {
            if let Some(rec) = ctx.store.deps_set(ctx.repo, id)? {
                found = Some(rec.set);
                return Ok(());
            }
            if exists(&lay.deps, &set_rel)? {
                let path = ctx.store.deps_dir(ctx.repo, id);
                return Err(orphan(&path));
            }
            Ok(())
        };
        ctx.locks.with_write(ctx.repo, &mut f)??;
        if let Some(s) = found {
            return Ok(Some(s));
        }
    }
    if kind == AttemptKind::Build {
        let builds: Vec<_> = ctx
            .store
            .deps_attempts(Some(ctx.repo))?
            .into_iter()
            .filter(|a| {
                a.set == *id && a.kind == AttemptKind::Build && a.state != AttemptState::Refused
            })
            .collect();
        if let Some(last) = builds.last()
            && last.state == AttemptState::Failed
            && std::fs::symlink_metadata(&last.staging).is_ok()
        {
            let was_interrupted = |a: &strata_store::DepsAttempt| {
                a.state == AttemptState::Failed
                    && a.error.as_ref().and_then(reason) == Some("interrupted")
            };
            let block = if !was_interrupted(last) {
                true
            } else {
                builds.len() >= 2 && was_interrupted(&builds[builds.len() - 2])
            };
            if block {
                return Err(unknown(
                    "previous_attempt",
                    format!(
                        "deps attempt {} failed; its staging {} is kept for inspection",
                        last.id,
                        last.staging.display()
                    ),
                    json!({ "attempt": last.id, "staging": last.staging, "error": last.error }),
                ));
            }
        }
    }
    Ok(None)
}

pub(crate) fn orphan(path: &Path) -> StrataError {
    unknown(
        "orphan",
        format!("{} exists without a deps set record", path.display()),
        json!({ "path": path }),
    )
}

/// Whether `rel` exists under `root` (any kind, no symlink followed).
pub(crate) fn exists(root: &Path, rel: &RepoPath) -> Result<bool> {
    match strata_apfs::attrs_of(root, rel) {
        Ok(_) => Ok(true),
        Err(e) if e.code == Code::NoSuchPath => Ok(false),
        Err(e) => Err(e),
    }
}

/// §5.3 step 4: begin the attempt and create its staging directory.
pub(crate) fn begin(ctx: &Ctx, lay: &Layout, id: &DepsSetId, kind: AttemptKind) -> Result<Staging> {
    let a = ctx
        .store
        .begin_deps_attempt(ctx.repo, id, kind, SystemTime::now())?;
    let st = Staging::new(a, ctx.store.deps_staging_dir(ctx.repo, a));
    if let Err(e) = strata_apfs::mkdir_rooted(&lay.repo_dir, &Layout::staging_rel(a), 0o755, true) {
        return Err(refuse(ctx, &st, e));
    }
    seams::after_attempt_begins();
    Ok(st)
}

/// A named refusal before install: retire the staging, then close the
/// attempt as refused. Returns `err`.
pub(crate) fn refuse(ctx: &Ctx, st: &Staging, err: StrataError) -> StrataError {
    let now = SystemTime::now();
    let retired = match std::fs::symlink_metadata(&st.dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        _ => ctx.store.retire_dir(&st.dir).is_ok(),
    };
    // Decision: a staging that could not be retired is kept and the attempt
    // closed as failed, so the scene is never left behind a refused record
    // (gc does not enter `.staging/`).
    let to = if retired {
        AttemptState::Refused
    } else {
        AttemptState::Failed
    };
    let _ = ctx.store.end_deps_attempt(st.a, to, &err, now);
    err
}

/// A failure after install started: close as failed, keep the scene.
pub(crate) fn fail(ctx: &Ctx, st: &Staging, mut err: StrataError) -> StrataError {
    // Every failed attempt names its kept scene (decision: `attempt` and
    // `staging` are added to `Unknown` details that lack them).
    if err.code == Code::Unknown
        && let Some(serde_json::Value::Object(m)) = err.detail.as_mut()
    {
        m.entry("attempt")
            .or_insert_with(|| serde_json::json!(st.a));
        m.entry("staging")
            .or_insert_with(|| serde_json::json!(st.dir));
    }
    let _ = ctx
        .store
        .end_deps_attempt(st.a, AttemptState::Failed, &err, SystemTime::now());
    err
}

fn barrier(file: &File, path: &Path) -> Result<()> {
    loop {
        // SAFETY: fcntl on a descriptor we own.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) };
        if rc != -1 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(strata_apfs::classify_io_error(
            &err,
            strata_apfs::IoSide::AfterWrite,
            path,
        ));
    }
}

/// Whether a symlink at `at` (repo layout) with `target` resolves lexically
/// inside the repo root.
pub(crate) fn link_stays_inside(at: &RepoPath, target: &Path) -> bool {
    use std::path::Component;
    let mut depth: i64 = at.as_str().matches('/').count() as i64;
    for c in target.components() {
        match c {
            Component::RootDir | Component::Prefix(_) => return false,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::Normal(_) => depth += 1,
        }
    }
    true
}

/// §5.3 step 5: one walk of the seal candidate `seal` (an absolute path
/// whose layout mirrors the repo) that validates every entry and puts an
/// `F_BARRIERFSYNC` on every regular file and directory. A violation is
/// `Unknown` with `detail.reason` special_file, unreadable_file or
/// symlink_escapes and `detail.path` (relative to the seal).
pub(crate) fn validate_and_sync(seal: &Path) -> Result<()> {
    let all = |_: &Path| true;
    for item in strata_apfs::walk(seal, &all) {
        let (path, attrs) = item?;
        match &attrs.kind {
            FileKind::Dir => {
                let f = strata_apfs::open_rooted(seal, &path)?;
                barrier(&f, &seal.join(path.as_str()))?;
                seams::count_sync(Sync::BarrierDir);
            }
            FileKind::File => {
                let unreadable = |detail: Option<StrataError>| {
                    unknown(
                        "unreadable_file",
                        format!("{} is not readable by its owner", path.as_str()),
                        json!({ "path": path.as_str(), "error": detail }),
                    )
                };
                if attrs.mode & 0o400 == 0 {
                    return Err(unreadable(None));
                }
                let f = match strata_apfs::open_rooted(seal, &path) {
                    Ok(f) => f,
                    Err(e) => return Err(unreadable(Some(e))),
                };
                barrier(&f, &seal.join(path.as_str()))?;
                seams::count_sync(Sync::BarrierFile);
            }
            FileKind::Symlink { target } => {
                if !link_stays_inside(&path, target) {
                    return Err(unknown(
                        "symlink_escapes",
                        format!(
                            "symlink {} -> {} leaves the repository",
                            path.as_str(),
                            target.display()
                        ),
                        json!({ "path": path.as_str(), "target": target }),
                    ));
                }
            }
            _ => {
                return Err(unknown(
                    "special_file",
                    format!("{} is a special file", path.as_str()),
                    json!({ "path": path.as_str() }),
                ));
            }
        }
    }
    Ok(())
}

/// §5.3 steps 6–7: rename `.staging/<a>/seal` to `<id>`, set the root to
/// 0555, full-sync the repo dir and record the set, all under the repo write
/// lock; then best-effort retire what is left of the staging.
///
/// `Busy` from the repo write lock: an adoption refuses (staging retired,
/// attempt refused) and returns `Busy`; a build retries until it gets the
/// lock.
pub(crate) fn seal(
    ctx: &Ctx,
    lay: &Layout,
    st: &Staging,
    set: &DepsSet,
    kind: AttemptKind,
) -> Result<DepsSet> {
    let id = &set.id;
    let origin = match kind {
        AttemptKind::Adopt => DepsOrigin::Adopt,
        AttemptKind::Build => DepsOrigin::Build,
    };
    let from = rp(format!(".staging/{}/seal", st.a));
    let to = lay.set_rel(id);
    let set_dir = ctx.store.deps_dir(ctx.repo, id);
    let mut f = || -> Result<()> {
        match strata_apfs::rename_rooted(&lay.repo_dir, &from, &to, RenameMode::Excl) {
            Ok(()) => {}
            Err(e) if e.code == Code::Exists => return Err(orphan(&set_dir)),
            Err(e) => {
                return Err(unknown(
                    "seal_failed",
                    format!("could not move the seal candidate to {}", set_dir.display()),
                    json!({ "path": set_dir, "attempt": st.a, "error": e }),
                ));
            }
        }
        let after = || -> Result<()> {
            let root = strata_apfs::open_rooted(&lay.repo_dir, &to)?;
            strata_apfs::set_file_meta(
                &root,
                &FileMeta {
                    clear_flags: false,
                    mode: Some(0o555),
                    mtime: None,
                },
            )?;
            let dir = strata_apfs::open_root(&lay.repo_dir)?;
            strata_apfs::full_fsync(&dir)?;
            seams::count_sync(Sync::Full);
            ctx.store
                .record_deps_set(set, origin, Some(st.a), SystemTime::now())
        };
        after().map_err(|e| {
            unknown(
                "seal_failed",
                format!("sealed {} but could not record it", set_dir.display()),
                json!({ "path": set_dir, "attempt": st.a, "error": e }),
            )
        })
    };
    let result = loop {
        match ctx.locks.with_write(ctx.repo, &mut f) {
            Ok(r) => break r,
            Err(busy) => match kind {
                AttemptKind::Adopt => return Err(refuse(ctx, st, busy)),
                AttemptKind::Build => std::thread::sleep(Duration::from_millis(10)),
            },
        }
    };
    if let Err(e) = result {
        return Err(fail(ctx, st, e));
    }
    let _ = ctx.store.retire_dir(&st.dir);
    Ok(set.clone())
}

/// The parent of `p` created under `root` (if `p` has one).
pub(crate) fn mkdir_parent(root: &Path, p: &RepoPath) -> Result<()> {
    match parent(p) {
        Some(par) => strata_apfs::mkdir_rooted(root, &par, 0o755, true),
        None => Ok(()),
    }
}

/// §5.6 step 1: the empty deps set (no `deps_dirs`), created and recorded
/// under the repo write lock. No build lock, attempt or staging.
pub(crate) fn seal_empty(ctx: &Ctx) -> Result<DepsSet> {
    let h = crate::empty_inputs_hash();
    let id = crate::set_id(&h, &crate::platform(), "", &crate::DepsConfigKey::empty());
    if let Some(rec) = ctx.store.deps_set(ctx.repo, &id)? {
        return Ok(rec.set);
    }
    let lay = Layout::of(ctx);
    let set = DepsSet {
        id: id.clone(),
        repo: ctx.repo.clone(),
        inputs_hash: h,
        platform: crate::platform(),
        toolchain: String::new(),
        dirs: Vec::new(),
    };
    let set_dir = ctx.store.deps_dir(ctx.repo, &id);
    let rel = rp(format!("{}/{}", ctx.repo.as_str(), id.as_str()));
    let mut found: Option<DepsSet> = None;
    let mut f = || -> Result<()> {
        if let Some(rec) = ctx.store.deps_set(ctx.repo, &id)? {
            found = Some(rec.set);
            return Ok(());
        }
        match strata_apfs::attrs_of(&lay.deps, &rel) {
            Err(e) if matches!(e.code, Code::NoSuchPath) => {
                strata_apfs::mkdir_rooted(&lay.deps, &rel, 0o755, true)?
            }
            Err(e) => return Err(e),
            Ok(a) if a.kind == FileKind::Dir => {
                let mut entries = strata_apfs::list_dir(&lay.deps, Some(&rel));
                if entries.next().is_some() {
                    return Err(orphan(&set_dir));
                }
            }
            Ok(_) => return Err(orphan(&set_dir)),
        }
        let after = || -> Result<()> {
            let root = strata_apfs::open_rooted(&lay.repo_dir, &lay.set_rel(&id))?;
            strata_apfs::set_file_meta(
                &root,
                &FileMeta {
                    clear_flags: false,
                    mode: Some(0o555),
                    mtime: None,
                },
            )?;
            let dir = strata_apfs::open_root(&lay.repo_dir)?;
            strata_apfs::full_fsync(&dir)?;
            seams::count_sync(Sync::Full);
            ctx.store
                .record_deps_set(&set, DepsOrigin::Empty, None, SystemTime::now())
        };
        after().map_err(|e| {
            unknown(
                "seal_failed",
                format!("created {} but could not record it", set_dir.display()),
                json!({ "path": set_dir, "error": e }),
            )
        })
    };
    ctx.locks.with_write(ctx.repo, &mut f)??;
    Ok(found.unwrap_or(set))
}
