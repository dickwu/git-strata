//! session.md §8 Task 2: the full sibling layout on per-session shadow repos.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use strata_session::{NewReport, NewWarning, SessionSeam};
use strata_store::Lifecycle;
use strata_types::{BaseGen, Code, CommitOid, DepsSetId};
use support::*;

fn commit_oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.to_string()).unwrap()
}

fn ws(rep: &NewReport, repo: &str) -> PathBuf {
    rep.manifest.repos[&repo_name(repo)].path.clone()
}

fn trash_count(fx: &Fx) -> usize {
    fs::read_dir(fx.root().join("store/trash")).map(|d| d.count()).unwrap_or(0)
}

fn staging_empty(fx: &Fx, repo: &str) -> bool {
    let p = fx.root().join("bases").join(repo).join(".staging");
    !p.exists() || fs::read_dir(&p).unwrap().count() == 0
}

/// Commit to main and publish it to origin.
fn push(fx: &Fx, repo: &str, files: &[(&str, &[u8])], msg: &str) -> String {
    let main = fx.main(repo);
    let c = fx.sb.commit(&main, files, msg);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    c
}

/// Append ignore patterns to the committed `.gitignore` and publish.
fn ignore(fx: &Fx, repo: &str, extra: &str) {
    let main = fx.main(repo);
    let mut g = fs::read(main.join(".gitignore")).unwrap();
    g.extend_from_slice(extra.as_bytes());
    push(fx, repo, &[(".gitignore", &g)], "ignore");
}

fn assert_refused_without_session(fx: &Fx, e: &strata_types::StrataError) {
    assert_eq!(fx.session_rows(), 0, "a session row after {e:?}");
    assert!(fx.session_dirs().is_empty(), "{:?}", fx.session_dirs());
}

fn readlink_bytes(p: &Path) -> Vec<u8> {
    fs::read_link(p).unwrap().as_os_str().as_bytes().to_vec()
}

// ---------------------------------------------------------------- §5.2

#[test]
fn ensure_base_builds_outside_the_lock() {
    let fx = Fx::with(&["tool"]);
    let tool = fx.rn("tool");
    let main = fx.main("tool");
    let s = fx.sessions();

    // Cold path: the first staging only feeds `ensure`, then is retired; the
    // registered generation comes from a second staging.
    let t0 = trash_count(&fx);
    let h0 = commit_oid(&fx.sb.head(&main));
    let g0 = s.ensure_base(&tool, &h0).unwrap();
    assert_eq!(fx.sb.install_count(), 1);
    assert!(trash_count(&fx) > t0, "the first staging was retired");
    assert!(fx.store().has_base_gen(&g0).unwrap());
    assert!(staging_empty(&fx, "tool"));

    // Export, deps and manifest run without the lock.
    fx.lock.set_wait(Duration::from_millis(200));
    for (i, phase) in ["base.export", "base.deps", "base.manifest"].iter().enumerate() {
        let head = commit_oid(&fx.sb.commit(&main, &[(&format!("f{i}.txt"), b"x\n")], "more"));
        std::thread::scope(|sc| {
            s.arm(SessionSeam::PauseAt { phase: phase.to_string() });
            let h = sc.spawn(|| s.ensure_base(&tool, &head));
            s.wait_paused();
            let mut ran = false;
            fx.lock_write(&tool, || ran = true).unwrap_or_else(|e| panic!("{phase}: lock held: {e:?}"));
            assert!(ran);
            s.release();
            let g = h.join().unwrap().unwrap();
            assert!(fx.store().has_base_gen(&g).unwrap());
            assert_eq!(g.deps, g0.deps);
        });
    }
    fx.lock.set_wait(Duration::from_secs(60));

    // Register waits for the lock.
    let head = commit_oid(&fx.sb.commit(&main, &[("reg.txt", b"r\n")], "reg"));
    let want = BaseGen { repo: tool.clone(), tree: fx.gits[&tool].tree_of(&head).unwrap(), deps: g0.deps.clone() };
    std::thread::scope(|sc| {
        s.arm(SessionSeam::PauseAt { phase: "base.register".into() });
        let h = sc.spawn(|| s.ensure_base(&tool, &head));
        s.wait_paused();
        let guard = fx.lock.hold("tool");
        s.release();
        std::thread::sleep(Duration::from_millis(200));
        assert!(!fx.store().has_base_gen(&want).unwrap(), "registered while the lock was held");
        drop(guard);
        assert_eq!(h.join().unwrap().unwrap(), want);
    });

    // Two builders of one tree: the later one retires its own staging.
    let head = commit_oid(&fx.sb.commit(&main, &[("race.txt", b"r\n")], "race"));
    let tree = fx.gits[&tool].tree_of(&head).unwrap();
    let before_trash = trash_count(&fx);
    std::thread::scope(|sc| {
        s.arm(SessionSeam::PauseAt { phase: "base.register".into() });
        let a = sc.spawn(|| s.ensure_base(&tool, &head));
        s.wait_paused();
        let b = s.ensure_base(&tool, &head).unwrap();
        s.release();
        let a = a.join().unwrap().unwrap();
        assert_eq!(a, b);
    });
    let gens: Vec<_> = fs::read_dir(fx.root().join("bases/tool").join(tree.as_str())).unwrap().collect();
    assert_eq!(gens.len(), 1, "one generation directory");
    assert!(trash_count(&fx) > before_trash, "A's staging went to the trash");
    assert!(staging_empty(&fx, "tool"));

    // ensure_base_for: an old tree with a newer sealed set, never installing.
    let old_tree = fx.gits[&tool].tree_of(&h0).unwrap();
    let h2 = commit_oid(&fx.sb.commit(&main, &[("bun.lock", b"lock v2\n")], "bump"));
    let g2 = s.ensure_base(&tool, &h2).unwrap();
    assert_ne!(g2.deps, g0.deps);
    fx.sb.set_install_script("echo no cold installs here >&2; exit 1\n");
    let installs = fx.sb.install_count();
    let g = s.ensure_base_for(&tool, &old_tree, &g2.deps).unwrap();
    assert_eq!(g, BaseGen { repo: tool.clone(), tree: old_tree.clone(), deps: g2.deps.clone() });
    assert!(fx.store().has_base_gen(&g).unwrap());
    assert_eq!(fx.sb.install_count(), installs, "ensure_base_for never installs");

    let unsealed = DepsSetId::try_from("ab".repeat(32)).unwrap();
    let e = s.ensure_base_for(&tool, &tree, &unsealed).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!(reason(&e), Some("deps_unsealed"));
    assert!(staging_empty(&fx, "tool"));
    assert_eq!(fx.sb.install_count(), installs);
}

