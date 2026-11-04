//! Activity scope is persisted with each move; carried caches remain protected.
use crate::Result;
use serde::Serialize;
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use strata_store::ActivityScope;
use strata_types::{Code, RepoConfig, RepoPath, StrataError, WorkspaceState};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub t0_ns: i128,
    /// All carried entries; used by G4 and every old-tree check.
    pub m0_ns: i128,
    /// Quiet-window entries only; explicit quiet_skip does not weaken G4.
    pub m0_counted: i128,
    pub newest: PathBuf,
    pub entries: u64,
    pub late: Option<PathBuf>,
    pub root_names: Vec<String>,
    pub scope: ActivityScope,
}
fn ns(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}
pub(crate) fn now_ns() -> i64 {
    i64::try_from(ns(SystemTime::now())).unwrap_or(i64::MAX)
}
fn ctime_ns(md: &fs::Metadata) -> i128 {
    md.ctime() as i128 * 1_000_000_000 + md.ctime_nsec() as i128
}

fn key(path: &str, fold: bool) -> String {
    if fold {
        strata_apfs::case_fold_key(path)
    } else {
        path.to_owned()
    }
}
fn under(path: &str, root: &str, fold: bool) -> bool {
    let (path, root) = (key(path, fold), key(root, fold));
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|rest| rest.starts_with('/'))
}
fn quiet_rules(patterns: &[String], fold: bool) -> strata_overlay::LwwRules {
    strata_overlay::LwwRules::parse(
        &patterns
            .iter()
            .map(|p| key(p, fold))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}
fn quiet_matches(rules: &strata_overlay::LwwRules, path: &str, fold: bool) -> bool {
    RepoPath::try_from(key(path, fold)).is_ok_and(|p| strata_overlay::lww_matches(rules, &p))
}

/// Union configured and currently pinned dependency roots. Tracked protection
/// uses ws.head, never the main checkout index. Nested carried caches are extra roots.
pub fn activity_scope(
    cfg: &RepoConfig,
    git: &strata_gitio::Git,
    store: &strata_store::Store,
    ws: &WorkspaceState,
) -> Result<ActivityScope> {
    let tracked = git.tracked_paths(&ws.head)?;
    let fold = !strata_apfs::volume_caps(&ws.path)?.case_sensitive;
    let quiet = quiet_rules(&cfg.rebase.quiet_skip, fold);
    if let Some(path) = tracked
        .iter()
        .find(|p| quiet_matches(&quiet, p.as_str(), fold))
    {
        return Err(StrataError::new(
            Code::InvalidInput,
            format!("rebase.quiet_skip matches tracked path {path}"),
        )
        .with_detail(json!({"reason":"quiet_skip_tracked","path":path})));
    }
    let mut dirs = cfg.deps_dirs.clone();
    if let Some(set) = strata_deps::find_set(store, &ws.base.repo, &ws.base.deps)? {
        dirs.extend(set.dirs);
    }
    dirs.sort();
    dirs.dedup();
    let mut scope = ActivityScope {
        fold,
        ..Default::default()
    };
    for dir in dirs {
        if tracked
            .iter()
            .any(|p| under(p.as_str(), dir.as_str(), fold))
        {
            static WARNED: std::sync::OnceLock<
                std::sync::Mutex<std::collections::BTreeSet<String>>,
            > = std::sync::OnceLock::new();
            let key = format!(
                "{}:{}:{}",
                ws.base.repo,
                cfg.path.display(),
                key(dir.as_str(), fold)
            );
            if WARNED
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(key)
            {
                eprintln!(
                    "warning: deps_dir_tracked: {}/{} is checked for activity",
                    ws.base.repo, dir
                );
            }
        } else {
            scope.skip.push(dir.to_string());
        }
    }
    for cache in &cfg.seed_caches {
        if scope.skip.iter().any(|d| under(cache.as_str(), d, fold)) {
            scope.exceptions.push(cache.to_string());
        }
    }
    scope.skip.sort_by_key(|p| key(p, fold));
    scope.skip.dedup_by(|a, b| key(a, fold) == key(b, fold));
    scope.exceptions.sort_by_key(|p| key(p, fold));
    scope
        .exceptions
        .dedup_by(|a, b| key(a, fold) == key(b, fold));
    let exceptions = scope.exceptions.clone();
    scope.exceptions.retain(|e| {
        !exceptions
            .iter()
            .any(|other| e != other && under(e, other, fold))
    });
    Ok(scope)
}

/// Do not follow symlinks through the skipped tree to reach an exception.
fn exception_exists(root: &Path, relative: &str) -> Result<bool> {
    let mut path = root.to_path_buf();
    let mut comps = Path::new(relative).components().peekable();
    while let Some(c) = comps.next() {
        path.push(c);
        match fs::symlink_metadata(&path) {
            Ok(m) if comps.peek().is_some() && (!m.is_dir() || m.file_type().is_symlink()) => {
                return Ok(false);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(walk_error(&path, &e)),
        }
    }
    Ok(true)
}
fn walk_scope(
    root: &Path,
    scope: &ActivityScope,
    mut visit: impl FnMut(&Path, &fs::Metadata),
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    for e in &scope.exceptions {
        if exception_exists(root, e)? {
            let path = root.join(e);
            let md = fs::symlink_metadata(&path).map_err(|e| walk_error(&path, &e))?;
            names.push(e.clone());
            visit(&path, &md);
            if md.is_dir() {
                stack.push(path);
            }
        }
    }
    while let Some(dir) = stack.pop() {
        for ent in fs::read_dir(&dir).map_err(|e| walk_error(&dir, &e))? {
            let ent = ent.map_err(|e| walk_error(&dir, &e))?;
            let path = ent.path();
            if dir == root {
                names.push(ent.file_name().to_string_lossy().into_owned());
            }
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if scope
                .skip
                .iter()
                .any(|d| key(&relative.to_string_lossy(), scope.fold) == key(d, scope.fold))
            {
                continue;
            }
            let md = fs::symlink_metadata(&path).map_err(|e| walk_error(&path, &e))?;
            visit(&path, &md);
            if md.is_dir() {
                stack.push(path);
            }
        }
    }
    names.sort();
    Ok(names)
}

pub fn walk_activity(root: &Path) -> Result<Activity> {
    walk_activity_scoped(root, &ActivityScope::default(), &[])
}
pub fn walk_activity_scoped(
    root: &Path,
    scope: &ActivityScope,
    quiet_skip: &[String],
) -> Result<Activity> {
    walk_activity_inner(root, scope, quiet_skip, || {})
}

/// Test-only mutation after the scan's timestamp, before visiting entries.
#[cfg(feature = "test-seams")]
pub fn walk_activity_during_scan(
    root: &Path,
    scope: &ActivityScope,
    quiet_skip: &[String],
    during: impl FnOnce(),
) -> Result<Activity> {
    walk_activity_inner(root, scope, quiet_skip, during)
}

fn walk_activity_inner(
    root: &Path,
    scope: &ActivityScope,
    quiet_skip: &[String],
    during: impl FnOnce(),
) -> Result<Activity> {
    let t0_ns = ns(SystemTime::now());
    let md = fs::symlink_metadata(root).map_err(|e| walk_error(root, &e))?;
    let mut a = Activity {
        t0_ns,
        m0_ns: ctime_ns(&md),
        m0_counted: ctime_ns(&md),
        newest: root.to_path_buf(),
        entries: 1,
        late: (ctime_ns(&md) > t0_ns).then(|| root.to_path_buf()),
        root_names: Vec::new(),
        scope: scope.clone(),
    };
    let quiet = quiet_rules(quiet_skip, scope.fold);
    during();
    a.root_names = walk_scope(root, scope, |path, md| {
        let c = ctime_ns(md);
        a.entries += 1;
        a.m0_ns = a.m0_ns.max(c);
        let skipped = quiet_matches(&quiet, &rel(root, path), scope.fold);
        if !skipped {
            if c > a.m0_counted {
                a.m0_counted = c;
                a.newest = path.to_path_buf();
            }
            if c > a.t0_ns && a.late.is_none() {
                a.late = Some(path.to_path_buf());
            }
        }
    })?;
    Ok(a)
}
pub fn baseline_of(a: &Activity) -> strata_store::MoveBaseline {
    strata_store::MoveBaseline {
        scope: a.scope.clone(),
        m0_ns: i64::try_from(a.m0_ns).unwrap_or(i64::MAX),
        t0_ns: i64::try_from(a.t0_ns).unwrap_or(i64::MAX),
        root_names: a.root_names.clone(),
        entries: a.entries,
    }
}
pub fn changed_since(root: &Path, b: &strata_store::MoveBaseline) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let names = walk_scope(root, &b.scope, |path, md| {
        if ctime_ns(md) > b.m0_ns as i128 {
            out.push(rel(root, path));
        }
    })?;
    let (old, new): (std::collections::BTreeSet<_>, std::collections::BTreeSet<_>) =
        (b.root_names.iter().collect(), names.iter().collect());
    for name in old.symmetric_difference(&new) {
        out.push((*name).clone());
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn holders_json(root: &Path, holders: &[strata_deps::OpenFile], limit: usize) -> Vec<Value> {
    holders.iter().take(limit).map(|h| json!({"pid":h.pid,"command":h.command,"path":rel(root, &h.path),
        "hold":match h.hold { strata_deps::Hold::Fd => "fd", strata_deps::Hold::Cwd => "cwd", strata_deps::Hold::Mapped => "map" }})).collect()
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

fn walk_error(p: &Path, e: &std::io::Error) -> strata_types::StrataError {
    strata_apfs::classify_io_error(e, strata_apfs::IoSide::Read, p)
}

/// Why an `Auto` move is not made now (§1.4); `detail` is for people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Idle {
    pub reason: &'static str,
    pub detail: Value,
}

/// G1 then G2 over the whole workspace `root`: `None` when nobody holds
/// anything under it and nothing under it changed for `quiet`.
pub fn busy_reason(root: &Path, quiet: Duration) -> Result<Option<Idle>> {
    Ok(idle_activity(root, quiet)?.err())
}

/// [`busy_reason`] that returns G2's snapshot when the workspace is idle:
/// the baseline an `Auto` move persists and proves again (§1.4 G4).
pub fn idle_activity(root: &Path, quiet: Duration) -> Result<std::result::Result<Activity, Idle>> {
    idle_activity_scoped(root, quiet, &ActivityScope::default(), &[])
}

pub fn idle_activity_scoped(
    root: &Path,
    quiet: Duration,
    scope: &ActivityScope,
    quiet_skip: &[String],
) -> Result<std::result::Result<Activity, Idle>> {
    let holders = strata_deps::open_files(root)?;
    if !holders.is_empty() {
        let shown = holders_json(root, &holders, 20);
        return Ok(Err(Idle {
            reason: "open_files",
            detail: json!({ "holders": shown, "count": holders.len() }),
        }));
    }
    let a = match walk_activity_scoped(root, scope, quiet_skip) {
        Ok(a) => a,
        Err(e) => {
            return Ok(Err(Idle {
                reason: "recent_writes",
                detail: json!({ "walk_error": e.message, "code": e.code.as_str() }),
            }));
        }
    };
    if let Some(p) = &a.late {
        return Ok(Err(Idle {
            reason: "recent_writes",
            detail: json!({ "path": rel(root, p), "during_scan": true }),
        }));
    }
    let age_ns = ns(SystemTime::now()) - a.m0_counted;
    if age_ns < quiet.as_nanos() as i128 {
        return Ok(Err(Idle {
            reason: "recent_writes",
            detail: json!({ "path": rel(root, &a.newest), "age_secs": (age_ns.max(0) / 1_000_000_000) as u64, "quiet_secs": quiet.as_secs() }),
        }));
    }
    Ok(Ok(a))
}
