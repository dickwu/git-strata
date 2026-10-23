//! §5.5 `build` (cold install), §5.7 `clone_into`, `key_of` and `find_set`.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use strata_apfs::{CloneKind, FileKind, RenameMode};
use strata_store::{AttemptKind, Store};
use strata_types::{Code, DepsSet, DepsSetId, RepoName, RepoPath, StrataError};

use crate::inputs::{Member, input_members, resolve_program, sanitize};
use crate::layout::{Layout, Staging, join, reason, rp, unknown};
use crate::seal::{self, mkdir_parent};
use crate::{
    Ctx, DepsConfigKey, Result, empty_inputs_hash, inputs_hash, platform, set_id, toolchain,
};

/// The (inputs_hash, set id) of `tree`. With no `deps_dirs` this is the
/// empty set's key (§5.6 step 1).
pub fn key_of(ctx: &Ctx, tree: &Path) -> Result<(String, DepsSetId)> {
    if ctx.cfg.deps_dirs.is_empty() {
        let h = empty_inputs_hash();
        let id = set_id(&h, &platform(), "", &DepsConfigKey::empty());
        return Ok((h, id));
    }
    let config = DepsConfigKey::of(ctx.cfg)?;
    let tc = toolchain(ctx.cfg, ctx.env)?;
    let h = inputs_hash(ctx.cfg, ctx.git, tree)?;
    let id = set_id(&h, &platform(), &tc, &config);
    Ok((h, id))
}

/// A recorded deps set, `None` if there is no record.
pub fn find_set(store: &Store, repo: &RepoName, id: &DepsSetId) -> Result<Option<DepsSet>> {
    Ok(store.deps_set(repo, id)?.map(|r| r.set))
}

