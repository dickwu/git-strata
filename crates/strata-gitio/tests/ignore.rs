mod common;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use common::*;
use strata_gitio::Git;
use strata_types::{Code, RepoPath, StrataError};

fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

fn ig(git: &Git, wt: &Path, p: &str) -> bool {
    git.is_ignored(wt, &rp(p)).unwrap_or_else(|e| panic!("is_ignored {p}: {e:?}"))
}

fn ig_err(git: &Git, wt: &Path, p: &str) -> StrataError {
    match git.is_ignored(wt, &rp(p)) {
        Ok(v) => panic!("is_ignored {p} answered {v}, expected an error"),
        Err(e) => e,
    }
}

/// `git check-ignore --no-index -q`: the rules alone.
fn git_rules_ignore(sb: &Sandbox, wt: &Path, p: &str) -> bool {
    let st = sb.cmd(wt).args(["check-ignore", "--no-index", "-q", "--", p]).status().unwrap();
    match st.code() {
        Some(0) => true,
        Some(1) => false,
        c => panic!("check-ignore {p}: {c:?}"),
    }
}

/// Whether `git status --porcelain --ignored` reports `p` as ignored.
fn status_ignored(sb: &Sandbox, wt: &Path, p: &str) -> bool {
    let out = sb.git_output(wt, &["status", "--porcelain", "--ignored", "--untracked-files=all", "--", p]);
    String::from_utf8_lossy(&out.stdout).lines().any(|l| l.starts_with("!! "))
}

fn mkfifo(p: &Path) {
    let _ = fs::remove_file(p);
    let _ = fs::remove_dir_all(p);
    let st = std::process::Command::new("mkfifo").arg(p).status().unwrap();
    assert!(st.success());
}

fn symlink(target: &Path, link: &Path) {
    let _ = fs::remove_file(link);
    let _ = fs::remove_dir_all(link);
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Run `f` on another thread; fail if it does not return within 5 s.
fn watchdog<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(5)).expect("call did not return within 5 s")
}

fn ig_wd(git: &Git, wt: &Path, p: &str) -> strata_gitio::Result<bool> {
    let (g, w, p) = (git.clone(), wt.to_path_buf(), rp(p));
    watchdog(move || g.is_ignored(&w, &p))
}

fn ri_wd(git: &Git, wt: &Path) -> strata_gitio::Result<strata_gitio::IndexInfo> {
    let (g, w) = (git.clone(), wt.to_path_buf());
    watchdog(move || g.read_index(&w))
}

fn set_ignorecase(sb: &Sandbox, repo: &Path, v: bool) {
    sb.git(repo, &["config", "core.ignorecase", if v { "true" } else { "false" }]);
}

fn info_exclude(repo: &Path) -> PathBuf {
    repo.join(".git/info/exclude")
}

fn write_abs(p: &Path, content: &str) {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(p, content).unwrap();
}

#[test]
fn index_parse_tracked_never_ignored() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, true);
    sb.write(&main, ".gitignore", b"QLogs\n*.txt\n");
    sb.write(&main, "Kept.TXT", b"k\n");
    sb.git(&main, &["add", "-f", ".gitignore", "Kept.TXT"]);
    sb.git(&main, &["commit", "-qm", "kept"]);
    sb.write(&main, "qlogs/a.log", b"x\n");
    sb.write(&main, "Gone.TXT", b"x\n");
    let git = sb.open(&main);
    assert!(ig(&git, &main, "qlogs/a.log"));
    assert!(ig(&git, &main, "qlogs"));
    assert!(ig(&git, &main, "Gone.TXT"));
    assert!(!ig(&git, &main, "Kept.TXT"));
    assert!(!ig(&git, &main, "kept.txt"));
    assert!(!status_ignored(&sb, &main, "Kept.TXT"));
    assert!(status_ignored(&sb, &main, "Gone.TXT"));

    // Case-sensitive: a tracked file hit literally by a rule stays visible.
    let cs = sb.init_repo("cs");
    set_ignorecase(&sb, &cs, false);
    sb.write(&cs, ".gitignore", b"exact.txt\n");
    sb.write(&cs, "exact.txt", b"e\n");
    sb.git(&cs, &["add", "-f", ".gitignore", "exact.txt"]);
    sb.git(&cs, &["commit", "-qm", "exact"]);
    let git = sb.open(&cs);
    assert!(!ig(&git, &cs, "exact.txt"));
    assert!(!status_ignored(&sb, &cs, "exact.txt"));
}

