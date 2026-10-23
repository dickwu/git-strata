//! §5.4: adopt the main checkout's deps dirs as the first generation.

use std::path::Path;
use std::time::SystemTime;

use serde_json::json;
use strata_apfs::{Attrs, CloneKind, FileKind, WalkOptions};
use strata_store::AttemptKind;
use strata_types::{Code, DepsSet, RepoPath, StrataError};

use crate::build::{check_volume, inputs_tracked, key_of};
use crate::inputs::{inputs_hash, platform, toolchain, under};
use crate::layout::{Layout, Staging, join, parent, reason};
use crate::seal;
use crate::{Ctx, DepsConfigKey, Result};

/// One row of a walk of the main checkout's deps dirs (§5.4 step 6).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    path: String,
    kind: &'static str,
    size: u64,
    gen_count: Option<u64>,
    mtime: SystemTime,
    target: Option<std::path::PathBuf>,
}

impl Row {
    fn of(path: &RepoPath, a: &Attrs) -> Row {
        let (kind, target) = match &a.kind {
            FileKind::File => ("file", None),
            FileKind::Dir => ("dir", None),
            FileKind::Symlink { target } => ("symlink", Some(target.clone())),
            FileKind::Fifo => ("fifo", None),
            FileKind::Socket => ("socket", None),
            FileKind::CharDevice => ("char_device", None),
            FileKind::BlockDevice => ("block_device", None),
        };
        Row {
            path: path.as_str().to_string(),
            kind,
            size: a.size,
            gen_count: a.gen_count_known().then_some(a.gen_count),
            mtime: a.mtime,
            target,
        }
    }

    /// Equal for change detection: an unknown gen count never matches.
    fn same(&self, other: &Row) -> bool {
        self.gen_count.is_some() && self == other
    }
}

fn drift(reason: &str, message: String, detail: serde_json::Value) -> StrataError {
    let mut d = detail;
    if let serde_json::Value::Object(m) = &mut d {
        m.insert("reason".into(), json!(reason));
    }
    StrataError::new(Code::Drift, message).with_detail(d)
}

fn not_directory(main: &Path, d: &RepoPath, cause: Option<&StrataError>) -> StrataError {
    StrataError::new(
        Code::NotDirectory,
        format!("{} in the main checkout is not a directory", d.as_str()),
    )
    .with_detail(json!({ "path": main.join(d.as_str()), "error": cause }))
}