/// Every `install_inputs` entry exists in `tree` (rule tracked_in_base).
pub(crate) fn inputs_tracked(ctx: &Ctx, tree: &Path) -> Result<()> {
    for (index, e) in ctx.cfg.install_inputs.iter().enumerate() {
        match strata_apfs::attrs_of(tree, e) {
            Ok(_) => {}
            Err(err) if matches!(err.code, Code::NoSuchPath | Code::NotDirectory) => {
                return Err(StrataError::new(
                    Code::InvalidInput,
                    format!(
                        "install input {} of repo {} is not in the base",
                        e.as_str(),
                        ctx.repo.as_str()
                    ),
                )
                .with_detail(json!({
                    "repo": ctx.repo.as_str(),
                    "field": "install_inputs",
                    "index": index,
                    "rule": "tracked_in_base",
                    "path": e.as_str(),
                })));
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// `same_volume(nearest_existing_ancestor(Paths::deps()), other)`.
pub(crate) fn check_volume(ctx: &Ctx, other: &Path) -> Result<()> {
    let deps = ctx.paths.deps();
    let read = |e: std::io::Error, p: &Path| {
        strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, p)
    };
    let anc = strata_apfs::nearest_existing_ancestor(&deps).map_err(|e| read(e, &deps))?;
    if !strata_apfs::same_volume(&anc, other).map_err(|e| read(e, other))? {
        return Err(StrataError::new(
            Code::CrossVolume,
            format!(
                "{} and {} are on different volumes",
                deps.display(),
                other.display()
            ),
        )
        .with_detail(json!({ "a": deps, "b": other })));
    }
    Ok(())
}

/// §5.5: a cold install of `tree`'s install inputs (taken from
/// `inputs_from` when given) sealed as a new generation.
pub fn build(ctx: &Ctx, tree: &Path, inputs_from: Option<&Path>) -> Result<DepsSet> {
    if ctx.cfg.deps_dirs.is_empty() {
        return seal::seal_empty(ctx);
    }
    // 2. Preflight; nothing written.
    let config = DepsConfigKey::of(ctx.cfg)?;
    let tc = toolchain(ctx.cfg, ctx.env)?;
    if config.install.is_empty() {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!("repo {} has deps_dirs but no install", ctx.repo.as_str()),
        )
        .with_detail(
            json!({ "repo": ctx.repo.as_str(), "field": "install", "rule": "required" }),
        ));
    }
    let src = match inputs_from {
        Some(p) => p,
        None => {
            inputs_tracked(ctx, tree)?;
            tree
        }
    };
    let h = inputs_hash(ctx.cfg, ctx.git, src)?;
    let plat = platform();
    let id = set_id(&h, &plat, &tc, &config);
    check_volume(ctx, tree)?;
    let program = resolve_program(&config.install[0], &ctx.env.path).ok_or_else(|| {
        StrataError::new(
            Code::NoSuchPath,
            format!("install program {} not found on PATH", config.install[0]),
        )
        .with_detail(json!({ "program": config.install[0], "path": ctx.env.path }))
    })?;

    // 3. Build lock and the checks after it.
    let lay = Layout::of(ctx);
    let _lock = seal::lock_build(ctx, &lay, &id)?;
    if let Some(set) = seal::after_lock(ctx, &lay, &id, AttemptKind::Build)? {
        return Ok(set);
    }

    // 4. Attempt.
    let mut st = seal::begin(ctx, &lay, &id, AttemptKind::Build)?;

    // 5–6. Prepare S; named refusals.
    if let Err(e) = prepare(ctx, &mut st, tree, inputs_from, &config) {
        return Err(seal::refuse(ctx, &st, e));
    }
    let s = st.dir.join("tree");
    match inputs_hash(ctx.cfg, ctx.git, &s) {
        Ok(found) if found == h => {}
        Ok(found) => {
            let e = StrataError::new(Code::Drift, "install inputs changed while they were copied")
                .with_detail(json!({ "reason": "inputs_changed", "expected": h, "found": found }));
            return Err(seal::refuse(ctx, &st, e));
        }
        Err(e) => return Err(seal::refuse(ctx, &st, e)),
    }

    // 7–8. Install. From here on failures keep the scene.
    run_install(ctx, &st, &program, &config.install)?;

    // 9. Install must not have changed its inputs.
    match inputs_hash(ctx.cfg, ctx.git, &s) {
        Ok(after) if after == h => {}
        Ok(after) => {
            let e = unknown(
                "inputs_modified",
                "install modified its install inputs",
                json!({ "attempt": st.a, "staging": st.dir, "expected": h, "found": after }),
            );
            return Err(seal::fail(ctx, &st, e));
        }
        Err(e) => return Err(seal::fail(ctx, &st, after_install(e, &st))),
    }

    // 10–11. Assemble, validate and sync.
    let dirs = match assemble(&mut st, &config) {
        Ok(d) => d,
        Err(e) => return Err(seal::fail(ctx, &st, after_install(e, &st))),
    };
    if let Err(e) = seal::validate_and_sync(&st.dir.join("seal")) {
        return Err(seal::fail(ctx, &st, after_install(e, &st)));
    }

    // 12. Seal.
    let set = DepsSet {
        id,
        repo: ctx.repo.clone(),
        inputs_hash: h,
        platform: plat,
        toolchain: tc,
        dirs,
    };
    seal::seal(ctx, &lay, &st, &set, AttemptKind::Build)
}

/// Any failure after install is `Unknown`; one that is not already gets
/// reason `staging_failed` with the original error in the detail.
fn after_install(e: StrataError, st: &Staging) -> StrataError {
    if e.code == Code::Unknown && reason(&e).is_some() {
        return e;
    }
    unknown(
        "staging_failed",
        format!("deps attempt {} failed after install: {}", st.a, e.message),
        json!({ "attempt": st.a, "staging": st.dir, "error": e }),
    )
}

fn tree_rel(p: &RepoPath) -> RepoPath {
    join("tree", p.as_str())
}

/// §5.5 step 5.
fn prepare(
    ctx: &Ctx,
    st: &mut Staging,
    tree: &Path,
    inputs_from: Option<&Path>,
    config: &DepsConfigKey,
) -> Result<()> {
    let root = st.dir.clone();
    strata_apfs::clone_tree_rooted(tree, &root, &rp("tree"))?;
    let mut away: Vec<&RepoPath> = config
        .deps_dirs
        .iter()
        .chain(ctx.cfg.seed_caches.iter())
        .collect();
    away.sort_by(|a, b| a.as_str().as_bytes().cmp(b.as_str().as_bytes()));
    for d in away {
        if seal::exists(&root, &tree_rel(d))? {
            st.discard(&tree_rel(d))?;
        }
    }
    let Some(from) = inputs_from else {
        return Ok(());
    };
    let members = input_members(ctx.cfg, ctx.git, from)?;
    for e in &ctx.cfg.install_inputs {
        if seal::exists(&root, &tree_rel(e))? {
            st.discard(&tree_rel(e))?;
        }
    }
    for (m, kind) in members {
        let dst = tree_rel(&m);
        mkdir_parent(&root, &dst)?;
        match kind {
            Member::File => {
                let slot = st.input_slot()?;
                strata_apfs::clone_from_rooted(
                    from,
                    &m,
                    &root.join(slot.as_str()),
                    CloneKind::File,
                )?;
                strata_apfs::rename_rooted(&root, &slot, &dst, RenameMode::Excl)?;
            }
            Member::Link(target) => strata_apfs::symlink_rooted(&root, &dst, target.as_os_str())?,
        }
    }
    Ok(())
}

const LOG_TAIL: u64 = 8 * 1024;
const TERM_GRACE: Duration = Duration::from_secs(10);

fn log_tail(st: &Staging) -> String {
    let Ok(mut f) = strata_apfs::open_rooted_file(&st.dir, &rp("install.log")) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(LOG_TAIL)));
    let mut b = Vec::new();
    let _ = f.read_to_end(&mut b);
    String::from_utf8_lossy(&b).into_owned()
}