#[test]
fn untracking_a_file_lets_rules_apply() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.write(&main, ".gitignore", b"*.txt\n");
    sb.write(&main, "TRACKED.txt", b"t\n");
    sb.git(&main, &["add", "-f", ".gitignore", "TRACKED.txt"]);
    sb.git(&main, &["commit", "-qm", "t"]);
    let git = sb.open(&main);
    assert!(!ig(&git, &main, "TRACKED.txt"));
    sb.git(&main, &["rm", "-q", "--cached", "TRACKED.txt"]);
    assert!(ig(&git, &main, "TRACKED.txt"));
    fs::write(main.join(".git/index"), b"DIRC garbage").unwrap();
    let e = ig_err(&git, &main, "TRACKED.txt");
    assert_eq!(e.code, Code::Unknown, "{e:?}");
}

#[test]
fn case_sensitive_repo_matches_exactly() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, false);
    sb.write(&main, ".gitignore", b"*.txt\n");
    let git = sb.open(&main);
    assert!(!ig(&git, &main, "Gone.TXT"));
    assert!(ig(&git, &main, "gone.txt"));
    // Asking again (a walk asks path by path) gives the same answers.
    for _ in 0..3 {
        assert!(!ig(&git, &main, "Gone.TXT"));
        assert!(ig(&git, &main, "gone.txt"));
    }
    assert!(!git_rules_ignore(&sb, &main, "Gone.TXT"));
}

#[test]
fn ascii_only_case_folding_like_git() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, true);
    sb.write(&main, ".gitignore", "É.txt\n".as_bytes());
    let git = sb.open(&main);
    assert!(!ig(&git, &main, "é.txt"));
    assert!(ig(&git, &main, "É.txt"));
    assert!(!git_rules_ignore(&sb, &main, "é.txt"));
    assert!(git_rules_ignore(&sb, &main, "É.txt"));
}

#[test]
fn root_gitignore_file_and_dir_patterns() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.write(&main, ".gitignore", b"*.log\ntarget/\n");
    sb.write(&main, "target/out", b"x\n");
    sb.write(&main, "sub/target", b"a file\n");
    sb.write(&main, "src/main.rs", b"fn main() {}\n");
    let git = sb.open(&main);
    assert!(ig(&git, &main, "a.log"));
    assert!(ig(&git, &main, "deep/x.log"));
    assert!(ig(&git, &main, "target"));
    assert!(ig(&git, &main, "target/out"));
    assert!(!ig(&git, &main, "sub/target"));
    assert!(!ig(&git, &main, "src/main.rs"));
    assert!(!ig(&git, &main, "src"));
}

#[test]
fn self_ignored_gitignore_still_applies() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.write(&main, "build/.gitignore", b"*\n");
    sb.write(&main, "build/artifact.o", b"o\n");
    let git = sb.open(&main);
    assert!(ig(&git, &main, "build/.gitignore"));
    assert!(ig(&git, &main, "build/artifact.o"));
    assert!(!ig(&git, &main, "build"));
    assert!(!ig(&git, &main, "artifact.o"));
}

#[test]
fn malformed_ignore_line_skips_only_itself() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.write(&main, ".gitignore", b"[\n*.log\n[unclosed\n*.tmp\n");
    let git = sb.open(&main);
    assert!(ig(&git, &main, "a.log"));
    assert!(ig(&git, &main, "a.tmp"));
    assert!(!ig(&git, &main, "["));
    assert!(!git_rules_ignore(&sb, &main, "["));
}

#[test]
fn no_rules_ignores_nothing() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let _ = fs::remove_file(info_exclude(&main));
    assert!(!sb.xdg.join("git/ignore").exists());
    sb.write(&main, "dir/a.log", b"x\n");
    let git = sb.open(&main);
    for p in ["a.log", "dir", "dir/a.log", ".env", "x/y/z", "README.md"] {
        assert!(!ig(&git, &main, p), "{p}");
    }
}