// ---------------------------------------------------------------- layout

#[test]
fn new_clones_entire_sibling_layout() {
    let fx = Fx::with(&["quick-start", "release-notes", "tool"]);
    push(&fx, "quick-start", &[("src/index.ts", b"export const quick = 42;\n")], "quick");
    push(&fx, "tool", &[("scripts/read.sh", b"cat ../quick-start/src/index.ts\n")], "sibling read");
    let s = fx.sessions();
    let rep = s.new(Some("9246"), &[fx.rn("tool")]).unwrap();
    let m = &rep.manifest;
    assert_eq!(m.repos.len(), 3);
    for (r, w) in &m.repos {
        assert_eq!(w.tracked, r.as_str() == "tool", "{r}");
        assert_eq!(w.path, m.root.join(r.as_str()));
        assert!(!w.path.to_string_lossy().contains("9246"));
    }
    assert_eq!(m.root, fx.root().join("sessions").join(m.id.as_str()));
    let names = |p: PathBuf| -> BTreeSet<String> {
        fs::read_dir(p).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
    };
    assert_eq!(names(m.root.clone()), ["quick-start", "release-notes", "tool"].iter().map(|s| s.to_string()).collect());
    assert_eq!(
        names(fx.root().join("sessions/.repos").join(m.id.as_str())),
        ["quick-start.git", "release-notes.git", "tool.git"].iter().map(|s| s.to_string()).collect()
    );
    let via_sibling = fs::read(ws(&rep, "tool").join("../quick-start/src/index.ts")).unwrap();
    let pinned = &m.repos[&fx.rn("quick-start")].head;
    let want = fx.sb.cmd(&fx.main("quick-start")).args(["show", &format!("{}:src/index.ts", pinned.as_str())]).output().unwrap().stdout;
    assert_eq!(via_sibling, want);
    let rec = fx.store().session(&m.id).unwrap().unwrap();
    assert_eq!(rec.lifecycle, Lifecycle::Active);
}

/// Every non-.git, non-deps, non-cache, non-seed entry of `ws`.
fn walk(root: &Path, rel: &Path, skip: &BTreeSet<&str>, out: &mut BTreeMap<String, fs::Metadata>) {
    for e in fs::read_dir(root.join(rel)).unwrap() {
        let e = e.unwrap();
        let r = rel.join(e.file_name());
        let rs = r.to_string_lossy().into_owned();
        if skip.contains(rs.as_str()) {
            continue;
        }
        let md = fs::symlink_metadata(e.path()).unwrap();
        if md.is_dir() {
            walk(root, &r, skip, out);
        } else {
            out.insert(rs, md);
        }
    }
}

