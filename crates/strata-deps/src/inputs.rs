//! Install-input hash, deps set id, platform, toolchain (deps.md §4.3, §4.4,
//! §5.1, §5.2).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use strata_apfs::{FileKind, WalkOptions};
use strata_gitio::Git;
use strata_types::{Code, DepsSetId, RepoConfig, RepoPath, StrataError};

use crate::{DepsConfigKey, InstallEnv, Result};

const INPUTS_CONTEXT: &str = "git-strata install-inputs v1";
const SET_ID_CONTEXT: &str = "git-strata deps-set-id v2";
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

const TAG_ABSENT: u8 = 0;
const TAG_FILE: u8 = 1;
const TAG_SYMLINK: u8 = 2;
const TAG_MANIFEST: u8 = 3;

/// `path` is `prefix` or lies under it (the prefix ends at a `/`).
pub(crate) fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || (path.len() > prefix.len()
            && path.starts_with(prefix)
            && path.as_bytes()[prefix.len()] == b'/')
}

fn strictly_under(path: &str, prefix: &str) -> bool {
    path != prefix && under(path, prefix)
}

impl DepsConfigKey {
    /// `deps_dirs` and `nested_seed_caches` (each in byte order) and the
    /// `install` argv (config.md §2.4). `InvalidInput` when `install` does
    /// not split (V9).
    pub fn of(cfg: &RepoConfig) -> Result<DepsConfigKey> {
        let mut deps_dirs = cfg.deps_dirs.clone();
        deps_dirs.sort_by(|a, b| a.as_str().as_bytes().cmp(b.as_str().as_bytes()));
        deps_dirs.dedup();
        let install = cfg.install_argv()?.unwrap_or_default();
        Ok(DepsConfigKey {
            deps_dirs,
            nested_seed_caches: nested_seed_caches(cfg),
            install,
        })
    }

    /// The empty configuration (the empty deps set, §5.6).
    pub fn empty() -> DepsConfigKey {
        DepsConfigKey {
            deps_dirs: Vec::new(),
            nested_seed_caches: Vec::new(),
            install: Vec::new(),
        }
    }
}

/// `seed_caches` entries inside some `deps_dirs` entry, in byte order.
pub fn nested_seed_caches(cfg: &RepoConfig) -> Vec<RepoPath> {
    let mut out: Vec<RepoPath> = cfg
        .seed_caches
        .iter()
        .filter(|c| {
            cfg.deps_dirs
                .iter()
                .any(|d| strictly_under(c.as_str(), d.as_str()))
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| a.as_str().as_bytes().cmp(b.as_str().as_bytes()));
    out.dedup();
    out
}

/// `path` equals an `install_inputs` entry or lies under one.
pub fn is_input(cfg: &RepoConfig, path: &RepoPath) -> bool {
    cfg.install_inputs
        .iter()
        .any(|e| under(path.as_str(), e.as_str()))
}

/// `is_input` under the volume's name rules: `path` and every entry
/// compared by their case-fold keys (Unicode caseless, NFD), so
/// `BUN.LOCK` or a decomposed spelling of an input still counts
/// (rm-followups §1.8: an install input is never regenerable).
pub fn is_input_folded(cfg: &RepoConfig, path: &RepoPath) -> bool {
    let p = strata_apfs::case_fold_key(path.as_str());
    cfg.install_inputs
        .iter()
        .any(|e| under(&p, &strata_apfs::case_fold_key(e.as_str())))
}

/// The compile-time target triple (§4.3).
pub fn platform() -> String {
    format!("{}-apple-darwin", std::env::consts::ARCH)
}

fn write_bytes(h: &mut blake3::Hasher, b: &[u8]) {
    h.update(&(b.len() as u64).to_le_bytes());
    h.update(b);
}

/// §4.3, v2.
pub fn set_id(
    inputs_hash: &str,
    platform: &str,
    toolchain: &str,
    config: &DepsConfigKey,
) -> DepsSetId {
    let mut h = blake3::Hasher::new_derive_key(SET_ID_CONTEXT);
    write_bytes(&mut h, inputs_hash.as_bytes());
    write_bytes(&mut h, platform.as_bytes());
    write_bytes(&mut h, toolchain.as_bytes());
    for list in [&config.deps_dirs, &config.nested_seed_caches] {
        let mut v: Vec<&str> = list.iter().map(|p| p.as_str()).collect();
        v.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        h.update(&(v.len() as u64).to_le_bytes());
        for e in v {
            write_bytes(&mut h, e.as_bytes());
        }
    }
    h.update(&(config.install.len() as u64).to_le_bytes());
    for a in &config.install {
        write_bytes(&mut h, a.as_bytes());
    }
    DepsSetId::try_from(h.finalize().to_hex().to_string()).expect("blake3 hex is a deps set id")
}

/// The hash of no install inputs at all (the empty deps set).
pub fn empty_inputs_hash() -> String {
    blake3::Hasher::new_derive_key(INPUTS_CONTEXT)
        .finalize()
        .to_hex()
        .to_string()
}

struct Record {
    tag: u8,
    digest: Option<[u8; 32]>,
}

fn not_file(path: &str, what: &str) -> StrataError {
    StrataError::new(Code::NotFile, format!("install input {path} is a {what}"))
        .with_detail(json!({ "path": path, "reason": what }))
}

fn special_name(kind: &FileKind) -> &'static str {
    match kind {
        FileKind::Fifo => "fifo",
        FileKind::Socket => "socket",
        FileKind::CharDevice | FileKind::BlockDevice => "device",
        _ => "special",
    }
}