#[test]
fn info_exclude_applies_through_worktree_gitfile() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    write_abs(&info_exclude(&main), "*.secret\n");
    let tree = sb.tree_of(&main, "HEAD");
    let (shadow, ws) = fake_shadow(&sb, &main, "s", &tree, true);
    // The shadow mirrors the main checkout's info/exclude once, at creation.
    fs::copy(info_exclude(&main), shadow.join("info/exclude")).unwrap();
    let git = sb.open(&main);
    assert!(ig(&git, &ws, "x.secret"));
    write_abs(&info_exclude(&main), "*.other\n");
    assert!(ig(&git, &ws, "x.secret"));
    assert!(!ig(&git, &ws, "x.other"));
    assert!(ig(&git, &main, "x.other"));
    fs::remove_file(shadow.join("info/exclude")).unwrap();
    assert!(!ig(&git, &ws, "x.secret"));
}

#[test]
fn relative_commondir_resolves_against_gitdir() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let tree = sb.tree_of(&main, "HEAD");
    // ws/../.. is sb.root/a: give it a decoy info/exclude.
    let ws = sb.path("a/b/ws");
    write_abs(&sb.path("a/info/exclude"), "*.decoy\n");
    let (shadow, ws) = fake_shadow_at(&sb, &main, "rel", &tree, true, &ws);
    write_abs(&shadow.join("info/exclude"), "*.rel\n");
    let git = sb.open(&main);
    assert!(ig(&git, &ws, "x.rel"));
    assert!(!ig(&git, &ws, "x.decoy"));
}

#[test]
fn global_excludes_file_is_honoured() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let xdg2 = sb.path("xdg2");
    write_abs(&xdg2.join("git/ignore"), "*.xdg\n");
    write_abs(&sb.home.join(".config/git/ignore"), "*.home\n");
    // XDG_CONFIG_HOME set: its git/ignore.
    let env: Vec<(&str, Option<&OsStr>)> = vec![
        ("HOME", Some(sb.home.as_os_str())),
        ("XDG_CONFIG_HOME", Some(xdg2.as_os_str())),
        ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
    ];
    let git = Git::open_with_env(&main, &env).unwrap();
    assert!(ig(&git, &main, "a.xdg"));
    assert!(!ig(&git, &main, "a.home"));
    // Unset: ~/.config/git/ignore.
    let env: Vec<(&str, Option<&OsStr>)> = vec![
        ("HOME", Some(sb.home.as_os_str())),
        ("XDG_CONFIG_HOME", None),
        ("GIT_CONFIG_NOSYSTEM", Some(OsStr::new("1"))),
    ];
    let git = Git::open_with_env(&main, &env).unwrap();
    assert!(!ig(&git, &main, "a.xdg"));
    assert!(ig(&git, &main, "a.home"));

    // core.excludesFile set in the repository: that file; the shadow copies
    // the key and the workspace resolves the same file.
    let core = sb.path("core-excludes");
    write_abs(&core, "*.core\n.claude/settings.local.json\n");
    sb.git(&main, &["config", "core.excludesFile", core.to_str().unwrap()]);
    let tree = sb.tree_of(&main, "HEAD");
    let (shadow, ws) = fake_shadow(&sb, &main, "g", &tree, true);
    sb.git(&shadow, &["config", "core.excludesFile", core.to_str().unwrap()]);
    sb.write(&main, ".claude/settings.local.json", b"{}\n");
    sb.write(&ws, ".claude/settings.local.json", b"{}\n");
    let git = sb.open(&main);
    for wt in [&main, &ws] {
        assert!(ig(&git, wt, "a.core"));
        assert!(!ig(&git, wt, "a.home"));
        assert!(ig(&git, wt, ".claude/settings.local.json"));
        assert!(!ig(&git, wt, ".claude/other.json"));
        assert!(git_rules_ignore(&sb, wt, ".claude/settings.local.json"));
    }
}

#[test]
fn local_negation_overrides_global_excludes() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    write_abs(&sb.xdg.join("git/ignore"), "*.log\n*.env\n");
    write_abs(&info_exclude(&main), "!keep2.log\n!a.env\n*.ie\n");
    sb.write(&main, ".gitignore", b"!keep.log\n*.env\n!b.ie\n");
    let git = sb.open(&main);
    assert!(!ig(&git, &main, "keep.log"));
    assert!(!ig(&git, &main, "keep2.log"));
    assert!(ig(&git, &main, "x.log"));
    // .gitignore > info/exclude > global.
    assert!(ig(&git, &main, "a.env"));
    assert!(!ig(&git, &main, "b.ie"));
    assert!(ig(&git, &main, "c.ie"));
    for p in ["keep.log", "keep2.log", "x.log", "a.env", "b.ie", "c.ie"] {
        assert_eq!(ig(&git, &main, p), git_rules_ignore(&sb, &main, p), "{p}");
    }
}