fn workspace_equals_tree(n: usize) {
    let fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    for i in 0..n {
        let rel = format!("gen/d{}/f{i}.txt", i % 10);
        fx.sb.write(&main, &rel, format!("file {i}\n").as_bytes());
        if i % 7 == 0 {
            fs::set_permissions(main.join(&rel), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    symlink("d0/f0.txt", main.join("gen/link")).unwrap();
    fx.sb.git(&main, &["add", "-A"]);
    fx.sb.git(&main, &["commit", "-q", "-m", "many"]);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);

    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let w = ws(&rep, "tool");
    let st = &rep.manifest.repos[&fx.rn("tool")];
    let skip: BTreeSet<&str> = [".git", "node_modules", ".cache"].into();
    let mut got = BTreeMap::new();
    walk(&w, Path::new(""), &skip, &mut got);
    let ls = fx.sb.git(&main, &["ls-tree", "-r", "--full-tree", st.head.as_str()]);
    let mut want = BTreeMap::new();
    for line in ls.lines() {
        let (meta, path) = line.split_once('\t').unwrap();
        let f: Vec<&str> = meta.split(' ').collect();
        want.insert(path.to_string(), (f[0].to_string(), f[2].to_string()));
    }
    assert_eq!(got.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>());
    for (p, (mode, oid)) in &want {
        let md = &got[p];
        let blob = fx.sb.cmd(&main).args(["cat-file", "blob", oid]).output().unwrap().stdout;
        match mode.as_str() {
            "120000" => {
                assert!(md.file_type().is_symlink(), "{p}");
                assert_eq!(readlink_bytes(&w.join(p)), blob, "{p}");
            }
            m => {
                assert!(md.is_file(), "{p}");
                assert_eq!(fs::read(w.join(p)).unwrap(), blob, "{p}");
                assert_eq!(md.permissions().mode() & 0o111 != 0, m == "100755", "{p} exec bit");
            }
        }
    }
    assert!(fx.store().has_base_gen(&st.base).unwrap());
    assert!(fx.store().base_dir(&st.base).is_dir());
    assert!(staging_empty(&fx, "tool"));
    let beside: Vec<String> = fx.session_dirs();
    assert_eq!(beside, vec![format!(".repos/{}", rep.manifest.id), rep.manifest.id.to_string()]);
}

#[test]
fn new_workspace_equals_base_tree() {
    workspace_equals_tree(100);
    workspace_equals_tree(1000);
}

#[test]
fn new_workspace_keeps_seed_bytes_and_link_targets() {
    let mut fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    ignore(&fx, "tool", ".env.local\nCLAUDE.md\n.claude/\nother.txt\n");
    // Five links in the tree, plus a binary file.
    symlink("node_modules/x/index.js", main.join("to-deps")).unwrap();
    symlink("nowhere/at/all", main.join("broken")).unwrap();
    symlink("self", main.join("self")).unwrap();
    symlink("../../outside/the/repo", main.join("outside")).unwrap();
    symlink(OsStr::from_bytes(b"caf\xe9-latin1"), main.join("latin1")).unwrap();
    fs::write(main.join("bin.dat"), [0u8, 159, 146, 150, 255, 0, 1]).unwrap();
    fx.sb.git(&main, &["add", "to-deps", "broken", "self", "outside", "latin1", "bin.dat"]);
    fx.sb.git(&main, &["commit", "-q", "-m", "links"]);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    // Seeds: binary .env.local, CLAUDE.md as a link, a non-UTF-8 link candidate.
    let env_bytes: &[u8] = b"TOKEN=\x00\xff\xfe\n";
    fs::write(main.join(".env.local"), env_bytes).unwrap();
    fs::write(main.join("CLAUDE-real.md"), b"# real\n").unwrap();
    fx.sb.git(&main, &["add", "CLAUDE-real.md"]);
    fx.sb.git(&main, &["commit", "-q", "-m", "claude"]);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    symlink("CLAUDE-real.md", main.join("CLAUDE.md")).unwrap();
    fs::create_dir_all(main.join(".claude")).unwrap();
    symlink(OsStr::from_bytes(b"\xff\xfe-not-utf8"), main.join(".claude/settings.local.json")).unwrap();
    fs::write(main.join("other.txt"), b"not a seed\n").unwrap();

    // Explicit non-UTF-8 link: refused before create_session.
    fx.cfg_mut("tool").seed_files = Some(rps(&[".claude/settings.local.json"]));
    {
        let s = fx.sessions();
        let e = s.new(None, &[fx.rn("tool")]).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{e:?}");
        assert_eq!(reason(&e), Some("link_not_utf8"));
        assert_refused_without_session(&fx, &e);
    }
    fx.cfg_mut("tool").seed_files = None;

    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let w = ws(&rep, "tool");
    let head = rep.manifest.repos[&fx.rn("tool")].head.clone();
    for l in ["to-deps", "broken", "self", "outside", "latin1"] {
        let blob = fx.sb.cmd(&main).args(["cat-file", "blob", &format!("{}:{l}", head.as_str())]).output().unwrap().stdout;
        assert_eq!(readlink_bytes(&w.join(l)), blob, "{l}");
    }
    assert_eq!(fs::read(w.join("bin.dat")).unwrap(), fs::read(main.join("bin.dat")).unwrap());
    assert_eq!(fs::read(w.join(".env.local")).unwrap(), env_bytes);
    assert!(fs::symlink_metadata(w.join("CLAUDE.md")).unwrap().file_type().is_symlink());
    assert_eq!(readlink_bytes(&w.join("CLAUDE.md")), b"CLAUDE-real.md");
    assert!(fs::symlink_metadata(w.join(".claude/settings.local.json")).is_err(), "a non-UTF-8 candidate is skipped");
    assert!(!w.join("other.txt").exists(), "ignored files outside the list stay out");

    fs::write(main.join(".env.local"), b"changed\n").unwrap();
    fs::remove_file(main.join("CLAUDE.md")).unwrap();
    assert_eq!(fs::read(w.join(".env.local")).unwrap(), env_bytes);
    assert_eq!(readlink_bytes(&w.join("CLAUDE.md")), b"CLAUDE-real.md");

    let gitfile = fs::symlink_metadata(w.join(".git")).unwrap();
    assert!(gitfile.is_file(), ".git is a gitfile");
    let shadow = fx.root().join("sessions/.repos").join(rep.manifest.id.as_str()).join("tool.git");
    assert!(shadow.join("objects/info/alternates").is_file(), "objects are borrowed, not copied");
    let packs = fs::read_dir(shadow.join("objects/pack")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(packs, 0, "no packs copied into the shadow");
}

#[test]
fn new_refuses_explicit_seed_tracked_or_unignored() {
    let mut fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    ignore(&fx, "tool", ".env*\n.ENV*\n.claude-target/\n");
    push(&fx, "tool", &[("src-tauri/src/secrets.rs", b"pub const S: &str = \"\";\n"), ("CLAUDE.md", b"# tracked\n")], "tracked");
    fs::write(main.join("notes.txt"), b"not ignored\n").unwrap();
    let tree_of_main = |fx: &Fx| -> BaseGen {
        let st = fx.store().sessions().unwrap();
        assert!(st.is_empty());
        let head = commit_oid(&fx.sb.git(&fx.main("tool"), &["rev-parse", "origin/main"]));
        let s = fx.sessions();
        s.ensure_base(&fx.rn("tool"), &head).unwrap()
    };

    let case = |fx: &mut Fx, seeds: &[&str], code: Code, why: &str, index: u64| {
        fx.cfg_mut("tool").seed_files = Some(rps(seeds));
        let s = fx.sessions();
        let e = s.new(None, &[fx.rn("tool")]).unwrap_err();
        assert_eq!(e.code, code, "{seeds:?}: {e:?}");
        assert_eq!(reason(&e), Some(why), "{e:?}");
        assert_eq!(detail_str(&e, "repo"), Some("tool"));
        assert_eq!(detail_str(&e, "field"), Some("seed_files"));
        assert_eq!(detail_str(&e, "rule"), Some("seed_files"));
        assert_eq!(e.detail.as_ref().unwrap()["index"].as_u64(), Some(index));
        drop(s);
        assert_refused_without_session(fx, &e);
    };
    case(&mut fx, &["src-tauri/src/secrets.rs"], Code::InvalidInput, "tracked", 0);
    case(&mut fx, &["notes.txt"], Code::InvalidInput, "not_ignored", 0);
    case(&mut fx, &["CLAUDE.md"], Code::InvalidInput, "tracked", 0);
    // The base generation was built and registered all the same.
    let g = tree_of_main(&fx);
    assert!(fx.store().has_base_gen(&g).unwrap());

    // Case-insensitive volume: the tracked .ENV blocks an explicit .env.
    push(&fx, "tool", &[(".ENV", b"TRACKED=1\n")], "upper env");
    fx.sb.git(&main, &["add", "-f", ".ENV"]);
    fx.sb.git(&main, &["commit", "-q", "--allow-empty", "-m", "force"]);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    let tracked = fx.sb.git(&main, &["ls-files", ".ENV"]);
    assert_eq!(tracked, ".ENV");
    // Git identity follows core.ignorecase (true here, as git init sets it
    // on this volume): the tracked check matches first.
    case(&mut fx, &[".env"], Code::InvalidInput, "tracked", 0);
    assert_eq!(fs::read(main.join(".ENV")).unwrap(), b"TRACKED=1\n");
    // With core.ignorecase false git sees no .env, but disk identity
    // follows the volume: the destination already exists in the base.
    fx.sb.git(&main, &["config", "core.ignorecase", "false"]);
    fx.reopen_gits();
    case(&mut fx, &[".env"], Code::InvalidInput, "exists_in_base", 0);
    assert_eq!(fs::read(main.join(".ENV")).unwrap(), b"TRACKED=1\n");
    fx.sb.git(&main, &["config", "core.ignorecase", "true"]);
    fx.reopen_gits();

    fs::write(main.join(".env.local"), b"A=1\n").unwrap();
    case(&mut fx, &[".env.local", ".ENV.LOCAL"], Code::InvalidInput, "duplicate", 1);

    // An explicit entry under a symlinked .claude.
    fs::create_dir_all(main.join(".claude-target")).unwrap();
    fs::write(main.join(".claude-target/settings.local.json"), b"{}\n").unwrap();
    symlink(".claude-target", main.join(".claude")).unwrap();
    fs::write(main.join(".git/info/exclude"), b".claude\n").unwrap();
    fx.cfg_mut("tool").seed_files = Some(rps(&[".claude/settings.local.json"]));
    let s = fx.sessions();
    let e = s.new(None, &[fx.rn("tool")]).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(reason(&e), Some("symlink"));
    drop(s);
    assert_refused_without_session(&fx, &e);
}

fn snapshot_with_mtime(dir: &Path) -> (Vec<String>, std::time::SystemTime) {
    let mut v: Vec<String> = Vec::new();
    fn rec(d: &Path, base: &Path, v: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let e = e.unwrap();
            v.push(e.path().strip_prefix(base).unwrap().to_string_lossy().into_owned());
            if e.file_type().unwrap().is_dir() {
                rec(&e.path(), base, v);
            }
        }
    }
    rec(dir, dir, &mut v);
    v.sort();
    (v, fs::metadata(dir).unwrap().modified().unwrap())
}

#[test]
fn new_seed_writes_never_leave_the_session_root() {
    // The base tree (origin) tracks `conf` and `.claude` as symlinks to O,
    // outside every session root. The main checkout still sits on the
    // commit before, with real, ignored `conf/` and `.claude/` directories,
    // so the sources are fine and only the destinations lead out.
    let mut fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    let o = fx.sb.root.join("outside-O");
    fs::create_dir_all(o.join("cache")).unwrap();
    fs::write(o.join("cache/c.bin"), b"cache").unwrap();
    fs::write(o.join("settings.local.json"), b"{}\n").unwrap();
    let c0 = fx.sb.head(&main);
    symlink(&o, main.join("conf")).unwrap();
    symlink(&o, main.join(".claude")).unwrap();
    fx.sb.git(&main, &["add", "conf", ".claude"]);
    fx.sb.git(&main, &["commit", "-q", "-m", "links out"]);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    fx.sb.git(&main, &["reset", "-q", "--hard", &c0]);
    assert!(!main.join("conf").exists());
    fs::write(main.join(".git/info/exclude"), b"conf/\n.claude/\n").unwrap();
    fx.sb.write(&main, "conf/.env", b"E=1\n");
    fx.sb.write(&main, "conf/cache/m.bin", b"main cache");
    fx.sb.write(&main, ".claude/settings.local.json", b"{\"main\":1}\n");
    let before = snapshot_with_mtime(&o);

    fx.cfg_mut("tool").seed_files = Some(rps(&["conf/.env"]));
    {
        let s = fx.sessions();
        let e = s.new(None, &[fx.rn("tool")]).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{e:?}");
        assert_eq!(reason(&e), Some("parent_not_directory"));
        assert_eq!(e.detail.as_ref().unwrap()["parent"], "conf");
        assert_refused_without_session(&fx, &e);
    }
    fx.cfg_mut("tool").seed_files = None;
    fx.cfg_mut("tool").seed_caches = rps(&["conf/cache"]);
    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let w = ws(&rep, "tool");
    assert!(fs::symlink_metadata(w.join("conf")).unwrap().file_type().is_symlink());
    assert!(fs::symlink_metadata(w.join(".claude")).unwrap().file_type().is_symlink());
    let seeds = &fx.store().session(&rep.manifest.id).unwrap().unwrap().workspaces[&fx.rn("tool")].seeds;
    assert!(seeds.is_empty(), "{seeds:?}");
    assert_eq!(snapshot_with_mtime(&o), before, "O was written to");
}

// ---------------------------------------------------------------- pins

fn pinned_head(fx: &Fx) -> String {
    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let h = rep.manifest.repos[&fx.rn("tool")].head.as_str().to_string();
    assert_eq!(fx.sb.git(&fx.main("tool"), &["rev-parse", &format!("refs/strata/pins/{}", rep.manifest.id)]), h);
    h
}

fn set_ref(fx: &Fx, name: &str, to: Option<&str>) {
    let main = fx.main("tool");
    match to {
        Some(c) => fx.sb.git(&main, &["update-ref", name, c]),
        None => {
            let _ = fx.sb.git_output(&main, &["update-ref", "-d", name]);
            String::new()
        }
    };
}

#[test]
fn new_pins_chain_top_when_ahead() {
    let mut fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    let c0 = fx.sb.head(&main);
    let c1 = fx.sb.commit(&main, &[("ahead.txt", b"1\n")], "ahead");
    fx.sb.git(&main, &["reset", "-q", "--hard", &c0]);
    let c2 = fx.sb.commit(&main, &[("other.txt", b"2\n")], "diverged");
    fx.sb.git(&main, &["reset", "-q", "--hard", &c0]);

    // Leads with an anchor on origin → the chain top.
    set_ref(&fx, "refs/strata/publish/main", Some(&c1));
    set_ref(&fx, "refs/strata/anchor/main", Some(&c0));
    assert_eq!(pinned_head(&fx), c1);
    // Leads without an anchor → origin.
    set_ref(&fx, "refs/strata/anchor/main", None);
    assert_eq!(pinned_head(&fx), c0);
    // Anchor not on origin → origin.
    set_ref(&fx, "refs/strata/anchor/main", Some(&c2));
    assert_eq!(pinned_head(&fx), c0);
    // Equal → origin.
    set_ref(&fx, "refs/strata/anchor/main", Some(&c0));
    set_ref(&fx, "refs/strata/publish/main", Some(&c0));
    assert_eq!(pinned_head(&fx), c0);
    // Diverged → origin.
    set_ref(&fx, "refs/strata/publish/main", Some(&c2));
    fx.sb.git(&main, &["push", "-q", "origin", &format!("{c1}:refs/heads/main")]);
    assert_eq!(pinned_head(&fx), c1);
    // Behind → origin.
    set_ref(&fx, "refs/strata/publish/main", Some(&c0));
    assert_eq!(pinned_head(&fx), c1);
    // pin_unpushed = false → origin even when leading.
    fx.sb.git(&main, &["reset", "-q", "--hard", &c1]);
    let c3 = fx.sb.commit(&main, &[("lead2.txt", b"3\n")], "lead again");
    set_ref(&fx, "refs/strata/publish/main", Some(&c3));
    fx.cfg_mut("tool").pin_unpushed = false;
    assert_eq!(pinned_head(&fx), c1);
    fx.cfg_mut("tool").pin_unpushed = true;
    assert_eq!(pinned_head(&fx), c3);
}

#[test]
fn new_pins_default_branch_when_publish_branch_differs() {
    let mut fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    let c0 = fx.sb.head(&main);
    let c1 = fx.sb.commit(&main, &[("ahead.txt", b"1\n")], "ahead");
    fx.cfg_mut("tool").publish.branch = "release".into();
    set_ref(&fx, "refs/strata/publish/release", Some(&c1));
    set_ref(&fx, "refs/strata/anchor/release", Some(&c0));
    set_ref(&fx, "refs/strata/publish/main", Some(&c1));
    set_ref(&fx, "refs/strata/anchor/main", Some(&c0));
    assert_eq!(pinned_head(&fx), c0);
}

// ---------------------------------------------------------------- seeds

#[test]
fn new_seeds_default_names_only_when_untracked_and_ignored() {
    let fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    ignore(&fx, "tool", ".env.local\n");
    push(&fx, "tool", &[("CLAUDE.md", b"# base\n")], "claude");
    fs::write(main.join("CLAUDE.md"), b"# local edit\n").unwrap();
    fs::write(main.join(".env"), b"NOT_IGNORED=1\n").unwrap();
    fs::write(main.join(".env.local"), b"LOCAL=1\n").unwrap();
    let excludes = fx.sb.root.join("global-excludes");
    fs::write(&excludes, b".claude/\n").unwrap();
    let mut gc = fs::read_to_string(fx.sb.home.join(".gitconfig")).unwrap();
    gc.push_str(&format!("[core]\n\texcludesFile = {}\n", excludes.display()));
    fs::write(fx.sb.home.join(".gitconfig"), gc).unwrap();
    fx.sb.write(&main, ".claude/settings.local.json", b"{\"a\":1}\n");

    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    let w = ws(&rep, "tool");
    assert_eq!(fs::read(w.join("CLAUDE.md")).unwrap(), b"# base\n");
    assert!(!w.join(".env").exists());
    assert_eq!(fs::read(w.join(".env.local")).unwrap(), b"LOCAL=1\n");
    assert_eq!(fs::read(w.join(".claude/settings.local.json")).unwrap(), b"{\"a\":1}\n");
    let seeds = &fx.store().session(&rep.manifest.id).unwrap().unwrap().workspaces[&fx.rn("tool")].seeds;
    let paths: BTreeSet<&str> = seeds.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, [".claude/settings.local.json", ".env.local"].into());

    // A directory and a FIFO candidate are skipped; the FIFO is never opened.
    fs::remove_file(main.join(".env")).unwrap();
    fs::create_dir(main.join(".env")).unwrap();
    fs::remove_file(main.join(".env.local")).unwrap();
    mkfifo(&main.join(".env.local"));
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let w = ws(&rep, "tool");
    assert!(!w.join(".env").exists());
    assert!(!w.join(".env.local").exists());

    // A tracked .ENV on a case-insensitive volume: the .env candidate is skipped.
    fs::remove_dir(main.join(".env")).unwrap();
    fs::remove_file(main.join(".env.local")).unwrap();
    push(&fx, "tool", &[(".ENV", b"BASE=1\n")], "upper");
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let w = ws(&rep, "tool");
    let names: Vec<String> = fs::read_dir(&w).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.contains(&".ENV".to_string()) && !names.contains(&".env".to_string()), "{names:?}");
    assert_eq!(fs::read(w.join(".ENV")).unwrap(), b"BASE=1\n");
}

