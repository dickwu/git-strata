//! Task 10: restore scenes, verification, cross-base projection and the
//! randomised incremental-equals-cold property (§5.7).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use common::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use strata_apfs::WatchEvent;
use strata_overlay::{
    Base, Checkpointed, DEFAULT_DIRTY_CAPACITY, Overlay, OverlayFault, RESTORE_TEMP_PREFIX,
    RestoreRequest, Restored, Tracker, WorkspaceRef, checkpoint, compute, inject_fault,
    reconcile_full, release_fault, restore, wait_fault,
};
use strata_store::{MoveState, Store};
use strata_types::{
    BaseGen, CheckpointId, CheckpointReason, Code, DepsSetId, EntryKind, OverlayEntry, RepoName,
    SessionId, StrataError, TreeOid,
};

// ---------------------------------------------------------------- helpers

fn tracker(w: &World) -> Tracker {
    Tracker::new(&w.ws, DEFAULT_DIRTY_CAPACITY)
}

fn cold_on(w: &World, base: &Base) -> Overlay {
    let o = reconcile_full(&w.ws_ref(), base).expect("reconcile_full");
    o.validate().expect("validate");
    o
}

fn cold(w: &World) -> Overlay {
    cold_on(w, &w.base)
}

fn manual() -> CheckpointReason {
    CheckpointReason::Manual {
        message: "m".to_owned(),
    }
}

fn cp(w: &World, t: Option<&mut Tracker>) -> Checkpointed {
    let c = checkpoint(&w.ws_ref(), &w.base, t, manual()).expect("checkpoint");
    assert_eq!(
        Overlay::from_checkpoint(&c.checkpoint),
        cold(w),
        "the checkpoint equals the cold scan"
    );
    c
}

fn id(c: &Checkpointed) -> CheckpointId {
    c.checkpoint.id
}

fn req<'a>(target: CheckpointId, tb: Option<&'a Base>, verify: bool) -> RestoreRequest<'a> {
    RestoreRequest {
        target,
        target_base: tb,
        paths: None,
        verify,
    }
}

fn try_rs(w: &World, target: CheckpointId) -> Result<Restored, StrataError> {
    restore(&w.ws_ref(), &w.base, None, &req(target, None, false))
}

fn try_rsv(w: &World, target: CheckpointId, verify: bool) -> Result<Restored, StrataError> {
    restore(&w.ws_ref(), &w.base, None, &req(target, None, verify))
}

/// A restore that must succeed; afterwards the workspace's overlay is the
/// target's and T is gone.
fn rs(w: &World, target: CheckpointId) -> Restored {
    let r = try_rs(w, target).expect("restore");
    let want = Overlay::from_checkpoint(&w.store.checkpoint(&w.sid, target).unwrap());
    assert_eq!(cold(w).entries, want.entries, "the workspace is the target");
    assert!(!temp(w).exists(), "T is removed after success");
    r
}

fn temp(w: &World) -> PathBuf {
    w.ws.parent()
        .unwrap()
        .join(format!("{RESTORE_TEMP_PREFIX}{}", w.repo.as_str()))
}

fn latest(w: &World) -> Option<CheckpointId> {
    w.store
        .latest_checkpoint(&w.sid, &w.repo)
        .unwrap()
        .map(|c| c.id)
}

fn reason_of(w: &World, c: CheckpointId) -> CheckpointReason {
    w.store.checkpoint(&w.sid, c).unwrap().reason
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    File { bytes: Vec<u8>, exec: bool },
    Link(Vec<u8>),
    Dir,
}

/// Every path under `dir` except the top-level `.git`.
fn tree(dir: &Path) -> BTreeMap<String, Node> {
    let mut out = BTreeMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for e in fs::read_dir(dir.join(&rel)).unwrap() {
            let e = e.unwrap();
            let r = rel.join(e.file_name());
            if r.as_os_str() == ".git" {
                continue;
            }
            let m = fs::symlink_metadata(e.path()).unwrap();
            let key = r.to_string_lossy().into_owned();
            if m.file_type().is_symlink() {
                let t = fs::read_link(e.path()).unwrap();
                out.insert(key, Node::Link(t.as_os_str().as_bytes().to_vec()));
            } else if m.is_dir() {
                out.insert(key, Node::Dir);
                stack.push(r);
            } else {
                out.insert(
                    key,
                    Node::File {
                        bytes: fs::read(e.path()).unwrap(),
                        exec: m.mode() & 0o111 != 0,
                    },
                );
            }
        }
    }
    out
}

/// `tree` without directories (empty directories are not recorded).
fn files(dir: &Path) -> BTreeMap<String, Node> {
    tree(dir)
        .into_iter()
        .filter(|(_, n)| *n != Node::Dir)
        .collect()
}

fn entry<'a>(o: &'a Overlay, p: &str) -> Option<&'a OverlayEntry> {
    o.entries.iter().find(|e| e.path.as_str() == p)
}