#[test]
fn deeper_rules_override_shallower() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.write(&main, ".gitignore", b"*.log\n");
    sb.write(&main, "pkg/.gitignore", b"!keep.log\n");
    sb.write(&main, "a/b/.gitignore", b"!important.log\n");
    let git = sb.open(&main);
    assert!(!ig(&git, &main, "pkg/keep.log"));
    assert!(ig(&git, &main, "keep.log"));
    assert!(ig(&git, &main, "pkg/x.log"));
    assert!(!ig(&git, &main, "a/b/important.log"));
    assert!(!ig(&git, &main, "a/b/c/important.log"));
    assert!(ig(&git, &main, "a/important.log"));
}

#[test]
fn nested_gitignore_scoped_to_its_directory() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.write(&main, ".gitignore", b"*.log\n");
    sb.write(&main, "lib/.gitignore", b"*.c\n");
    sb.write(&main, "all/.gitignore", b"*\n");
    sb.write(&main, "src/.gitignore", b"*.tmp\n");
    let git = sb.open(&main);
    assert!(ig(&git, &main, "lib/x.c"));
    assert!(ig(&git, &main, "lib/deep/x.c"));
    assert!(!ig(&git, &main, "x.c"));
    assert!(!ig(&git, &main, "other/x.c"));
    assert!(ig(&git, &main, "all/a"));
    assert!(!ig(&git, &main, "a"));
    assert!(ig(&git, &main, "src/a.log"));
    assert!(ig(&git, &main, "src/a.tmp"));
    assert!(!ig(&git, &main, "a.tmp"));
}

#[test]
fn gitignore_above_worktree_root_is_not_applied() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    let tree = sb.tree_of(&main, "HEAD");
    let session = sb.path("root/sessions/sid");
    write_abs(&session.join(".gitignore"), "*\n");
    write_abs(&sb.path("root/sessions/.gitignore"), "*.x\n");
    let (_, ws) = fake_shadow_at(&sb, &main, "above", &tree, true, &session.join("ws"));
    let git = sb.open(&main);
    assert!(!ig(&git, &ws, "x.x"));
    assert!(!ig(&git, &ws, "anything"));
}

#[test]
fn negation_cannot_reinclude_under_excluded_dir() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.write(&main, ".gitignore", b"build/\n!build/keep.c\n");
    sb.write(&main, "build/keep.c", b"c\n");
    let git = sb.open(&main);
    assert!(ig(&git, &main, "build/keep.c"));
    assert!(git_rules_ignore(&sb, &main, "build/keep.c"));
    assert!(status_ignored(&sb, &main, "build/keep.c"));
}

#[test]
fn unreadable_index_is_an_error_not_empty() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    sb.write(&main, ".gitignore", b"*.log\n");
    let git = sb.open(&main);
    let index = main.join(".git/index");
    let good = fs::read(&index).unwrap();
    // No permission.
    fs::set_permissions(&index, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&index).is_err() {
        assert_eq!(ig_err(&git, &main, "a.log").code, Code::Unknown);
        assert_eq!(ig_err(&git, &main, "x.txt").code, Code::Unknown);
    }
    fs::set_permissions(&index, fs::Permissions::from_mode(0o644)).unwrap();
    // Truncated.
    fs::write(&index, &good[..good.len() / 2]).unwrap();
    assert_eq!(ig_err(&git, &main, "a.log").code, Code::Unknown);
    // Missing.
    fs::remove_file(&index).unwrap();
    let e = ig_err(&git, &main, "a.log");
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(reason(&e).as_deref(), Some("no-index"));
}