#[test]
fn new_clones_seed_caches_as_real_directories() {
    let mut fx = Fx::new();
    ignore(&fx, "tool", "target/\n.next/\n");
    for r in ["front", "tool"] {
        fx.cfg_mut(r).seed_caches = rps(&["src-tauri/target", ".next/cache", "node_modules/.cache"]);
        let m = fx.main(r);
        fx.sb.write(&m, "src-tauri/target/debug/app", b"binary");
        fx.sb.write(&m, ".next/cache/c.json", b"{}");
        fx.sb.write(&m, "node_modules/.cache/x.bin", b"cached");
    }
    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let w = ws(&rep, "tool");
    for (c, f) in [("src-tauri/target", "debug/app"), (".next/cache", "c.json"), ("node_modules/.cache", "x.bin")] {
        let md = fs::symlink_metadata(w.join(c)).unwrap();
        assert!(md.is_dir() && !md.file_type().is_symlink(), "{c}");
        fs::write(w.join(c).join(f), b"edited").unwrap();
        assert_ne!(fs::read(fx.main("tool").join(c).join(f)).unwrap(), b"edited", "{c}");
    }
    // The deps dir from the base generation is still whole around the cache.
    let bdir = fx.store().base_dir(&rep.manifest.repos[&fx.rn("tool")].base);
    for e in fs::read_dir(bdir.join("node_modules")).unwrap() {
        let n = e.unwrap().file_name();
        assert!(fs::symlink_metadata(w.join("node_modules").join(&n)).is_ok(), "{n:?} lost from the deps dir");
    }
    let front = ws(&rep, "front");
    assert!(!front.join("src-tauri/target").exists());
    assert!(!front.join(".next/cache").exists());
    assert!(!front.join("node_modules/.cache").exists());
    drop(s);

    // A symlinked target in main: NotDirectory before any write.
    let m = fx.main("tool");
    fs::remove_dir_all(m.join("src-tauri/target")).unwrap();
    let elsewhere = fx.sb.root.join("elsewhere-target");
    fs::create_dir_all(&elsewhere).unwrap();
    symlink(&elsewhere, m.join("src-tauri/target")).unwrap();
    let rows = fx.session_rows();
    let dirs = fx.session_dirs();
    let s = fx.sessions();
    let e = s.new(None, &[fx.rn("tool")]).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    assert_eq!(detail_str(&e, "field"), Some("seed_caches"));
    assert_eq!(fx.session_rows(), rows);
    assert_eq!(fx.session_dirs(), dirs);
}