fn read_rooted(root: &Path, path: &RepoPath) -> Result<Vec<u8>> {
    let mut f = strata_apfs::open_rooted_file(root, path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| {
        strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &root.join(path.as_str()))
    })?;
    Ok(buf)
}

fn file_record(root: &Path, path: &RepoPath) -> Result<Record> {
    let bytes = read_rooted(root, path)?;
    let name = path.as_str().rsplit('/').next().unwrap_or("");
    if let Some(canon) = canonical_manifest(name, &bytes) {
        return Ok(Record {
            tag: TAG_MANIFEST,
            digest: Some(*blake3::hash(&canon).as_bytes()),
        });
    }
    Ok(Record {
        tag: TAG_FILE,
        digest: Some(*blake3::hash(&bytes).as_bytes()),
    })
}

fn link_record(target: &Path) -> Record {
    Record {
        tag: TAG_SYMLINK,
        digest: Some(*blake3::hash(target.as_os_str().as_bytes()).as_bytes()),
    }
}

/// One resolved install-input member (§5.1): a regular file or a symlink
/// (the link itself, never its target).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Member {
    File,
    Link(PathBuf),
}

/// The resolved members of every `install_inputs` entry under `root`, in
/// byte order, deduplicated. Absent entries (and empty directories) have no
/// members. Special files are `NotFile`.
pub(crate) fn input_members(
    cfg: &RepoConfig,
    git: &Git,
    root: &Path,
) -> Result<Vec<(RepoPath, Member)>> {
    Ok(resolve_inputs(cfg, git, root)?
        .into_iter()
        .filter_map(|(p, m)| m.map(|m| (p, m)))
        .collect())
}

/// Every entry with its members; `(entry, None)` marks an absent entry.
fn resolve_inputs(
    cfg: &RepoConfig,
    git: &Git,
    root: &Path,
) -> Result<Vec<(RepoPath, Option<Member>)>> {
    match fs::symlink_metadata(root) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(StrataError::new(
                Code::NotDirectory,
                format!("{} is not a directory", root.display()),
            )
            .with_detail(json!({ "path": root })));
        }
        Err(e) => {
            return Err(strata_apfs::classify_io_error(
                &e,
                strata_apfs::IoSide::Read,
                root,
            ));
        }
    }
    let worktree = fs::symlink_metadata(root.join(".git")).is_ok();
    let mut out: Vec<(RepoPath, Option<Member>)> = Vec::new();
    for e in &cfg.install_inputs {
        let attrs = match strata_apfs::attrs_of(root, e) {
            Ok(a) => a,
            Err(err) if matches!(err.code, Code::NoSuchPath | Code::NotDirectory) => {
                out.push((e.clone(), None));
                continue;
            }
            Err(err) => return Err(err),
        };
        match attrs.kind {
            FileKind::File => out.push((e.clone(), Some(Member::File))),
            FileKind::Symlink { target } => out.push((e.clone(), Some(Member::Link(target)))),
            FileKind::Dir => {
                let members = dir_members(git, root, e, worktree)?;
                if members.is_empty() {
                    out.push((e.clone(), None));
                } else {
                    out.extend(members.into_iter().map(|(p, m)| (p, Some(m))));
                }
            }
            other => return Err(not_file(e.as_str(), special_name(&other))),
        }
    }
    out.sort_by(|a, b| a.0.as_str().as_bytes().cmp(b.0.as_str().as_bytes()));
    out.dedup_by(|a, b| a.0 == b.0);
    Ok(out)
}