#[test]
fn rule_and_index_reads_never_hang_on_non_regular_files() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, true);
    let wt = sb.path("wt");
    sb.git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let git = sb.open(&main);

    // .gitignore as a symlink or a directory: no rules, like git.
    let rules = sb.path("rules-copy");
    write_abs(&rules, "*.log\n");
    for root in [&main, &wt] {
        symlink(&rules, &root.join(".gitignore"));
        assert!(!ig_wd(&git, root, "x.log").unwrap());
        assert!(!status_ignored(&sb, root, "x.log") || !root.join("x.log").exists());
        assert!(!git_rules_ignore(&sb, root, "x.log"));
        fs::remove_file(root.join(".gitignore")).unwrap();
        fs::create_dir(root.join(".gitignore")).unwrap();
        assert!(!ig_wd(&git, root, "x.log").unwrap());
        assert!(!git_rules_ignore(&sb, root, "x.log"));
        // A FIFO .gitignore: Unknown naming it.
        mkfifo(&root.join(".gitignore"));
        let e = ig_wd(&git, root, "x.log").unwrap_err();
        assert_eq!(e.code, Code::Unknown, "{e:?}");
        assert_eq!(detail(&e, "path").as_deref(), root.join(".gitignore").to_str());
        fs::remove_file(root.join(".gitignore")).unwrap();
    }

    // info/exclude as a FIFO, and as a symlink to a FIFO (resolved path).
    let ie = info_exclude(&main);
    mkfifo(&ie);
    let e = ig_wd(&git, &main, "x.log").unwrap_err();
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(detail(&e, "path").as_deref(), ie.to_str());
    assert_eq!(ig_wd(&git, &wt, "x.log").unwrap_err().code, Code::Unknown);
    let fifo = sb.path("a-fifo");
    mkfifo(&fifo);
    symlink(&fifo, &ie);
    let e = ig_wd(&git, &main, "x.log").unwrap_err();
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(detail(&e, "path").as_deref(), fifo.to_str());
    // info/exclude as a symlink to a regular file: its rules; dangling: none.
    let ie_file = sb.path("ie-file");
    write_abs(&ie_file, "*.ie\n*.LOG\n");
    symlink(&ie_file, &ie);
    assert!(ig_wd(&git, &main, "a.ie").unwrap());
    assert!(ig_wd(&git, &wt, "a.ie").unwrap());
    assert!(git_rules_ignore(&sb, &main, "a.ie"));
    // config as a symlink to a copy with ignorecase = false: *.LOG no
    // longer ignores x.log.
    assert!(ig_wd(&git, &main, "x.log").unwrap());
    let cfg = main.join(".git/config");
    let copy = sb.path("config-copy");
    fs::copy(&cfg, &copy).unwrap();
    sb.git(&sb.root, &["config", "--file", copy.to_str().unwrap(), "core.ignorecase", "false"]);
    let orig = sb.path("config-orig");
    fs::rename(&cfg, &orig).unwrap();
    symlink(&copy, &cfg);
    assert!(!ig_wd(&git, &main, "x.log").unwrap());
    assert!(!git_rules_ignore(&sb, &main, "x.log"));
    fs::remove_file(&cfg).unwrap();
    fs::rename(&orig, &cfg).unwrap();
    symlink(&sb.path("nowhere"), &ie);
    assert!(!ig_wd(&git, &main, "a.ie").unwrap());
    assert!(!git_rules_ignore(&sb, &main, "a.ie"));
    fs::remove_file(&ie).unwrap();

    // ~/.config/git/ignore as a FIFO → Unknown; as a symlink to a file → rules.
    let global = sb.xdg.join("git/ignore");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    mkfifo(&global);
    let e = ig_wd(&git, &main, "x.g").unwrap_err();
    assert_eq!(e.code, Code::Unknown);
    assert_eq!(detail(&e, "path").as_deref(), global.to_str());
    let gfile = sb.path("global-file");
    write_abs(&gfile, "*.g\n");
    symlink(&gfile, &global);
    assert!(ig_wd(&git, &main, "x.g").unwrap());
    assert!(git_rules_ignore(&sb, &main, "x.g"));
    fs::remove_file(&global).unwrap();

    // A path component of info/exclude being a regular file: no rules.
    let info_dir = main.join(".git/info");
    fs::remove_dir_all(&info_dir).unwrap();
    fs::write(&info_dir, b"not a dir").unwrap();
    assert!(!ig_wd(&git, &main, "a.ie").unwrap());
    fs::remove_file(&info_dir).unwrap();

    // The linked worktree's .git as a symlink to its gitfile, or a FIFO.
    let dotgit = wt.join(".git");
    let gitfile = sb.path("gitfile-copy");
    fs::copy(&dotgit, &gitfile).unwrap();
    symlink(&gitfile, &dotgit);
    assert_eq!(ri_wd(&git, &wt).unwrap_err().code, Code::Unknown);
    assert_eq!(ig_wd(&git, &wt, "x").unwrap_err().code, Code::Unknown);
    mkfifo(&dotgit);
    assert_eq!(ri_wd(&git, &wt).unwrap_err().code, Code::Unknown);
    assert_eq!(ig_wd(&git, &wt, "x").unwrap_err().code, Code::Unknown);
    fs::remove_file(&dotgit).unwrap();
    fs::copy(&gitfile, &dotgit).unwrap();
    assert!(ri_wd(&git, &wt).is_ok());

    // The main index as a FIFO, or a symlink to the real one.
    let index = main.join(".git/index");
    let saved = sb.path("index-saved");
    fs::rename(&index, &saved).unwrap();
    mkfifo(&index);
    assert_eq!(ri_wd(&git, &main).unwrap_err().code, Code::Unknown);
    symlink(&saved, &index);
    assert_eq!(ri_wd(&git, &main).unwrap_err().code, Code::Unknown);
    fs::remove_file(&index).unwrap();
    fs::rename(&saved, &index).unwrap();

    // Opened through a symlink to the main checkout: works.
    let link = sb.path("main-link");
    symlink(&main, &link);
    let via = sb.open(&link);
    assert!(ri_wd(&via, &link).is_ok());
    assert!(!ig_wd(&via, &link, "README.md").unwrap());

    // A workspace directory replaced by a symlink to the main checkout.
    let tree = sb.tree_of(&main, "HEAD");
    let (_, ws) = fake_shadow(&sb, &main, "h", &tree, true);
    fs::remove_dir_all(&ws).unwrap();
    symlink(&main, &ws);
    assert_eq!(ri_wd(&git, &ws).unwrap_err().code, Code::Unknown);
}

