mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use strata_types::{Code, RepoPath};

fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

fn names(info: &strata_gitio::IndexInfo) -> Vec<String> {
    info.tracked.iter().map(|p| p.as_str().to_string()).collect()
}

#[test]
fn follows_a_gitdir_pointer_file() {
    let sb = Sandbox::new();
    let work = sb.path("work");
    let sep = sb.path("sep.git");
    fs::create_dir_all(&work).unwrap();
    sb.git(&work, &["init", "-q", "--separate-git-dir", sep.to_str().unwrap()]);
    sb.commit_file(&work, "a.txt", b"a\n", "a");
    sb.git(&work, &["config", "core.ignorecase", "true"]);
    assert!(work.join(".git").is_file());
    let git = sb.open(&work);
    let info = git.read_index(&work).unwrap();
    assert_eq!(names(&info), ["a.txt"]);
    assert!(info.ignorecase);
    assert!(info.is_tracked(&rp("A.TXT")));
}

#[test]
fn linked_worktree_uses_the_common_config_and_its_own_index() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.git(&main, &["config", "core.ignorecase", "true"]);
    let wt = sb.path("wt");
    sb.git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    sb.write(&wt, "only-in-wt.txt", b"x\n");
    sb.git(&wt, &["add", "only-in-wt.txt"]);
    let git = sb.open(&main);
    let info = git.read_index(&wt).unwrap();
    assert_eq!(names(&info), ["README.md", "only-in-wt.txt"]);
    assert!(info.ignorecase);
    assert_eq!(names(&git.read_index(&main).unwrap()), ["README.md"]);

    // config.worktree without the extension does nothing.
    let admin = PathBuf::from(sb.git(&wt, &["rev-parse", "--absolute-git-dir"]));
    fs::write(admin.join("config.worktree"), "[core]\n\tignorecase = false\n").unwrap();
    assert!(git.read_index(&wt).unwrap().ignorecase);
    // With extensions.worktreeConfig it overrides.
    sb.git(&main, &["config", "extensions.worktreeConfig", "true"]);
    assert!(!git.read_index(&wt).unwrap().ignorecase);
    assert!(git.read_index(&main).unwrap().ignorecase);

    // A strata workspace: the shadow's own core.ignorecase, index in the
    // admin dir worktrees/<n>/index.
    let tree = sb.tree_of(&main, "HEAD");
    let (shadow, ws) = fake_shadow(&sb, &main, "s", &tree, true);
    sb.git(&shadow, &["config", "core.ignorecase", "false"]);
    let info = git.read_index(&ws).unwrap();
    assert_eq!(names(&info), ["README.md"]);
    assert!(!info.ignorecase);
    sb.git(&shadow, &["config", "core.ignorecase", "true"]);
    assert!(git.read_index(&ws).unwrap().ignorecase);
}

#[test]
fn read_index_without_git_is_an_error() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let git = sb.open(&main);
    let plain = sb.path("plain");
    fs::create_dir_all(&plain).unwrap();
    assert_eq!(git.read_index(&plain).unwrap_err().code, Code::NoSuchPath);

    let fresh = sb.path("fresh");
    fs::create_dir_all(&fresh).unwrap();
    sb.git(&fresh, &["init", "-q"]);
    let g2 = sb.open(&fresh);
    let e = g2.read_index(&fresh).unwrap_err();
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(reason(&e).as_deref(), Some("no-index"));
}

#[test]
fn dot_git_symlink_is_unknown() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let git = sb.open(&main);
    let ws = sb.path("ws");
    fs::create_dir_all(&ws).unwrap();
    std::os::unix::fs::symlink(main.join(".git"), ws.join(".git")).unwrap();
    assert_eq!(git.read_index(&ws).unwrap_err().code, Code::Unknown);
}

#[test]
fn non_utf8_index_path_is_not_text() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let blob = sb.git_stdin(&main, &["hash-object", "-w", "--stdin"], b"x\n");
    let mut line = format!("100644 {blob} 0\t").into_bytes();
    line.extend_from_slice(b"\xff.txt\n");
    sb.git_stdin(&main, &["update-index", "--add", "--index-info"], &line);
    let git = sb.open(&main);
    assert_eq!(git.read_index(&main).unwrap_err().code, Code::NotText);
}