// ---------------------------------------------------------------- observe

#[test]
fn new_reports_a_failed_observation_as_a_warning() {
    for mode in ["fail", "busy"] {
        let fx = Fx::new();
        let first = fx.rn("front");
        let s = fx.sessions();
        let rep = if mode == "fail" {
            s.arm(SessionSeam::FailAt { phase: "new.observe@front".into(), errno: libc::EIO });
            s.new(None, &fx.all()).unwrap()
        } else {
            std::thread::scope(|sc| {
                s.arm(SessionSeam::PauseAt { phase: "new.finalize".into() });
                let h = sc.spawn(|| s.new(None, &fx.all()));
                s.wait_paused();
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
                let lock = &fx.lock;
                let holder = sc.spawn(move || {
                    let g = lock.hold("front");
                    held_tx.send(()).unwrap();
                    rx.recv().unwrap();
                    drop(g);
                });
                held_rx.recv().unwrap();
                fx.lock.set_wait(Duration::from_millis(300));
                s.release();
                let r = h.join().unwrap();
                tx.send(()).unwrap();
                holder.join().unwrap();
                fx.lock.set_wait(Duration::from_secs(60));
                r.unwrap()
            })
        };
        assert_eq!(rep.warnings.len(), 1, "{mode}: {:?}", rep.warnings);
        let NewWarning::ObserveFailed { repo, detail } = &rep.warnings[0];
        assert_eq!(repo, &first);
        let want = if mode == "fail" { Code::Unknown } else { Code::Busy };
        assert_eq!(detail.code, want, "{mode}: {detail:?}");
        let rec = fx.store().session(&rep.manifest.id).unwrap().unwrap();
        assert_eq!(rec.lifecycle, Lifecycle::Active);
        assert!(rec.scene.is_none());
        assert!(fx.store().deps_main(&fx.rn("tool")).unwrap().is_some(), "{mode}: tool was observed");
        assert!(ws(&rep, "front").join("src/index.ts").is_file());
        assert!(!fx.pins("front").is_empty());
        let ctx_ok = fx.store().deps_main(&first).unwrap();
        assert!(ctx_ok.is_none(), "{mode}: front was not recorded");
        // status is unaffected, and observing again fills the gap.
        let st = fx.sessions().status(&rep.manifest.id).unwrap();
        assert_eq!(st.state, strata_session::SessionState::Active, "{mode}");
        assert!(st.workspaces.values().all(|w| w.health == strata_session::WorkspaceHealth::Ok), "{mode}");
        let fw = &rep.manifest.repos[&first];
        let ctx = strata_deps::Ctx {
            store: fx.store(),
            paths: &fx.config.paths,
            repo: &first,
            cfg: &fx.config.repos[&first],
            git: &fx.gits[&first],
            env: &fx.env,
            locks: &fx.lock,
        };
        strata_deps::observe_main(&ctx, &fw.base.tree, &fw.head, &fx.store().base_dir(&fw.base)).unwrap();
        assert!(fx.store().deps_main(&first).unwrap().is_some(), "{mode}: front recorded after observing again");
    }
}

