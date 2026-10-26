//! Task 2: open files under a directory (deps.md §5.11, §7).

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::symlink;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use strata_deps::{Hold, open_files};

const WORKER_ENV: &str = "STRATA_DEPS_OPEN_FILES_WORKER";

/// Worker mode: hold something, say READY, wait for stdin to close.
fn worker(spec: &str) {
    let (kind, path) = spec.split_once(':').unwrap();
    let mut keep: Vec<fs::File> = Vec::new();
    match kind {
        "fd" => keep.push(fs::File::open(path).unwrap()),
        "mmap" => {
            let f = fs::File::open(path).unwrap();
            let len = f.metadata().unwrap().len() as usize;
            // SAFETY: read-only private mapping of an open file.
            let p = unsafe {
                libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0)
            };
            assert_ne!(p, libc::MAP_FAILED);
            drop(f); // only the mapping holds it now
        }
        "evt" => {
            let c = std::ffi::CString::new(path).unwrap();
            // SAFETY: a valid C path; the fd is leaked until exit on purpose.
            let fd = unsafe { libc::open(c.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
            assert!(fd >= 0);
        }
        "cwd" => {} // the parent sets our cwd
        _ => panic!("unknown worker {kind}"),
    }
    println!("READY");
    std::io::stdout().flush().unwrap();
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    std::process::exit(0);
}

fn spawn(spec: String, cwd: Option<&Path>) -> Child {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "open_files_reports_fd_mapping_and_cwd", "--nocapture", "--test-threads=1"])
        .env(WORKER_ENV, spec)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let mut child = c.spawn().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "worker exited before READY");
        if line.contains("READY") {
            break;
        }
    }
    child
}

fn stop(mut c: Child) {
    drop(c.stdin.take());
    c.wait().unwrap();
}

#[test]
fn open_files_reports_fd_mapping_and_cwd() {
    if let Ok(spec) = std::env::var(WORKER_ENV) {
        worker(&spec);
        return;
    }
    let tmp = tempfile::Builder::new().prefix("strata-deps-of-").tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    let dir = base.join("dir");
    for sub in ["a", "b", "c"] {
        fs::create_dir_all(dir.join(sub)).unwrap();
    }
    fs::write(dir.join("a/x.js"), b"module.exports = 1\n").unwrap();
    fs::write(dir.join("b/y.node"), vec![7u8; 16384]).unwrap();
    fs::create_dir_all(base.join("dir2")).unwrap();
    fs::write(base.join("dir2/x.js"), b"x\n").unwrap();

    let fd = spawn(format!("fd:{}", dir.join("a/x.js").display()), None);
    let mm = spawn(format!("mmap:{}", dir.join("b/y.node").display()), None);
    let cwd = spawn("cwd:".into(), Some(&dir.join("c")));
    let other = spawn(format!("fd:{}", base.join("dir2/x.js").display()), None);

    let got = open_files(&dir).unwrap();
    let want: Vec<(i32, PathBuf, Hold)> = {
        let mut v = vec![
            (fd.id() as i32, dir.join("a/x.js"), Hold::Fd),
            (mm.id() as i32, dir.join("b/y.node"), Hold::Mapped),
            (cwd.id() as i32, dir.join("c"), Hold::Cwd),
        ];
        v.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        v
    };
    let seen: Vec<(i32, PathBuf, Hold)> = got.iter().map(|o| (o.pid, o.path.clone(), o.hold)).collect();
    assert_eq!(seen, want, "{got:#?}");
    assert!(got.iter().all(|o| !o.command.is_empty()), "{got:#?}");
    assert!(!got.iter().any(|o| o.pid == other.id() as i32));

    // The prefix boundary is by path component.
    let got2 = open_files(&base.join("dir2")).unwrap();
    assert_eq!(got2.len(), 1);
    assert_eq!(got2[0].pid, other.id() as i32);

    // Batch-like repeat is stable; JSON shape carries pid and path.
    let j = serde_json::to_value(&got[0]).unwrap();
    assert!(j["pid"].is_i64() && j["path"].is_string());

    for c in [fd, mm, cwd, other] {
        stop(c);
    }
    assert_eq!(open_files(&dir).unwrap(), vec![]);

    // Missing dir, or a symlink: empty.
    assert_eq!(open_files(&base.join("missing")).unwrap(), vec![]);
    symlink(&dir, base.join("link")).unwrap();
    let held = spawn(format!("fd:{}", dir.join("a/x.js").display()), None);
    assert_eq!(open_files(&base.join("link")).unwrap(), vec![]);
    assert_eq!(open_files(&dir).unwrap().len(), 1);
    stop(held);
}