fn blob_at(o: &Overlay, p: &str) -> Option<String> {
    entry(o, p).and_then(|e| match &e.kind {
        EntryKind::Added { blob }
        | EntryKind::Modified { blob }
        | EntryKind::Untracked { blob }
        | EntryKind::Renamed { blob, .. } => Some(blob.as_str().to_owned()),
        EntryKind::Deleted => None,
    })
}

fn ino(w: &World, rel: &str) -> u64 {
    fs::symlink_metadata(w.ws.join(rel)).unwrap().ino()
}

fn chmod(p: &Path, mode: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

fn reason(e: &StrataError) -> Option<&str> {
    e.detail.as_ref()?.get("reason")?.as_str()
}

fn detail_str<'a>(e: &'a StrataError, k: &str) -> Option<&'a str> {
    e.detail.as_ref()?.get(k)?.as_str()
}

fn shown(w: &World, rel: &str) -> String {
    w.ws.join(rel).to_string_lossy().into_owned()
}

/// The names directly in T.
fn t_names(w: &World) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(temp(w))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Arms `f` under `root`, runs `run` on this thread and `during` on another
/// while `run` is paused at the fault, then lets it go on.
fn paused<R>(
    root: &Path,
    f: OverlayFault,
    during: impl FnOnce() + Send,
    run: impl FnOnce() -> R,
) -> R {
    inject_fault(root, f.clone());
    thread::scope(|s| {
        let h = s.spawn(move || {
            let got = wait_fault(root);
            if got.is_some() {
                during();
            }
            release_fault(root);
            got
        });
        let r = run();
        // A run that never reached the fault: disarm it so the waiter ends.
        release_fault(root);
        let got = h.join().expect("during");
        assert_eq!(got, Some(f), "the run paused at the fault");
        r
    })
}

// ---------------------------------------------------------------- unrecorded content

const GI: &str = "*.log\n/node_modules\n/target\n.DS_Store\n.env\n/.next\n/src\n!/src/\n";

#[test]
fn restore_never_overwrites_unrecorded_content() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, ".gitignore", GI.as_bytes());
    });
    let c0 = cp(&w, None);

    // A normal restore keeps ignored files, deps dirs and seeds: same bytes,
    // same inodes.
    let keep = [
        "node_modules/pkg/index.js",
        ".next/cache/x.bin",
        ".env",
        "build.log",
    ];
    for k in keep {
        w.write(k, format!("kept {k}\n").as_bytes());
    }
    let kept: Vec<(Vec<u8>, u64)> = keep.iter().map(|k| (w.read(k), ino(&w, k))).collect();
    w.write("README.md", b"edited\n");
    w.write("new.txt", b"new\n");
    rs(&w, id(&c0));
    assert_eq!(w.read("README.md"), b"hello\n");
    assert!(!w.ws.join("new.txt").exists());
    let now: Vec<(Vec<u8>, u64)> = keep.iter().map(|k| (w.read(k), ino(&w, k))).collect();
    assert_eq!(now, kept, "ignored, deps and seed files untouched");

    // The target writes out/report.txt where an ignored file now sits.
    w.write("out/report.txt", b"v1\n");
    let c1 = cp(&w, None);
    w.remove("out/report.txt");
    w.write(".gitignore", format!("{GI}out/\n").as_bytes());
    w.write("out/report.txt", b"ignored bytes\n");
    let (at, rows) = (tree(&w.ws), latest(&w));
    let e = try_rs(&w, id(&c1)).unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!(
        detail_str(&e, "path"),
        Some(shown(&w, "out/report.txt").as_str())
    );
    assert_eq!(
        (tree(&w.ws), latest(&w)),
        (at, rows),
        "nothing written, no row"
    );
    assert!(!temp(&w).exists());
    w.write(".gitignore", GI.as_bytes());
    rs(&w, id(&c0));

    // An ignored .DS_Store in a directory the restore would empty.
    w.write("newdir/a.txt", b"a\n");
    w.write("newdir/.DS_Store", b"ds\n");
    let (at, rows) = (tree(&w.ws), latest(&w));
    let e = try_rs(&w, id(&c0)).unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!(
        detail_str(&e, "path"),
        Some(shown(&w, "newdir/.DS_Store").as_str())
    );
    assert_eq!((tree(&w.ws), latest(&w)), (at, rows));
    w.remove("newdir/.DS_Store");
    rs(&w, id(&c0));
    assert!(!w.ws.join("newdir").exists());

    // src is now an ignored file while the target has src/new.ts.
    w.write("src/new.ts", b"new\n");
    let c2 = cp(&w, None);
    w.remove("src");
    fs::write(w.ws.join("src"), b"an ignored file\n").unwrap();
    assert!(entry(&cold(&w), "src").is_none(), "src is ignored");
    let (at, rows) = (tree(&w.ws), latest(&w));
    let e = try_rs(&w, id(&c2)).unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!(detail_str(&e, "path"), Some(shown(&w, "src").as_str()));
    assert_eq!((tree(&w.ws), latest(&w)), (at, rows));
    w.remove("src");
    rs(&w, id(&c2));
    assert_eq!(w.read("src/new.ts"), b"new\n");
}