fn group_gone(pgid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    let rc = unsafe { libc::killpg(pgid, 0) };
    rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Wait until the group is empty (reaping the leader) or `limit` passes.
fn wait_group(child: &mut std::process::Child, pgid: i32, limit: Duration) -> bool {
    let start = Instant::now();
    loop {
        let _ = child.try_wait();
        if group_gone(pgid) {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// §5.5 steps 7–8.
fn run_install(ctx: &Ctx, st: &Staging, program: &Path, argv: &[String]) -> Result<()> {
    let failure = |reason: &str, message: String, exit_code: Option<i32>| {
        let e = unknown(
            reason,
            message,
            json!({
                "attempt": st.a,
                "staging": st.dir,
                "command": argv,
                "exit_code": exit_code,
                "log_tail": log_tail(st),
            }),
        );
        seal::fail(ctx, st, e)
    };
    let log = match strata_apfs::create_file_rooted(&st.dir, &rp("install.log"), 0o644) {
        Ok(f) => f,
        Err(e) => {
            return Err(failure(
                "install_failed",
                format!("cannot create install.log: {}", e.message),
                None,
            ));
        }
    };
    let log2 = match log.try_clone() {
        Ok(f) => f,
        Err(e) => {
            return Err(failure(
                "install_failed",
                format!("cannot duplicate install.log: {e}"),
                None,
            ));
        }
    };
    let mut cmd = Command::new(program);
    cmd.args(&argv[1..])
        .current_dir(st.dir.join("tree"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        .process_group(0);
    sanitize(&mut cmd, ctx.env);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Err(failure(
                "install_failed",
                format!("cannot start {}: {e}", program.display()),
                None,
            ));
        }
    };
    drop(cmd);
    let pid = child.id() as i32;
    let _ = ctx.store.set_deps_attempt_pid(st.a, pid);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(failure(
                    "install_failed",
                    format!("{} exited with {status}", argv.join(" ")),
                    status.code(),
                ));
            }
            Ok(None) if start.elapsed() >= ctx.env.timeout => {
                // SAFETY: signalling the install's own process group.
                unsafe { libc::killpg(pid, libc::SIGTERM) };
                if !wait_group(&mut child, pid, TERM_GRACE) {
                    // SAFETY: as above.
                    unsafe { libc::killpg(pid, libc::SIGKILL) };
                    wait_group(&mut child, pid, Duration::from_secs(5));
                }
                let code = child.wait().ok().and_then(|s| s.code());
                return Err(failure(
                    "timeout",
                    format!(
                        "{} did not finish within {:?}",
                        argv.join(" "),
                        ctx.env.timeout
                    ),
                    code,
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                return Err(failure(
                    "install_failed",
                    format!("waiting for install: {e}"),
                    None,
                ));
            }
        }
    }
}

/// §5.5 step 10.
fn assemble(st: &mut Staging, config: &DepsConfigKey) -> Result<Vec<RepoPath>> {
    let root = st.dir.clone();
    strata_apfs::mkdir_rooted(&root, &rp("seal"), 0o755, false)?;
    let mut dirs = Vec::new();
    for d in &config.deps_dirs {
        let not_dir = || {
            unknown(
                "not_directory",
                format!(
                    "install produced {} as something other than a directory",
                    d.as_str()
                ),
                json!({ "attempt": st.a, "staging": st.dir, "path": d.as_str() }),
            )
        };
        match strata_apfs::attrs_of(&root, &tree_rel(d)) {
            Err(e) if e.code == Code::NoSuchPath => continue,
            Err(e) if e.code == Code::NotDirectory => return Err(not_dir()),
            Err(e) => return Err(e),
            Ok(a) if a.kind != FileKind::Dir => return Err(not_dir()),
            Ok(_) => {}
        }
        let dst = join("seal", d.as_str());
        mkdir_parent(&root, &dst)?;
        strata_apfs::rename_rooted(&root, &tree_rel(d), &dst, RenameMode::Excl)?;
        dirs.push(d.clone());
    }
    if dirs.is_empty() {
        return Err(unknown(
            "no_deps_dirs",
            "install produced none of the deps dirs",
            json!({ "attempt": st.a, "staging": st.dir, "deps_dirs": config.deps_dirs }),
        ));
    }
    for c in &config.nested_seed_caches {
        let rel = join("seal", c.as_str());
        if seal::exists(&root, &rel)? {
            st.discard(&rel)?;
        }
    }
    Ok(dirs)
}

/// §5.7: clone every dir of `set` into `dst_root`.
pub fn clone_into(store: &Store, set: &DepsSet, dst_root: &Path) -> Result<()> {
    let src = store.deps_dir(&set.repo, &set.id);
    let corrupt = |p: PathBuf| {
        StrataError::new(
            Code::Corrupt,
            format!("sealed deps dir {} is missing", p.display()),
        )
        .with_detail(json!({ "path": p, "set": set.id.as_str() }))
    };
    match std::fs::symlink_metadata(&src) {
        Ok(m) if m.is_dir() => {}
        _ => return Err(corrupt(src)),
    }
    for d in &set.dirs {
        match strata_apfs::attrs_of(&src, d) {
            Ok(a) if a.kind == FileKind::Dir => {}
            _ => return Err(corrupt(src.join(d.as_str()))),
        }
    }
    for d in &set.dirs {
        match strata_apfs::attrs_of(dst_root, d) {
            Ok(_) => {
                let p = dst_root.join(d.as_str());
                return Err(StrataError::new(
                    Code::Exists,
                    format!("{} already exists", p.display()),
                )
                .with_detail(json!({ "path": p })));
            }
            Err(e) if e.code == Code::NoSuchPath => {}
            Err(e) => return Err(e),
        }
    }
    for (i, d) in set.dirs.iter().enumerate() {
        let r = mkdir_parent(dst_root, d)
            .and_then(|()| strata_apfs::clone_tree_rooted(&src.join(d.as_str()), dst_root, d));
        if let Err(e) = r {
            if i == 0 {
                return Err(e);
            }
            return Err(unknown(
                "clone_failed",
                format!(
                    "cloning {} into {} failed after earlier dirs were cloned: {}",
                    d.as_str(),
                    dst_root.display(),
                    e.message
                ),
                json!({ "path": dst_root.join(d.as_str()), "error": e }),
            ));
        }
    }
    Ok(())
}