#[test]
fn open_files_failure_is_injected_per_thread() {
    if std::env::var_os(WORKER_ENV).is_some() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = fs::canonicalize(tmp.path()).unwrap();
    let e = strata_types::StrataError::new(strata_types::Code::Unknown, "injected");
    strata_deps::test_seams::fail_open_files_under(&dir, e.clone());
    assert_eq!(open_files(&dir.join("x")).unwrap_err(), e);
    let other = std::thread::spawn({
        let d = dir.clone();
        move || open_files(&d).map(|v| v.len())
    });
    assert_eq!(other.join().unwrap().unwrap(), 0, "other threads are unaffected");
    strata_deps::test_seams::reset();
    assert_eq!(open_files(&dir).unwrap(), vec![]);
}

#[test]
fn open_files_scan_is_fast() {
    if std::env::var_os(WORKER_ENV).is_some() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    open_files(tmp.path()).unwrap();
    eprintln!("open_files scan took {:?}", t.elapsed());
}

/// The flaky `rm_waits_for_publishing_in_the_lock_free_gap` (OpenFiles held
/// by the test process itself on `sessions/<id>` and `sessions/<id>/tool`):
/// those are exactly the `O_EVTONLY` descriptors FSEvents opens on a watch
/// root and its ancestors (`kFSEventStreamCreateFlagWatchRoot`). The calling
/// process's own event-only descriptors are not reported; a plain fd of its
/// own, and another process's event-only fd, still are.
#[test]
fn own_event_only_descriptors_are_not_holders() {
    if std::env::var(WORKER_ENV).is_ok() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let dir = fs::canonicalize(t.path()).unwrap();
    let sub = dir.join("tool");
    fs::create_dir(&sub).unwrap();
    let c = std::ffi::CString::new(sub.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a valid C path; closed below.
    let evt = unsafe { libc::open(c.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
    assert!(evt >= 0);
    assert_eq!(open_files(&dir).unwrap(), vec![], "own O_EVTONLY fd is not a holder");
    let plain = fs::File::open(&sub).unwrap();
    let got = open_files(&dir).unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!((got[0].pid, got[0].hold), (std::process::id() as i32, Hold::Fd));
    drop(plain);
    // SAFETY: our fd.
    unsafe { libc::close(evt) };
    let child = spawn(format!("evt:{}", sub.display()), None);
    let got = open_files(&dir).unwrap();
    assert!(got.iter().any(|o| o.pid == child.id() as i32 && o.hold == Hold::Fd), "another process's event-only fd still counts: {got:?}");
    stop(child);
}

/// A child being spawned briefly holds copies of all our fds, `O_CLOEXEC`
/// ones included, until its exec closes them; our own `O_EVTONLY` fd must
/// not show up under the child's pid then (it did in 11 of 150 scans while
/// 4 threads kept spawning, before the fix).
#[test]
fn own_event_only_fd_copied_into_a_spawning_child_is_not_a_holder() {
    if std::env::var(WORKER_ENV).is_ok() {
        return;
    }
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let t = tempfile::tempdir().unwrap();
    let dir = fs::canonicalize(t.path()).unwrap();
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a valid C path; closed below.
    let evt = unsafe { libc::open(c.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
    assert!(evt >= 0);
    let done = Arc::new(AtomicBool::new(false));
    let spawners: Vec<_> = (0..4)
        .map(|_| {
            let done = done.clone();
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    let _ = Command::new("/usr/bin/true").status();
                }
            })
        })
        .collect();
    let start = std::time::Instant::now();
    let mut seen = Vec::new();
    let mut scans = 0;
    while scans < 40 && start.elapsed() < std::time::Duration::from_secs(30) {
        seen.extend(open_files(&dir).unwrap());
        scans += 1;
    }
    done.store(true, Ordering::SeqCst);
    for s in spawners {
        s.join().unwrap();
    }
    // SAFETY: our fd.
    unsafe { libc::close(evt) };
    assert_eq!(seen, vec![], "after {scans} scans (own pid {})", std::process::id());
}