// ---------------------------------------------------------------- scenes

#[derive(serde::Serialize, serde::Deserialize)]
struct ChildArgs {
    root: PathBuf,
    main: PathBuf,
    home: PathBuf,
    xdg: PathBuf,
    ws: PathBuf,
    tree: String,
    deps: String,
    target: u64,
}

/// Run by `restore_concurrent_write_is_kept_in_scene` in a child process: a
/// restore that pauses after its first rename and prints PAUSED; the parent
/// kills it there.
#[test]
#[ignore = "helper: run only as a child process"]
fn restore_child_helper() {
    let Ok(args) = std::env::var("STRATA_CHILD") else {
        return;
    };
    let a: ChildArgs = serde_json::from_str(&args).unwrap();
    let store = Store::open(&a.root, &[&a.main]).unwrap();
    let env = [
        ("HOME", Some(a.home.as_os_str())),
        ("XDG_CONFIG_HOME", Some(a.xdg.as_os_str())),
        ("GIT_CONFIG_NOSYSTEM", Some(std::ffi::OsStr::new("1"))),
    ];
    let git = strata_gitio::Git::open_with_env(&a.main, &env).unwrap();
    let config = config_for(&a.main);
    let repo = RepoName::try_from("app".to_owned()).unwrap();
    let sid = SessionId::try_from("s-ovrlayaa".to_owned()).unwrap();
    let tree = TreeOid::try_from(a.tree.clone()).unwrap();
    let g = BaseGen {
        repo: repo.clone(),
        tree: tree.clone(),
        deps: DepsSetId::try_from(a.deps.clone()).unwrap(),
    };
    let base = Base {
        manifest: std::sync::Arc::new(store.manifest(&repo, &tree).unwrap().unwrap()),
        dir: store.base_dir(&g),
        r#gen: g,
    };
    let ws = WorkspaceRef {
        session: &sid,
        repo: &repo,
        root: &a.ws,
        config: &config,
        git: &git,
        store: &store,
    };
    let target: CheckpointId = serde_json::from_value(serde_json::json!(a.target)).unwrap();
    inject_fault(&a.ws, OverlayFault::PauseInApply { after: 1 });
    let root = a.ws.clone();
    thread::spawn(move || {
        if wait_fault(&root).is_some() {
            println!("PAUSED");
            std::io::stdout().flush().unwrap();
        }
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    });
    let _ = restore(&ws, &base, None, &req(target, None, false));
    println!("FINISHED");
}