// ---------------------------------------------------------------------------
// corpus

struct Corpus {
    files: BTreeMap<String, String>,
    info: String,
    global: String,
    tracked: Vec<String>,
    paths: Vec<(String, bool)>,
}

fn load_corpus() -> Corpus {
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ignore-corpus/corpus.txt")).unwrap();
    let mut c = Corpus { files: BTreeMap::new(), info: String::new(), global: String::new(), tracked: vec![], paths: vec![] };
    let mut section = String::new();
    for line in text.split('\n') {
        if let Some(s) = line.strip_prefix("== ") {
            section = s.to_string();
            continue;
        }
        if section.is_empty() || (line.starts_with("##")) {
            continue;
        }
        match section.as_str() {
            "@info/exclude" => c.info += &format!("{line}\n"),
            "@global" => c.global += &format!("{line}\n"),
            "tracked" => {
                if !line.is_empty() {
                    c.tracked.push(line.to_string())
                }
            }
            "paths" => {
                if !line.is_empty() {
                    match line.strip_suffix('/') {
                        Some(d) => c.paths.push((d.to_string(), true)),
                        None => c.paths.push((line.to_string(), false)),
                    }
                }
            }
            f => *c.files.entry(f.to_string()).or_default() += &format!("{line}\n"),
        }
    }
    c
}