// ---------------------------------------------------------------- git

#[test]
fn workspace_git_is_detached_and_prepush_refuses() {
    let fx = Fx::new();
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    let bare = fx.sb.root.join("push-target.git");
    fx.sb.git(&fx.sb.root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    for (r, st) in &rep.manifest.repos {
        let w = &st.path;
        assert!(!fx.sb.git_output(w, &["symbolic-ref", "-q", "HEAD"]).status.success(), "{r} attached");
        assert_eq!(fx.sb.git(w, &["rev-parse", "HEAD"]), st.head.as_str());
        assert_eq!(fx.gits[r].tree_of(&st.head).unwrap(), st.base.tree);
        let common = PathBuf::from(fx.sb.git(w, &["rev-parse", "--git-common-dir"]));
        let want = fx.root().join("sessions/.repos").join(rep.manifest.id.as_str()).join(format!("{r}.git"));
        assert_eq!(fs::canonicalize(w.join(&common)).unwrap(), fs::canonicalize(&want).unwrap());
        assert_eq!(fx.sb.git(w, &["remote"]), "");
        assert_eq!(fx.sb.git(w, &["status", "--porcelain"]), "", "{r} dirty");

        let url = bare.to_str().unwrap();
        let o = fx.sb.git_output(w, &["push", url, "HEAD:refs/heads/x"]);
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("git-strata"), "{}", String::from_utf8_lossy(&o.stderr));
        let empty_hooks = fx.sb.root.join("empty-hooks");
        fs::create_dir_all(&empty_hooks).unwrap();
        let global = fx.sb.root.join("global-hooks.gitconfig");
        fs::write(&global, format!("[core]\n\thooksPath = {}\n", empty_hooks.display())).unwrap();
        let o = fx.sb.cmd(w).env("GIT_CONFIG_GLOBAL", &global).args(["push", url, "HEAD:refs/heads/x"]).output().unwrap();
        assert!(!o.status.success(), "a user-level hooksPath bypassed the guard");
        assert!(fx.sb.git(&bare, &["for-each-ref", "refs/heads/x"]).is_empty());
        let o = fx.sb.cmd(w).env("STRATA_PUBLISH", "1").args(["push", "-q", url, "HEAD:refs/heads/x"]).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        fx.sb.git(&bare, &["update-ref", "-d", "refs/heads/x"]);
    }
}

#[test]
fn manifest_round_trips_through_store() {
    let mut fx = Fx::new();
    let rep = {
        let s = fx.sessions();
        s.new(Some("9246"), &[fx.rn("tool")]).unwrap()
    };
    let json = serde_json::to_string(&rep.manifest).unwrap();
    assert_eq!(serde_json::from_str::<strata_types::SessionManifest>(&json).unwrap(), rep.manifest);
    fx.close_store();
    fx.reopen_store();
    let rec = fx.store().session(&rep.manifest.id).unwrap().unwrap();
    assert_eq!(rec.manifest, rep.manifest);
    assert_eq!(rec.manifest.ticket.as_deref(), Some("9246"));
}

#[test]
#[ignore = "timing target (design §8): measured for the user's decision; run by hand"]
fn new_warm_under_5s() {
    let fx = Fx::with(&["front", "quick-start", "release-notes", "tool"]);
    let s = fx.sessions();
    s.new(None, &[fx.rn("tool")]).unwrap();
    let t = Instant::now();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let took = t.elapsed();
    eprintln!("warm new of {} repos: {took:?}", rep.manifest.repos.len());
    assert!(took < Duration::from_secs(5), "{took:?}");
}