/// W1 / W2: walk every present deps dir of the main checkout, skipping
/// nested seed caches. Special files and escaping symlinks are `Drift`.
fn walk_main(main: &Path, present: &[RepoPath], nested: &[RepoPath]) -> Result<Vec<Row>> {
    let skip = |p: &Path| {
        let s = p.to_string_lossy();
        nested.iter().any(|c| under(&s, c.as_str()))
    };
    let filter = |p: &Path, _: &Attrs| !skip(p);
    let mut rows = Vec::new();
    for d in present {
        let a = strata_apfs::attrs_of(main, d)?;
        rows.push(Row::of(d, &a));
        let opts = WalkOptions {
            start: Some(d.clone()),
            ..Default::default()
        };
        for item in strata_apfs::walk_with(main, &opts, &filter) {
            let (path, attrs) = item?;
            match &attrs.kind {
                FileKind::File | FileKind::Dir => {}
                FileKind::Symlink { target } => {
                    if !seal::link_stays_inside(&path, target) {
                        return Err(drift(
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
                    return Err(drift(
                        "special_file",
                        format!("{} in the main checkout is a special file", path.as_str()),
                        json!({ "path": path.as_str() }),
                    ));
                }
            }
            rows.push(Row::of(&path, &attrs));
        }
    }
    rows.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(rows)
}

/// The first path (byte order) whose rows differ between W1 and W2.
fn first_difference(w1: &[Row], w2: &[Row]) -> Option<String> {
    let (mut i, mut j) = (0, 0);
    loop {
        match (w1.get(i), w2.get(j)) {
            (None, None) => return None,
            (Some(a), None) => return Some(a.path.clone()),
            (None, Some(b)) => return Some(b.path.clone()),
            (Some(a), Some(b)) => match a.path.as_bytes().cmp(b.path.as_bytes()) {
                std::cmp::Ordering::Less => return Some(a.path.clone()),
                std::cmp::Ordering::Greater => return Some(b.path.clone()),
                std::cmp::Ordering::Equal => {
                    if !a.same(b) {
                        return Some(a.path.clone());
                    }
                    i += 1;
                    j += 1;
                }
            },
        }
    }
}

/// §5.4: clone, check, sync and seal the main checkout's deps dirs when its
/// install inputs equal `tree`'s.
pub fn adopt(ctx: &Ctx, tree: &Path) -> Result<DepsSet> {
    // 1.
    if ctx.cfg.deps_dirs.is_empty() {
        return seal::seal_empty(ctx);
    }
    let main = ctx.cfg.path.as_path();
    // 2. Preflight; nothing written.
    inputs_tracked(ctx, tree)?;
    let (h, id) = key_of(ctx, tree)?;
    check_volume(ctx, main)?;
    let config = DepsConfigKey::of(ctx.cfg)?;
    let tc = toolchain(ctx.cfg, ctx.env)?;
    // 3.
    let m = inputs_hash(ctx.cfg, ctx.git, main)?;
    if m != h {
        return Err(drift(
            "inputs",
            "the main checkout's install inputs differ from the base".into(),
            json!({ "base_inputs": h, "main_inputs": m }),
        ));
    }
    // 4. The deps dirs present in the main checkout.
    let mut present = Vec::new();
    for d in &config.deps_dirs {
        match strata_apfs::attrs_of(main, d) {
            Ok(a) if a.kind == FileKind::Dir => present.push(d.clone()),
            Ok(_) => return Err(not_directory(main, d, None)),
            Err(e) if e.code == Code::NoSuchPath => continue,
            Err(e) if matches!(e.code, Code::NotDirectory | Code::NotFile) => {
                return Err(not_directory(main, d, Some(&e)));
            }
            Err(e) => return Err(e),
        }
    }
    if present.is_empty() {
        return Err(StrataError::new(
            Code::NoSuchPath,
            "the main checkout has none of the deps dirs",
        )
        .with_detail(json!({ "path": main, "deps_dirs": config.deps_dirs })));
    }
    let nested = &config.nested_seed_caches;

    // 5. Build lock and the checks after it (no D8 for adoption).
    let lay = Layout::of(ctx);
    let _lock = seal::lock_build(ctx, &lay, &id)?;
    if let Some(set) = seal::after_lock(ctx, &lay, &id, AttemptKind::Adopt)? {
        return Ok(set);
    }

    // 6. W1, before any write.
    let w1 = walk_main(main, &present, nested)?;

    // 7. Attempt and clone.
    let mut st = seal::begin(ctx, &lay, &id, AttemptKind::Adopt)?;
    if let Err(e) = clone_dirs(&mut st, main, &present, nested) {
        return Err(seal::refuse(ctx, &st, e));
    }

    // 8. W2 and the comparison.
    let w2 = match walk_main(main, &present, nested) {
        Ok(w) => w,
        Err(e) if e.code == Code::NoSuchPath => {
            let path = e.detail.as_ref().and_then(|d| d.get("path")).cloned();
            let e = drift(
                "changed_during_clone",
                "the main checkout's deps dirs changed during the clone".into(),
                json!({ "path": path }),
            );
            return Err(seal::refuse(ctx, &st, e));
        }
        Err(e) => return Err(seal::refuse(ctx, &st, e)),
    };
    if let Some(path) = first_difference(&w1, &w2) {
        let e = drift(
            "changed_during_clone",
            format!("{path} in the main checkout changed during the clone"),
            json!({ "path": path }),
        );
        return Err(seal::refuse(ctx, &st, e));
    }

    // 9. Validate and sync.
    if let Err(e) = seal::validate_and_sync(&st.dir.join("seal")) {
        // The validation reasons are adoption refusals: `Drift` (§5.4 step 9).
        let named = e.code == Code::Unknown
            && matches!(
                reason(&e),
                Some("unreadable_file" | "special_file" | "symlink_escapes")
            );
        let e = if named {
            StrataError {
                code: Code::Drift,
                ..e
            }
        } else {
            e
        };
        return Err(seal::refuse(ctx, &st, e));
    }

    // 10. Seal.
    let set = DepsSet {
        id,
        repo: ctx.repo.clone(),
        inputs_hash: h,
        platform: platform(),
        toolchain: tc,
        dirs: present,
    };
    seal::seal(ctx, &lay, &st, &set, AttemptKind::Adopt)
}

/// §5.4 step 7: clone each present deps dir into `seal/`, then move nested
/// seed caches out to `discard/`. A deps dir replaced since step 4 is
/// `NoSuchPath` or `NotDirectory`.
fn clone_dirs(
    st: &mut Staging,
    main: &Path,
    present: &[RepoPath],
    nested: &[RepoPath],
) -> Result<()> {
    for d in present {
        let dst = join("seal", d.as_str());
        match parent(&dst) {
            Some(p) => strata_apfs::mkdir_rooted(&st.dir, &p, 0o755, true)?,
            None => unreachable!("seal/<d> has a parent"),
        }
        match strata_apfs::clone_from_rooted(main, d, &st.dir.join(dst.as_str()), CloneKind::Dir) {
            Ok(()) => {}
            Err(e) if matches!(e.code, Code::NotDirectory | Code::NotFile) => {
                return Err(not_directory(main, d, Some(&e)));
            }
            Err(e) => return Err(e),
        }
    }
    for c in nested {
        let rel = join("seal", c.as_str());
        if seal::exists(&st.dir, &rel)? {
            st.discard(&rel)?;
        }
    }
    Ok(())
}