#[test]
fn restore_concurrent_write_is_kept_in_scene() {
    let mut w = World::with(|m| {
        default_tree(m);
        put(m, "d/one.txt", b"1\n");
        put(m, "d/two.txt", b"2\n");
    });
    let pristine = tree(&w.ws);
    let c1 = cp(&w, None);
    let ws = w.ws.clone();

    // after:0 — the BeforeRestore row and plan.json exist, nothing moved yet.
    w.write("README.md", b"edited\n");
    let top = latest(&w);
    let t_dir = temp(&w);
    let ws2 = ws.clone();
    let seen = paused(
        &ws,
        OverlayFault::PauseInApply { after: 0 },
        || {
            assert!(t_dir.join("plan.json").is_file(), "plan.json is in T");
            assert_eq!(fs::read(ws2.join("README.md")).unwrap(), b"edited\n");
        },
        || try_rs(&w, id(&c1)),
    )
    .expect("restore");
    assert_ne!(Some(seen.before), top);
    assert_eq!(reason_of(&w, seen.before), CheckpointReason::BeforeRestore);
    assert_eq!(tree(&w.ws), pristine);

    // after:1 — a file planned for replacement is rewritten mid-apply.
    w.write("README.md", b"r1\n");
    w.write("src/lib.rs", b"l1\n");
    w.write("src/deep/x/y.txt", b"y1\n");
    let lib = ws.join("src/lib.rs");
    let e = paused(
        &ws,
        OverlayFault::PauseInApply { after: 1 },
        || fs::write(&lib, b"concurrent\n").unwrap(),
        || try_rs(&w, id(&c1)),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.clone().unwrap();
    for k in ["before", "done", "pending", "temp"] {
        assert!(d.get(k).is_some(), "{k}: {d}");
    }
    assert_eq!(
        d["temp"].as_str(),
        Some(temp(&w).to_string_lossy().as_ref())
    );
    let kept: Vec<String> = t_names(&w)
        .into_iter()
        .filter(|n| fs::read(temp(&w).join(n)).unwrap() == b"concurrent\n")
        .collect();
    assert_eq!(kept.len(), 1, "the rewrite is kept in T: {:?}", t_names(&w));

    // Every later restore stops at the unproven file, before any row.
    for _ in 0..2 {
        let rows = latest(&w);
        let e = try_rs(&w, id(&c1)).unwrap_err();
        assert_eq!((e.code, reason(&e)), (Code::Unknown, Some("restore_scene")));
        let un = e.detail.as_ref().unwrap()["unproven"].as_array().unwrap();
        assert!(
            un.iter()
                .any(|u| u["name"] == kept[0].as_str() && u["path"] == "src/lib.rs"),
            "{un:?}"
        );
        assert_eq!(latest(&w), rows, "no row");
    }
    // Moved away: the rest of the scene is proven and settled.
    let rescued = w.tmp.join("rescued");
    fs::rename(temp(&w).join(&kept[0]), &rescued).unwrap();
    rs(&w, id(&c1));
    assert_eq!(tree(&w.ws), pristine);
    assert_eq!(fs::read(&rescued).unwrap(), b"concurrent\n");

    // A child process killed after the first delete: the next restore settles.
    w.write("new.txt", b"new\n");
    w.write("README.md", b"killed\n");
    let args = ChildArgs {
        root: w.root.clone(),
        main: w.main.clone(),
        home: w.home.clone(),
        xdg: w.xdg.clone(),
        ws: w.ws.clone(),
        tree: w.base.r#gen.tree.as_str().to_owned(),
        deps: w.base.r#gen.deps.as_str().to_owned(),
        target: id(&c1).get(),
    };
    // One writer per store: hand the writer lock to the child meanwhile.
    let ro = Store::open_read_only(&w.root).unwrap();
    std::mem::replace(&mut w.store, ro).close().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "restore_child_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("STRATA_CHILD", serde_json::to_string(&args).unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let paused_ok = lines.any(|l| l.is_ok_and(|l| l.contains("PAUSED")));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(paused_ok, "the child paused after its first rename");
    let rw = Store::open(&w.root, &[&w.main]).unwrap();
    std::mem::replace(&mut w.store, rw).close().unwrap();
    assert!(!w.ws.join("new.txt").exists(), "new.txt was moved into T");
    assert!(t_names(&w).len() >= 2, "{:?}", t_names(&w));
    rs(&w, id(&c1));
    assert_eq!(tree(&w.ws), pristine);

    // T is a symlink: Exists, its target untouched.
    let outside = w.tmp.join("outside");
    put(&outside, "keep.txt", b"keep\n");
    let out_tree = tree(&outside);
    symlink(&outside, temp(&w)).unwrap();
    w.write("README.md", b"edited\n");
    let rows = latest(&w);
    let e = try_rs(&w, id(&c1)).unwrap_err();
    assert_eq!(
        (e.code, reason(&e)),
        (Code::Exists, Some("restore_temp_taken"))
    );
    assert_eq!((tree(&outside), latest(&w)), (out_tree.clone(), rows));
    fs::remove_file(temp(&w)).unwrap();

    // A T symlink out of R appears after the BeforeRestore row: Busy.
    let rows = latest(&w);
    let t_dir = temp(&w);
    let out2 = outside.clone();
    let e = paused(
        &ws,
        OverlayFault::PauseBeforePrecheck,
        || symlink(&out2, &t_dir).unwrap(),
        || try_rs(&w, id(&c1)),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, reason(&e)),
        (Code::Busy, Some("workspace_changed")),
        "{e:?}"
    );
    let now = latest(&w);
    assert_ne!(now, rows);
    assert_eq!(reason_of(&w, now.unwrap()), CheckpointReason::BeforeRestore);
    assert_eq!(tree(&outside), out_tree);
    assert_eq!(w.read("README.md"), b"edited\n");
    fs::remove_file(temp(&w)).unwrap();

    // A planned file edited before the precheck: Busy, no row.
    let rows = latest(&w);
    let readme = ws.join("README.md");
    let e = paused(
        &ws,
        OverlayFault::PauseBeforePrecheck,
        || fs::write(&readme, b"edited again, longer\n").unwrap(),
        || try_rs(&w, id(&c1)),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, reason(&e)),
        (Code::Busy, Some("workspace_changed")),
        "{e:?}"
    );
    assert_eq!(latest(&w), rows, "no row");
    assert_eq!(w.read("README.md"), b"edited again, longer\n");
    assert!(!temp(&w).exists());
    rs(&w, id(&c1));

    // Target `d` is a file, current d/ a directory: a file created in d/
    // after its last original file left stops the restore.
    w.remove("d");
    w.write("d", b"file d\n");
    let c_file = cp(&w, None);
    rs(&w, id(&c1));
    let newer = ws.join("d/new.txt");
    let e = paused(
        &ws,
        OverlayFault::PauseInApply { after: 2 },
        || fs::write(&newer, b"late\n").unwrap(),
        || try_rs(&w, id(&c_file)),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert!(w.ws.join("d").is_dir());
    assert_eq!(w.read("d/new.txt"), b"late\n");
    let before: CheckpointId =
        serde_json::from_value(e.detail.as_ref().unwrap()["before"].clone()).unwrap();
    let bo = Overlay::from_checkpoint(&w.store.checkpoint(&w.sid, before).unwrap());
    for p in ["d", "d/one.txt", "d/two.txt"] {
        assert!(entry(&bo, p).is_none(), "{p} is base in BeforeRestore");
    }
    w.remove("d/new.txt");
    rs(&w, id(&c1));
    assert_eq!(tree(&w.ws), pristine);
    // Uninterrupted: d/ goes, d is the target's file.
    rs(&w, id(&c_file));
    assert_eq!(w.read("d"), b"file d\n");
}