/// One canonical install-input record (§5.1): what `inputs_hash` hashes
/// for one member or absent entry. Two records are equal exactly when the
/// hash treats them the same (publish's drift diagnosis compares them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputRecord {
    pub path: RepoPath,
    tag: u8,
    digest: Option<[u8; 32]>,
}

/// The records `inputs_hash` hashes, in its order (byte order, deduplicated).
pub fn input_records(cfg: &RepoConfig, git: &Git, root: &Path) -> Result<Vec<InputRecord>> {
    let mut out = Vec::new();
    for (path, m) in resolve_inputs(cfg, git, root)? {
        let r = match m {
            None => Record { tag: TAG_ABSENT, digest: None },
            Some(Member::File) => file_record(root, &path)?,
            Some(Member::Link(target)) => link_record(&target),
        };
        out.push(InputRecord { path, tag: r.tag, digest: r.digest });
    }
    Ok(out)
}

/// The inputs hash of `records` (as [`input_records`] returns them).
pub fn hash_records(records: &[InputRecord]) -> String {
    let mut h = blake3::Hasher::new_derive_key(INPUTS_CONTEXT);
    for r in records {
        write_bytes(&mut h, r.path.as_str().as_bytes());
        h.update(&[r.tag]);
        if let Some(d) = &r.digest {
            h.update(d);
        }
    }
    h.finalize().to_hex().to_string()
}

/// §5.1 and §4.3.
pub fn inputs_hash(cfg: &RepoConfig, git: &Git, root: &Path) -> Result<String> {
    Ok(hash_records(&input_records(cfg, git, root)?))
}

fn dir_members(
    git: &Git,
    root: &Path,
    dir: &RepoPath,
    worktree: bool,
) -> Result<Vec<(RepoPath, Member)>> {
    let failure: Mutex<Option<StrataError>> = Mutex::new(None);
    let filter = |p: &Path, a: &strata_apfs::Attrs| -> bool {
        // Directories are always entered: a tracked file is never ignored,
        // even under an ignored directory (gitio's rule).
        if !worktree || a.kind == FileKind::Dir {
            return true;
        }
        let Some(s) = p.to_str() else { return true };
        let Ok(rp) = RepoPath::try_from(s.to_owned()) else {
            return true;
        };
        match git.is_ignored(root, &rp) {
            Ok(ignored) => !ignored,
            Err(e) => {
                let mut f = failure.lock().unwrap_or_else(|p| p.into_inner());
                if f.is_none() {
                    *f = Some(e);
                }
                false
            }
        }
    };
    let opts = WalkOptions {
        start: Some(dir.clone()),
        ..WalkOptions::default()
    };
    let mut out = Vec::new();
    for item in strata_apfs::walk_with(root, &opts, &filter) {
        let (path, attrs) = item?;
        if let Some(e) = failure.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Err(e);
        }
        match attrs.kind {
            FileKind::Dir => {}
            FileKind::File => out.push((path, Member::File)),
            FileKind::Symlink { target } => out.push((path, Member::Link(target))),
            other => return Err(not_file(path.as_str(), special_name(&other))),
        }
    }
    if let Some(e) = failure.lock().unwrap_or_else(|p| p.into_inner()).take() {
        return Err(e);
    }
    Ok(out)
}

/// §4.4: the canonical bytes of a known package manifest, or `None` when the
/// name is not in the table or the content is not a JSON object.
pub(crate) fn canonical_manifest(name: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    let lock = matches!(name, "package-lock.json" | "npm-shrinkwrap.json");
    if !lock && !matches!(name, "package.json" | "composer.json") {
        return None;
    }
    let Ok(Value::Object(mut obj)) = serde_json::from_slice::<Value>(bytes) else {
        return None;
    };
    obj.remove("version");
    if lock
        && let Some(Value::Object(pkgs)) = obj.get_mut("packages")
        && let Some(Value::Object(root)) = pkgs.get_mut("")
    {
        root.remove("version");
    }
    let mut out = Vec::new();
    write_canonical_object(&obj, &mut out);
    Some(out)
}