fn check_ignore_batch(sb: &Sandbox, wt: &Path, paths: &[&str]) -> std::collections::BTreeSet<String> {
    let mut child = sb
        .cmd(wt)
        .args(["check-ignore", "--no-index", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = Vec::new();
    for p in paths {
        input.extend_from_slice(p.as_bytes());
        input.push(0);
    }
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(matches!(out.status.code(), Some(0) | Some(1)));
    out.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8(s.to_vec()).unwrap()).collect()
}

fn corpus_round(ignorecase: bool) {
    let c = load_corpus();
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, ignorecase);
    for (f, body) in &c.files {
        sb.write(&main, f, body.as_bytes());
        sb.git(&main, &["add", "-f", "--", f]);
    }
    for t in &c.tracked {
        sb.write(&main, t, b"tracked\n");
        sb.git(&main, &["add", "-f", "--", t]);
    }
    sb.git(&main, &["commit", "-qm", "corpus"]);
    write_abs(&info_exclude(&main), &c.info);
    write_abs(&sb.xdg.join("git/ignore"), &c.global);

    let tree = sb.tree_of(&main, "HEAD");
    let (shadow, ws) = fake_shadow(&sb, &main, "corpus", &tree, true);
    set_ignorecase(&sb, &shadow, ignorecase);
    fs::copy(info_exclude(&main), shadow.join("info/exclude")).unwrap();
    sb.archive_tree_into(&main, "HEAD", &ws);

    let git = sb.open(&main);
    for wt in [&main, &ws] {
        for (p, is_dir) in &c.paths {
            if *is_dir {
                fs::create_dir_all(wt.join(p)).unwrap();
            }
        }
        let info = git.read_index(wt).unwrap();
        let untracked: Vec<&str> =
            c.paths.iter().map(|(p, _)| p.as_str()).filter(|p| !info.is_tracked(&rp(p))).collect();
        let by_git = check_ignore_batch(&sb, wt, &untracked);
        // The corpus must exercise both answers.
        assert!(by_git.len() > 30 && untracked.len() - by_git.len() > 20, "{by_git:?}");
        let mut diffs = Vec::new();
        for (p, _) in &c.paths {
            let ours = ig(&git, wt, p);
            if info.is_tracked(&rp(p)) {
                if ours || status_ignored(&sb, wt, p) {
                    diffs.push(format!("tracked {p:?}: ours={ours}"));
                }
            } else if ours != by_git.contains(p.as_str()) {
                diffs.push(format!("{p:?}: ours={ours} git={}", !ours));
            }
        }
        assert!(diffs.is_empty(), "ignorecase={ignorecase} in {}:\n{}", wt.display(), diffs.join("\n"));
    }
}

#[test]
fn is_ignored_agrees_with_git_on_corpus() {
    corpus_round(false);
    corpus_round(true);
}

// ---------------------------------------------------------------------------
// is_ignored_in_export

fn head_tree(sb: &Sandbox, repo: &Path) -> strata_types::TreeOid {
    strata_types::TreeOid::try_from(sb.tree_of(repo, "HEAD")).unwrap()
}

fn export_head(sb: &Sandbox, repo: &Path, rel: &str) -> PathBuf {
    let dir = sb.path(rel);
    sb.archive_tree_into(repo, "HEAD", &dir);
    dir
}

fn iie(git: &Git, dir: &Path, tree: &strata_types::TreeOid, p: &str) -> bool {
    git.is_ignored_in_export(dir, tree, &rp(p)).unwrap_or_else(|e| panic!("is_ignored_in_export {p}: {e:?}"))
}

#[test]
fn is_ignored_in_export_ignores_seeds_like_the_workspace() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, false);
    sb.commit_file(&main, "CLAUDE.md", b"# c\n", "claude");
    sb.commit_file(&main, ".claude/commands.md", b"x\n", "dot claude");
    write_abs(&info_exclude(&main), ".env\n");
    let home2 = sb.path("home2");
    write_abs(&home2.join(".config/git/ignore"), ".claude/settings.local.json\n");
    let git = sb.open(&main);
    git.set_env("HOME", Some(home2.as_os_str()));
    git.set_env("XDG_CONFIG_HOME", None);
    let tree = head_tree(&sb, &main);
    let dir = export_head(&sb, &main, "export");
    let before = snapshot(&dir);
    for (p, want) in [(".env", true), (".claude/settings.local.json", true), ("CLAUDE.md", false), (".claude/commands.md", false)] {
        assert_eq!(iie(&git, &dir, &tree, p), want, "{p}");
        assert_eq!(ig(&git, &main, p), want, "is_ignored on main {p}");
    }
    write_abs(&info_exclude(&main), "# nothing\n");
    assert!(!iie(&git, &dir, &tree, ".env"));
    assert!(!ig(&git, &main, ".env"));
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn is_ignored_in_export_never_ignores_a_tracked_path() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, false);
    sb.write(&main, ".gitignore", b"*.log\nlogs/\n");
    sb.write(&main, "logs/keep.log", b"k\n");
    sb.git(&main, &["add", "-f", ".gitignore", "logs/keep.log"]);
    sb.git(&main, &["commit", "-qm", "logs"]);
    let git = sb.open(&main);
    let tree = head_tree(&sb, &main);
    let dir = export_head(&sb, &main, "export");
    assert!(!iie(&git, &dir, &tree, "logs/keep.log"));
    assert!(iie(&git, &dir, &tree, "logs/other.log"));
    assert!(iie(&git, &dir, &tree, "logs"));
    assert_eq!(iie(&git, &dir, &tree, "logs"), ig(&git, &main, "logs"));
    // Case-sensitive repo: neither tracked nor matched by `*.log`/`logs/`.
    assert!(!iie(&git, &dir, &tree, "LOGS/KEEP.LOG"));
    assert!(iie(&git, &dir, &tree, "LOGS/KEEP.log") && iie(&git, &dir, &tree, "logs/KEEP.LOG"));
    set_ignorecase(&sb, &main, true);
    assert!(!iie(&git, &dir, &tree, "LOGS/KEEP.LOG"));
    assert!(!iie(&git, &dir, &tree, "logs/keep.log"));
    assert!(iie(&git, &dir, &tree, "logs/other.log"));

    let head = sb.head(&main);
    let b = sb.git_stdin(&main, &["hash-object", "-w", "--stdin"], b"x\n");
    let bad = sb.git_stdin(&main, &["mktree"], format!("160000 commit {head}\tsub\n100644 blob {b}\tf\n").as_bytes());
    let bad = strata_types::TreeOid::try_from(bad).unwrap();
    let e = git.is_ignored_in_export(&dir, &bad, &rp("f")).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");
}