// ---------------------------------------------------------------- symlinked dirs

#[test]
fn restore_never_writes_through_a_symlinked_directory() {
    let w = World::new();
    let c0 = cp(&w, None);
    let ws = w.ws.clone();

    // src/ replaced by a link to a directory outside.
    let outside = w.tmp.join("outside");
    put(&outside, "lib.rs", b"outside\n");
    let out_tree = tree(&outside);
    w.remove("src");
    symlink(&outside, w.ws.join("src")).unwrap();
    rs(&w, id(&c0));
    assert!(
        fs::symlink_metadata(w.ws.join("src"))
            .unwrap()
            .file_type()
            .is_dir()
    );
    assert_eq!(w.read("src/lib.rs"), b"pub fn a() {}\n");
    assert_eq!(tree(&outside), out_tree);

    // Swapped before the precheck: Exists, nothing written, no row.
    w.write("src/lib.rs", b"edited\n");
    let rows = latest(&w);
    let aside = w.tmp.join("src-aside");
    let (src, out, aside2) = (ws.join("src"), outside.clone(), aside.clone());
    let e = paused(
        &ws,
        OverlayFault::PauseBeforePrecheck,
        || {
            fs::rename(&src, &aside2).unwrap();
            symlink(&out, &src).unwrap();
        },
        || try_rs(&w, id(&c0)),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!(detail_str(&e, "path"), Some(shown(&w, "src").as_str()));
    assert_eq!(latest(&w), rows);
    assert_eq!(tree(&outside), out_tree);
    assert!(!temp(&w).exists());
    fs::remove_file(w.ws.join("src")).unwrap();
    fs::rename(&aside, w.ws.join("src")).unwrap();

    // Swapped after the first rename: Unknown, outside untouched, the rest
    // stays in T.
    w.write("README.md", b"edited\n");
    let (src, out, aside2) = (ws.join("src"), outside.clone(), aside.clone());
    let e = paused(
        &ws,
        OverlayFault::PauseInApply { after: 1 },
        || {
            fs::rename(&src, &aside2).unwrap();
            symlink(&out, &src).unwrap();
        },
        || try_rs(&w, id(&c0)),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(tree(&outside), out_tree);
    assert_eq!(w.read("README.md"), b"hello\n");
    let staged: Vec<String> = t_names(&w)
        .into_iter()
        .filter(|n| fs::read(temp(&w).join(n)).unwrap() == b"pub fn a() {}\n")
        .collect();
    assert_eq!(staged.len(), 1, "{:?}", t_names(&w));
    fs::remove_file(w.ws.join("src")).unwrap();
    fs::rename(&aside, w.ws.join("src")).unwrap();
    rs(&w, id(&c0));

    // T replaced by a link to an empty outside dir after plan.json.
    w.write("README.md", b"edited\n");
    let at = tree(&w.ws);
    let empty = w.tmp.join("empty-out");
    fs::create_dir(&empty).unwrap();
    let t_aside = w.tmp.join("t-aside");
    let (t_dir, e2, ta) = (temp(&w), empty.clone(), t_aside.clone());
    let e = paused(
        &ws,
        OverlayFault::PauseAfterPlan,
        || {
            fs::rename(&t_dir, &ta).unwrap();
            symlink(&e2, &t_dir).unwrap();
        },
        || try_rs(&w, id(&c0)),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(
        detail_str(&e, "temp"),
        Some(temp(&w).to_string_lossy().as_ref())
    );
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
    assert_eq!(tree(&w.ws), at);
    fs::remove_file(temp(&w)).unwrap();
    fs::rename(&t_aside, temp(&w)).unwrap();
    rs(&w, id(&c0));

    // The staged name swapped for a hard link to an outside 0600 file.
    let secret = w.tmp.join("secret");
    fs::write(&secret, b"secret\n").unwrap();
    chmod(&secret, 0o600);
    let st = Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(&secret)
        .status()
        .unwrap();
    assert!(st.success());
    let m0 = fs::metadata(&secret).unwrap();
    w.write("README.md", b"edited\n");
    let (t_dir, sec) = (temp(&w), secret.clone());
    let e = paused(
        &ws,
        OverlayFault::PauseBeforeTempMeta,
        || {
            let name = fs::read_dir(&t_dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .find(|n| n != "plan.json")
                .expect("a staged file");
            fs::remove_file(t_dir.join(&name)).unwrap();
            fs::hard_link(&sec, t_dir.join(&name)).unwrap();
        },
        || try_rs(&w, id(&c0)),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(
        detail_str(&e, "temp"),
        Some(temp(&w).to_string_lossy().as_ref())
    );
    let m1 = fs::metadata(&secret).unwrap();
    assert_eq!(
        (m1.mode() & 0o7777, m1.mtime(), m1.mtime_nsec()),
        (m0.mode() & 0o7777, m0.mtime(), m0.mtime_nsec())
    );
    assert_eq!(fs::read(&secret).unwrap(), b"secret\n");
    assert_eq!(w.read("README.md"), b"edited\n");
    for n in t_names(&w) {
        if n != "plan.json" {
            fs::remove_file(temp(&w).join(n)).unwrap();
        }
    }
    rs(&w, id(&c0));
}

// ---------------------------------------------------------------- cross-base

fn move_to(w: &World, nb: &Base, head: &strata_types::CommitOid, at: u64) {
    let staging = w.ws.parent().unwrap().join(".staging-app");
    let n = w
        .store
        .begin_move(&w.sid, &w.repo, &nb.r#gen, head, &staging, t(at))
        .unwrap();
    w.store
        .set_move_state(
            &w.sid,
            &w.repo,
            n,
            MoveState::Preparing,
            MoveState::Swapping,
            Some(&[]),
            t(at + 1),
        )
        .unwrap();
    w.store
        .finish_move(&w.sid, &w.repo, n, &[], t(at + 2))
        .unwrap();
}

#[test]
fn restore_across_bases_goes_through_rebase() {
    let w = World::with(|m| {
        default_tree(m);
        put(m, "s.txt", b"s0\n");
        put(m, "u.txt", b"u0\n");
        put(m, "b.txt", b"b0\n");
    });
    // Checkpoint on T1 with session changes to s and b.
    w.write("s.txt", b"s-session\n");
    w.write("b.txt", b"b-session\n");
    let c1 = cp(&w, None);
    // Upstream changes u and b; the workspace moves to T2.
    let (nb, head) = w.next_base(|m| {
        put(m, "u.txt", b"u-upstream\n");
        put(m, "b.txt", b"b-upstream\n");
    });
    move_to(&w, &nb, &head, 50);
    w.write("s.txt", b"s0\n");
    w.write("u.txt", b"u-upstream\n");
    w.write("b.txt", b"b-upstream\n");
    assert!(cold_on(&w, &nb).entries.is_empty());
    let t2_tree = tree(&nb.dir);

    let rows = latest(&w);
    let e = restore(&w.ws_ref(), &nb, None, &req(id(&c1), None, false)).unwrap_err();
    assert_eq!(
        (e.code, reason(&e)),
        (Code::InvalidInput, Some("target_base"))
    );
    assert_eq!(latest(&w), rows);

    let r = restore(&w.ws_ref(), &nb, None, &req(id(&c1), Some(&w.base), false))
        .expect("cross-base restore");
    assert_eq!(w.read("s.txt"), b"s-session\n");
    assert_eq!(w.read("u.txt"), b"u-upstream\n");
    assert_eq!(w.read("b.txt"), b"b-session\n");
    assert_eq!(
        r.conflicts.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        ["b.txt"]
    );
    assert_eq!(tree(&nb.dir), t2_tree, "the T2 base dir is untouched");

    // T1's directory deleted and exported again elsewhere: same BaseGen.
    let again = w.tmp.join("t1-again");
    w.git
        .export_tree(&w.base.r#gen.tree, &again)
        .expect("export again");
    let st = Command::new("chmod")
        .args(["-R", "u+w"])
        .arg(&w.base.dir)
        .status()
        .unwrap();
    assert!(st.success());
    fs::remove_dir_all(&w.base.dir).unwrap();
    let t1 = Base {
        dir: again,
        ..w.base.clone()
    };
    w.write("s.txt", b"s0\n");
    w.write("b.txt", b"b-upstream\n");
    let r =
        restore(&w.ws_ref(), &nb, None, &req(id(&c1), Some(&t1), false)).expect("re-exported base");
    assert_eq!(w.read("s.txt"), b"s-session\n");
    assert_eq!(w.read("b.txt"), b"b-session\n");
    assert_eq!(r.conflicts.len(), 1);

    // Another tree OID is not the same base, whatever its contents.
    let (t3, _) = w.next_base(|m| put(m, "s.txt", b"s3\n"));
    w.write("s.txt", b"s0\n");
    let e = restore(&w.ws_ref(), &nb, None, &req(id(&c1), Some(&t3), false)).unwrap_err();
    assert_eq!(
        (e.code, reason(&e)),
        (Code::InvalidInput, Some("target_base")),
        "{e:?}"
    );
    assert_eq!(w.read("s.txt"), b"s0\n");
}

// ---------------------------------------------------------------- verify

fn rewrite_in_place(p: &Path, f: impl FnOnce(&mut Vec<u8>)) {
    let mode = fs::metadata(p).unwrap().mode() & 0o7777;
    chmod(p, 0o644);
    let mut b = fs::read(p).unwrap();
    f(&mut b);
    let mut h = fs::OpenOptions::new().write(true).open(p).unwrap();
    h.seek(SeekFrom::Start(0)).unwrap();
    h.write_all(&b).unwrap();
    h.set_len(b.len() as u64).unwrap();
    drop(h);
    chmod(p, mode);
}

#[test]
fn restore_verify_detects_corruption() {
    let w = World::new();
    let c0 = cp(&w, None);
    let payload = b"corruptible payload 0123456789\n";
    w.write("data.bin", payload);
    let c1 = cp(&w, None);
    assert!(blob_at(&Overlay::from_checkpoint(&c1.checkpoint), "data.bin").is_some());
    let blob = w
        .store_files()
        .into_iter()
        .find(|p| fs::read(p).is_ok_and(|b| b == payload))
        .expect("the blob file");

    // One byte flipped, same length.
    rewrite_in_place(&blob, |b| b[5] ^= 0x20);
    let flipped = fs::read(&blob).unwrap();
    w.write("data.bin", b"other\n");
    let (at, rows) = (tree(&w.ws), latest(&w));
    let e = try_rsv(&w, id(&c1), true).unwrap_err();
    assert_eq!(e.code, Code::Corrupt, "{e:?}");
    assert_eq!((tree(&w.ws), latest(&w)), (at, rows), "nothing written");
    try_rsv(&w, id(&c1), false).expect("without verify the blob is trusted");
    assert_eq!(w.read("data.bin"), flipped);

    // A truncated base file to clone: Corrupt even without verify.
    let base_readme = w.base.dir.join("README.md");
    w.write("README.md", b"edited\n");
    rewrite_in_place(&base_readme, |b| b.truncate(2));
    let (at, rows) = (tree(&w.ws), latest(&w));
    let e = try_rsv(&w, id(&c0), false).unwrap_err();
    assert_eq!(e.code, Code::Corrupt, "{e:?}");
    assert_eq!((tree(&w.ws), latest(&w)), (at.clone(), rows));

    // A same-length change to a base file: only verify sees it.
    rewrite_in_place(&base_readme, |b| *b = b"HELLO\n".to_vec());
    let e = try_rsv(&w, id(&c0), true).unwrap_err();
    assert_eq!(e.code, Code::Corrupt, "{e:?}");
    assert_eq!((tree(&w.ws), latest(&w)), (at, rows));
    try_rsv(&w, id(&c0), false).expect("without verify");
    assert_eq!(w.read("README.md"), b"HELLO\n");
    rewrite_in_place(&base_readme, |b| *b = b"hello\n".to_vec());
}

// ---------------------------------------------------------------- random edits

const FILES: &[&str] = &[
    "a.txt",
    "b.txt",
    "d/c.txt",
    "d/sub/e.txt",
    "README.md",
    "src/lib.rs",
    "bin/run.sh",
    "link",
    "empty.txt",
];
const LINKS: &[&str] = &["README.md", "a.txt", "nowhere", "../x", "d/c.txt"];
const DIRS: &[(&str, &str)] = &[("d", "d2"), ("src", "src2")];

#[derive(Clone, Debug)]
enum Op {
    Write { f: usize, off: u64, data: Vec<u8> },
    Trunc { f: usize, len: u64, grow: u64 },
    Delete { f: usize },
    Rename { from: usize, to: usize },
    Chmod { f: usize },
    Link { f: usize, to: usize },
    MoveDir { d: usize },
}

fn op() -> impl Strategy<Value = Op> {
    let f = 0..FILES.len();
    prop_oneof![
        3 => (f.clone(), 0u64..48, prop::collection::vec(any::<u8>(), 0..24))
            .prop_map(|(f, off, data)| Op::Write { f, off, data }),
        1 => (f.clone(), 0u64..12, 0u64..12).prop_map(|(f, len, grow)| Op::Trunc { f, len, grow }),
        1 => f.clone().prop_map(|f| Op::Delete { f }),
        1 => (f.clone(), f.clone()).prop_map(|(from, to)| Op::Rename { from, to }),
        1 => f.clone().prop_map(|f| Op::Chmod { f }),
        1 => (f.clone(), 0..LINKS.len()).prop_map(|(f, to)| Op::Link { f, to }),
        1 => (0..DIRS.len()).prop_map(|d| Op::MoveDir { d }),
    ]
}

fn meta(p: &Path) -> Option<fs::Metadata> {
    fs::symlink_metadata(p).ok()
}

/// Applies `op` under `ws`; returns the absolute paths it touched.
fn apply(ws: &Path, op: &Op) -> Vec<PathBuf> {
    let at = |i: usize| ws.join(FILES[i]);
    let prepare = |p: &Path| -> Option<Vec<PathBuf>> {
        // A parent that is not a directory, or the path itself a directory: skip.
        let mut made = Vec::new();
        let mut q = p.parent().unwrap().to_path_buf();
        let mut chain = Vec::new();
        while q != ws {
            chain.push(q.clone());
            q = q.parent().unwrap().to_path_buf();
        }
        for d in chain.into_iter().rev() {
            match meta(&d) {
                Some(m) if m.is_dir() => {}
                Some(_) => return None,
                None => {
                    fs::create_dir(&d).unwrap();
                    made.push(d);
                }
            }
        }
        if meta(p).is_some_and(|m| m.is_dir()) {
            return None;
        }
        Some(made)
    };
    match op {
        Op::Write { f, off, data } => {
            let p = at(*f);
            let Some(mut touched) = prepare(&p) else {
                return vec![];
            };
            if meta(&p).is_some_and(|m| !m.is_file()) {
                fs::remove_file(&p).unwrap();
            }
            let mut h = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&p)
                .unwrap();
            h.seek(SeekFrom::Start(*off)).unwrap();
            h.write_all(data).unwrap();
            touched.push(p);
            touched
        }
        Op::Trunc { f, len, grow } => {
            let p = at(*f);
            if !meta(&p).is_some_and(|m| m.is_file()) {
                return vec![];
            }
            let h = fs::OpenOptions::new().write(true).open(&p).unwrap();
            h.set_len(*len).unwrap();
            h.set_len(len + grow).unwrap();
            vec![p]
        }
        Op::Delete { f } => {
            let p = at(*f);
            match meta(&p) {
                Some(m) if !m.is_dir() => {
                    fs::remove_file(&p).unwrap();
                    vec![p]
                }
                _ => vec![],
            }
        }
        Op::Rename { from, to } => {
            let (a, b) = (at(*from), at(*to));
            if from == to || !meta(&a).is_some_and(|m| !m.is_dir()) {
                return vec![];
            }
            let Some(mut touched) = prepare(&b) else {
                return vec![];
            };
            fs::rename(&a, &b).unwrap();
            touched.extend([a, b]);
            touched
        }
        Op::Chmod { f } => {
            let p = at(*f);
            match meta(&p) {
                Some(m) if m.is_file() => {
                    chmod(&p, if m.mode() & 0o100 != 0 { 0o644 } else { 0o755 });
                    vec![p]
                }
                _ => vec![],
            }
        }
        Op::Link { f, to } => {
            let p = at(*f);
            let Some(mut touched) = prepare(&p) else {
                return vec![];
            };
            if meta(&p).is_some() {
                fs::remove_file(&p).unwrap();
            }
            symlink(LINKS[*to], &p).unwrap();
            touched.push(p);
            touched
        }
        Op::MoveDir { d } => {
            let (a, b) = (ws.join(DIRS[*d].0), ws.join(DIRS[*d].1));
            let (ma, mb) = (meta(&a), meta(&b));
            let (from, to) = match (ma, mb) {
                (Some(m), None) if m.is_dir() => (a, b),
                (None, Some(m)) if m.is_dir() => (b, a),
                _ => return vec![],
            };
            fs::rename(&from, &to).unwrap();
            vec![from, to]
        }
    }
}

fn random_edits(cases: u32) {
    let w = World::new();
    let c0 = cp(&w, None);
    let mut runner = TestRunner::new(Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    });
    let strategy = prop::collection::vec(op(), 1..8);
    runner
        .run(&strategy, |ops| {
            // Back to the base state with a fresh, warm tracker.
            let _ = fs::remove_dir_all(temp(&w));
            try_rs(&w, id(&c0)).map_err(|e| TestCaseError::fail(format!("reset: {e:?}")))?;
            let mut t = tracker(&w);
            checkpoint(&w.ws_ref(), &w.base, Some(&mut t), manual())
                .map_err(|e| TestCaseError::fail(format!("warm: {e:?}")))?;
            let mut snaps = Vec::new();
            for op in &ops {
                let touched = apply(&w.ws, op);
                t.inbox().note(&WatchEvent::Changed(touched));
                let s = compute(&w.ws_ref(), &w.base, &mut t, Some(&[]))
                    .map_err(|e| TestCaseError::fail(format!("compute: {e:?}")))?;
                prop_assert_eq!(&s.overlay, &cold(&w), "incremental == cold after {:?}", op);
                let c = checkpoint(&w.ws_ref(), &w.base, Some(&mut t), manual())
                    .map_err(|e| TestCaseError::fail(format!("checkpoint: {e:?}")))?;
                prop_assert_eq!(Overlay::from_checkpoint(&c.checkpoint), cold(&w));
                snaps.push((c.checkpoint.id, files(&w.ws)));
            }
            for (cid, snap) in snaps.iter().rev() {
                try_rs(&w, *cid).map_err(|e| TestCaseError::fail(format!("restore: {e:?}")))?;
                prop_assert_eq!(&files(&w.ws), snap, "restore of {:?}", cid);
                prop_assert!(!temp(&w).exists());
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn random_edits_incremental_equals_cold_and_restores_exact() {
    random_edits(64);
}

#[test]
#[ignore = "long: 2048 cases"]
fn random_edits_incremental_equals_cold_and_restores_exact_long() {
    random_edits(2048);
}