#[test]
fn sparse_index_is_unsupported() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.commit_file(&main, "a/x.txt", b"x\n", "a");
    sb.commit_file(&main, "b/y.txt", b"y\n", "b");
    sb.git(&main, &["sparse-checkout", "set", "--cone", "--sparse-index", "a"]);
    let git = sb.open(&main);
    assert_eq!(git.read_index(&main).unwrap_err().code, Code::Unsupported);
}

#[test]
fn read_index_snapshot_is_immutable_and_cached() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let git = sb.open(&main);
    let first = git.read_index(&main).unwrap();
    sb.write(&main, "new.txt", b"n\n");
    sb.git(&main, &["add", "new.txt"]);
    assert_eq!(names(&first), ["README.md"]);
    let second = git.read_index(&main).unwrap();
    assert_eq!(names(&second), ["README.md", "new.txt"]);

    // Same-length garbage in place, mtime restored → cached snapshot.
    let index = main.join(".git/index");
    let meta = fs::metadata(&index).unwrap();
    let mtime = meta.modified().unwrap();
    let len = meta.len() as usize;
    {
        use std::io::{Seek, Write};
        let mut f = fs::OpenOptions::new().write(true).open(&index).unwrap();
        f.seek(std::io::SeekFrom::Start(0)).unwrap();
        f.write_all(&vec![b'Z'; len]).unwrap();
        f.set_modified(mtime).unwrap();
    }
    assert_eq!(git.read_index(&main).unwrap(), second);
    // mtime changes → re-parsed → garbage is Unknown.
    let f = fs::OpenOptions::new().write(true).open(&index).unwrap();
    f.set_modified(mtime + std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(git.read_index(&main).unwrap_err().code, Code::Unknown);
    assert_eq!(names(&second), ["README.md", "new.txt"]);
}

#[test]
fn read_index_refuses_worktree_of_another_repo() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let other = sb.init_repo("other");
    let git = sb.open(&main);
    assert_eq!(git.read_index(&other).unwrap_err().code, Code::InvalidInput);
    // A shadow-looking repository whose alternate is another object store.
    let tree = sb.tree_of(&main, "HEAD");
    let (_, ws) = fake_shadow(&sb, &main, "foreign", &tree, false);
    assert_eq!(git.read_index(&ws).unwrap_err().code, Code::InvalidInput);
    // Ours is fine.
    let (_, ws) = fake_shadow(&sb, &main, "ours", &tree, true);
    assert_eq!(names(&git.read_index(&ws).unwrap()), ["README.md"]);
}

fn ls_files(sb: &Sandbox, repo: &Path) -> Vec<String> {
    let o = sb.git_output(repo, &["ls-files", "-z"]);
    assert!(o.status.success());
    o.stdout.split(|&c| c == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8(s.to_vec()).unwrap()).collect()
}

#[test]
fn reads_the_split_indexes_git_writes() {
    for version in ["2", "4"] {
        let sb = Sandbox::new();
        let main = sb.init_repo("main");
        sb.git(&main, &["config", "splitIndex.maxPercentChange", "100"]);
        sb.git(&main, &["config", "splitIndex.sharedIndexExpire", "never"]);
        for i in 0..30 {
            sb.write(&main, &format!("dir{}/f{i:02}.txt", i % 3), format!("{i}\n").as_bytes());
        }
        sb.git(&main, &["add", "."]);
        sb.git(&main, &["commit", "-q", "-m", "files"]);
        sb.git(&main, &["update-index", "--index-version", version]);
        sb.git(&main, &["update-index", "--split-index"]);
        let shared: Vec<_> = fs::read_dir(main.join(".git"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("sharedindex."))
            .collect();
        assert!(!shared.is_empty(), "git wrote no shared index");
        // delete, replace, add
        sb.git(&main, &["rm", "-q", "--cached", "dir0/f00.txt"]);
        sb.write(&main, "dir1/f01.txt", b"changed\n");
        sb.git(&main, &["add", "dir1/f01.txt"]);
        sb.write(&main, "added.txt", b"new\n");
        sb.git(&main, &["add", "added.txt"]);
        let git = sb.open(&main);
        let info = git.read_index(&main).unwrap();
        assert_eq!(names(&info), ls_files(&sb, &main), "version {version}");
        assert!(!info.is_tracked(&rp("dir0/f00.txt")));
        assert!(info.is_tracked(&rp("added.txt")));
    }
}