#[test]
fn is_ignored_in_export_honours_negation() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, false);
    sb.commit_file(&main, ".gitignore", b"*.local\n!keep.local\n!keep.env\nbuild/\n!build/keep.c\n", "rules");
    write_abs(&info_exclude(&main), "*.env\n");
    let git = sb.open(&main);
    let tree = head_tree(&sb, &main);
    let dir = export_head(&sb, &main, "export");
    for (p, want) in [
        ("keep.local", false),
        ("x.local", true),
        ("keep.env", false),
        ("x.env", true),
        ("build/keep.c", true),
        ("build/other.c", true),
    ] {
        assert_eq!(iie(&git, &dir, &tree, p), want, "{p}");
        assert_eq!(ig(&git, &main, p), want, "is_ignored on main {p}");
    }
}

#[test]
fn is_ignored_in_export_reads_nested_gitignore_from_dir() {
    let sb = Sandbox::new();
    let main = sb.init_repo("main");
    set_ignorecase(&sb, &main, false);
    sb.commit_file(&main, "sub/.gitignore", b"secret.txt\n", "nested");
    sb.commit_file(&main, "CLAUDE.md", b"# c\n", "claude");
    // An uncommitted change in the main checkout does not reach the export.
    write_abs(&main.join(".gitignore"), "CLAUDE.md\n");
    let git = sb.open(&main);
    assert!(ig(&git, &main, "x/CLAUDE.md"));
    let tree = head_tree(&sb, &main);
    write_abs(&sb.path("exp/.gitignore"), "*\n");
    let dir = export_head(&sb, &main, "exp/d");
    let before = snapshot(&sb.path("exp"));
    for (p, want) in [
        ("sub/secret.txt", true),
        ("secret.txt", false),
        ("other/secret.txt", false),
        ("CLAUDE.md", false),
        ("x/CLAUDE.md", false),
        ("anything.txt", false),
    ] {
        assert_eq!(iie(&git, &dir, &tree, p), want, "{p}");
    }

    let e = git.is_ignored_in_export(&sb.path("exp/missing"), &tree, &rp("a")).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath, "{e:?}");
    let e = git.is_ignored_in_export(Path::new("exp/d"), &tree, &rp("a")).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    let e = git.is_ignored_in_export(&dir.join("CLAUDE.md"), &tree, &rp("a")).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    fs::create_dir(dir.join(".git")).unwrap();
    let with_git = snapshot(&sb.path("exp"));
    let e = git.is_ignored_in_export(&dir, &tree, &rp("a")).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!(reason(&e).as_deref(), Some("has-git"));
    assert_eq!(snapshot(&sb.path("exp")), with_git);
    fs::remove_dir(dir.join(".git")).unwrap();
    assert_eq!(snapshot(&sb.path("exp")), before);
}