fn write_canonical(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Object(m) => write_canonical_object(m, out),
        Value::Array(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(x, out);
            }
            out.push(b']');
        }
        other => out.extend_from_slice(
            serde_json::to_string(other)
                .expect("scalar serializes")
                .as_bytes(),
        ),
    }
}

fn write_canonical_object(m: &Map<String, Value>, out: &mut Vec<u8>) {
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    out.push(b'{');
    for (i, k) in keys.into_iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(
            serde_json::to_string(k)
                .expect("string serializes")
                .as_bytes(),
        );
        out.push(b':');
        write_canonical(&m[k], out);
    }
    out.push(b'}');
}

// ---- toolchain (§5.2) ----

type CacheKey = (PathBuf, u64, u64, i64, i64);

fn cache() -> &'static Mutex<HashMap<CacheKey, String>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<CacheKey, String>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Resolve `program` along `path` (a PATH string); a name with a `/` is used
/// as given.
pub(crate) fn resolve_program(program: &str, path: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let p = PathBuf::from(program);
        return is_executable_file(&p).then_some(p);
    }
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(program))
        .find(|p| is_executable_file(p))
}

/// The sanitized environment install and `--version` run with (§5.5 step 7):
/// the daemon's environment without `GIT_*`, PATH replaced.
pub(crate) fn sanitize(cmd: &mut Command, env: &InstallEnv) {
    for (k, _) in std::env::vars_os() {
        if k.as_bytes().starts_with(b"GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.env("PATH", &env.path);
}

/// `<program name> <first line of --version>`, `""` without `install`.
pub fn toolchain(cfg: &RepoConfig, env: &InstallEnv) -> Result<String> {
    let Some(argv) = cfg.install_argv()? else {
        return Ok(String::new());
    };
    let program = &argv[0];
    let Some(resolved) = resolve_program(program, &env.path) else {
        return Err(StrataError::new(
            Code::NoSuchPath,
            format!("install program {program} not found on PATH"),
        )
        .with_detail(json!({ "program": program, "path": env.path })));
    };
    let real = fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
    let meta = fs::metadata(&real)
        .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &real))?;
    let key: CacheKey = (
        real.clone(),
        meta.ino(),
        meta.size(),
        meta.mtime(),
        meta.mtime_nsec(),
    );
    if let Some(hit) = cache().lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return Ok(hit.clone());
    }
    let name = resolved
        .file_name()
        .unwrap_or(OsStr::new(program))
        .to_string_lossy()
        .into_owned();
    let line = run_version(&resolved, env)?;
    let out = format!("{name} {line}");
    cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, out.clone());
    Ok(out)
}

fn first_line(b: &[u8]) -> Option<String> {
    String::from_utf8_lossy(b)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

fn run_version(program: &Path, env: &InstallEnv) -> Result<String> {
    let unknown = |msg: String, reason: &str| {
        StrataError::new(Code::Unknown, msg)
            .with_detail(json!({ "reason": reason, "program": program }))
    };
    let mut cmd = Command::new(program);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sanitize(&mut cmd, env);
    let mut child = cmd.spawn().map_err(|e| {
        unknown(
            format!("cannot run {} --version: {e}", program.display()),
            "version_failed",
        )
    })?;
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() >= VERSION_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unknown(
                    format!("{} --version timed out", program.display()),
                    "version_timeout",
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                return Err(unknown(
                    format!("waiting for --version: {e}"),
                    "version_failed",
                ));
            }
        }
    };
    let out = t_out.join().unwrap_or_default();
    let err = t_err.join().unwrap_or_default();
    if !status.success() {
        return Err(unknown(
            format!("{} --version exited with {status}", program.display()),
            "version_failed",
        ));
    }
    first_line(&out)
        .or_else(|| first_line(&err))
        .ok_or_else(|| {
            unknown(
                format!("{} --version printed nothing", program.display()),
                "version_empty",
            )
        })
}
