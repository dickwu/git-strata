//! search.md §8 Task 2: the shared base index (§4.1, §5.1 steps 8–12).

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use strata_search::{Search, SearchSeam};
use strata_session::NewReport;
use strata_types::{Code, SessionId, TreeOid};
use support::*;
use tgrep_core::generations::{EntryContent, GenerationManager, IndexingProfile, Repository};

fn id(rep: &NewReport) -> SessionId {
    rep.manifest.id.clone()
}

fn tree(rep: &NewReport, repo: &str) -> TreeOid {
    rep.manifest.repos[&repo_name(repo)].base.tree.clone()
}

fn overlays(fx: &Fx, reps: &[(&NewReport, &str)]) -> FakeOverlays {
    let ov = FakeOverlays::default();
    for (rep, repo) in reps {
        ov.set(&id(rep), repo, fx.capture(&id(rep), repo));
    }
    ov
}

fn gen_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("gen-")).collect()
}

fn all_below(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn go(d: &Path, prefix: &str, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with(prefix) {
                out.push(p.clone());
            }
            if p.is_dir() {
                go(&p, prefix, out);
            }
        }
    }
    go(dir, prefix, &mut out);
    out
}

/// Push main's HEAD to origin's default branch, where new sessions start.
fn push(fx: &Fx, repo: &str) {
    let b = fx.config.repos[&fx.rn(repo)].default_branch.clone();
    fx.sb.git(&fx.main(repo), &["push", "-q", "origin", &format!("HEAD:refs/heads/{b}")]);
}

/// The product profile (D3), rebuilt here to open the same key from tgrep.
fn product_profile() -> IndexingProfile {
    IndexingProfile { max_blob_bytes: Some(strata_overlay::MERGE_MAX_BYTES), ..IndexingProfile::default() }
}

#[test]
fn base_index_shared_across_sessions_same_tree() {
    let fx = Fx::with(&["front"]);
    let a = fx.new_session(&["front"]);
    // Same tree, another commit.
    fx.sb.commit(&fx.main("front"), &[], "empty");
    push(&fx, "front");
    let b = fx.new_session(&["front"]);
    assert_eq!(tree(&a, "front"), tree(&b, "front"));
    assert_ne!(a.manifest.repos[&fx.rn("front")].head, b.manifest.repos[&fx.rn("front")].head);

    let ov = overlays(&fx, &[(&a, "front"), (&b, "front")]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let t = tree(&a, "front");
    assert_eq!(s.generation_ptr(&fx.rn("front"), &t), None);
    s.attach(&id(&a), &fx.rn("front")).unwrap();
    let p1 = s.generation_ptr(&fx.rn("front"), &t).expect("generation after attach");
    s.attach(&id(&b), &fx.rn("front")).unwrap();
    assert_eq!(s.generation_ptr(&fx.rn("front"), &t), Some(p1));

    let dir = s.storage_dir(&fx.rn("front")).unwrap();
    assert_eq!(gen_dirs(&dir).len(), 1, "{:?}", gen_dirs(&dir));
    // Nothing of `.git` was extracted: the only published data is the one
    // generation, and no stage was left.
    assert!(all_below(&dir, ".stage-").is_empty());

    // Eight concurrent attaches still share it.
    std::thread::scope(|sc| {
        for i in 0..8 {
            let s = &s;
            let r = if i % 2 == 0 { &a } else { &b };
            let fx = &fx;
            sc.spawn(move || s.attach(&id(r), &fx.rn("front")).unwrap());
        }
    });
    assert_eq!(s.generation_ptr(&fx.rn("front"), &t), Some(p1));
    assert_eq!(gen_dirs(&dir).len(), 1);

    // A second `Search` (another daemon instance) reuses the published
    // directory rather than building again.
    drop(s);
    let s2 = Search::open(&fx.config, &ss, &ov);
    s2.attach(&id(&a), &fx.rn("front")).unwrap();
    assert_eq!(gen_dirs(&dir).len(), 1);
}

#[test]
fn ensure_storage_is_under_strata_search_not_git_dir() {
    let fx = Fx::with(&["front"]);
    let rep = fx.new_session(&["front"]);
    let ov = overlays(&fx, &[(&rep, "front")]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    assert_eq!(s.storage_dir(&fx.rn("front")), None);
    s.attach(&id(&rep), &fx.rn("front")).unwrap();

    let dir = s.storage_dir(&fx.rn("front")).unwrap();
    let parent = fs::canonicalize(fx.root().join("search").join("front")).unwrap();
    assert!(dir.starts_with(&parent), "{} not under {}", dir.display(), parent.display());
    let git = fs::canonicalize(fx.main("front").join(".git")).unwrap();
    assert!(!dir.starts_with(&git), "{}", dir.display());
    assert!(dir.join("publication.lock").exists(), "no publication.lock in {}", dir.display());

    let main_id = Repository::discover(&fx.main("front")).unwrap().identity().to_string();
    let ws_id = Repository::discover(&fx.ws(&rep, "front").path).unwrap().identity().to_string();
    assert_ne!(main_id, ws_id, "the workspace is its own repository");
    assert_eq!(dir.file_name().unwrap().to_string_lossy(), main_id, "keyed by the main checkout");
    assert!(!parent.join(&ws_id).exists(), "nothing keyed by the workspace");
    // Nothing under either `.git`.
    assert!(all_below(&git, "gen-").is_empty());
    assert!(all_below(&fx.main("front").join(".git"), "publication.lock").is_empty());
}

#[test]
fn same_tree_oid_different_repos_do_not_share() {
    let fx = Fx::with(&["front", "tool"]);
    let rep = fx.new_session(&["front", "tool"]);
    assert_eq!(tree(&rep, "front"), tree(&rep, "tool"), "fixture: identical projects");
    let t = tree(&rep, "front");
    let ov = overlays(&fx, &[(&rep, "front"), (&rep, "tool")]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    s.attach(&id(&rep), &fx.rn("front")).unwrap();
    s.attach(&id(&rep), &fx.rn("tool")).unwrap();

    let pf = s.generation_ptr(&fx.rn("front"), &t).unwrap();
    let pt = s.generation_ptr(&fx.rn("tool"), &t).unwrap();
    assert_ne!(pf, pt);
    let df = s.storage_dir(&fx.rn("front")).unwrap();
    let dt = s.storage_dir(&fx.rn("tool")).unwrap();
    assert_ne!(df, dt);
    assert_eq!(gen_dirs(&df).len(), 1);
    assert_eq!(gen_dirs(&dt).len(), 1);

    // tool's repository over front's storage sees none of front's
    // generations.
    let tool = Repository::discover(&fx.main("tool")).unwrap();
    let m = GenerationManager::with_storage(tool, &fx.root().join("search").join("front")).unwrap();
    assert!(m.list().unwrap().is_empty());

    // Another profile is another key (test-only profile; the product path
    // uses D3's only).
    let scratch = fx.sb.root.join("profile-scratch");
    fs::create_dir_all(&scratch).unwrap();
    let front = Repository::discover(&fx.main("front")).unwrap();
    let m = GenerationManager::with_storage(front, &scratch).unwrap();
    let small = IndexingProfile { max_blob_bytes: Some(3), ..IndexingProfile::default() };
    let g3 = m.ensure("HEAD", small, None).unwrap().generation;
    let gd = m.ensure("HEAD", product_profile(), None).unwrap().generation;
    assert_ne!(g3.key(), gd.key());
    assert_eq!(g3.key().tree_oid(), gd.key().tree_oid());
    let pj = g3.entry("package.json").expect("listed in the small profile");
    assert!(matches!(pj.content, EntryContent::TooLarge), "{:?}", pj.content);
    assert!(matches!(gd.entry("package.json").unwrap().content, EntryContent::Indexed { .. }));
    assert_eq!(gen_dirs(m.directory()).len(), 2);
}

#[test]
fn base_excludes_dirty_and_untracked() {
    let fx = Fx::with(&["front"]);
    let main = fx.main("front");
    fx.sb.commit(
        &main,
        &[
            (".hidden", b"dot file\n" as &[u8]),
            (".gitignore", b".DS_Store\nnode_modules/\n.cache/\ngenerated/\n"),
            ("generated/kept.js", b"tracked but ignored\n"),
            ("assets/logo.png", b"text inside a png name\n"),
        ],
        "extras",
    );
    push(&fx, "front");
    let rep = fx.new_session(&["front"]);
    let ws = fx.ws(&rep, "front").path;
    // Staged new content, dirty tracked content and an untracked file.
    fx.sb.write(&ws, "staged.ts", b"staged only\n");
    fx.sb.git(&ws, &["add", "--", "staged.ts"]);
    fx.sb.write(&ws, "src/index.ts", b"dirty edit\n");
    fx.sb.write(&ws, "untracked.ts", b"untracked\n");
    // And the same kinds in the main checkout itself.
    fx.sb.write(&main, "main-untracked.ts", b"x\n");
    fx.sb.write(&main, "bun.lock", b"main dirty\n");

    let ov = overlays(&fx, &[(&rep, "front")]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    s.attach(&id(&rep), &fx.rn("front")).unwrap();
    let t = tree(&rep, "front");
    let ptr = s.generation_ptr(&fx.rn("front"), &t).unwrap();

    // Open the same generation through tgrep: the process-wide cache hands
    // back search's Arc.
    let repo = Repository::discover(&main).unwrap();
    let m = GenerationManager::with_storage(repo, &fx.root().join("search").join("front")).unwrap();
    let g = m.ensure(rep.manifest.repos[&fx.rn("front")].head.as_str(), product_profile(), None).unwrap().generation;
    assert_eq!(std::sync::Arc::as_ptr(g.base().reader()) as usize, ptr);
    assert_eq!(g.key().tree_oid(), t.as_str());

    let paths: Vec<&str> = g.entries().iter().map(|e| e.path.as_str()).collect();
    for p in [".gitignore", "package.json", "bun.lock", "src/index.ts", ".hidden", "generated/kept.js", "assets/logo.png"] {
        assert!(paths.contains(&p), "{p} missing from {paths:?}");
    }
    for p in ["staged.ts", "untracked.ts", "main-untracked.ts"] {
        assert!(!paths.contains(&p), "{p} is in the base: {paths:?}");
    }
    assert!(g.entry("src/index.ts").unwrap().matches_worktree_bytes(b"export {}\n"));
    assert!(g.entry("bun.lock").unwrap().matches_worktree_bytes(b"lock v1\n"));
    for p in [".hidden", "generated/kept.js", "assets/logo.png"] {
        let c = &g.entry(p).unwrap().content;
        assert!(matches!(c, EntryContent::Indexed { .. }), "{p}: {c:?}");
    }
    assert!(paths.iter().all(|p| !p.starts_with(".git/") && !p.starts_with(".strata")));
}

#[test]
fn ensure_failure_after_stage_is_unknown() {
    let fx = Fx::with(&["front"]);
    let rep = fx.new_session(&["front"]);
    let ov = overlays(&fx, &[(&rep, "front")]);
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    s.arm(SearchSeam::FailEnsureAfterStage);
    let e = s.attach(&id(&rep), &fx.rn("front")).expect_err("ensure must fail");
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(reason(&e), Some("index_build"), "{e:?}");
    let stage = PathBuf::from(detail_str(&e, "stage").expect("stage in detail"));
    assert!(stage.is_dir(), "stage {} is gone", stage.display());
    assert!(stage.file_name().unwrap().to_string_lossy().starts_with(".stage-"));
    let dir = s.storage_dir(&fx.rn("front")).unwrap();
    assert!(stage.starts_with(&dir));
    assert!(gen_dirs(&dir).is_empty());
    assert_eq!(s.generation_ptr(&fx.rn("front"), &tree(&rep, "front")), None);

    // Unarmed, the next attach builds and publishes past the stale stage.
    s.attach(&id(&rep), &fx.rn("front")).unwrap();
    assert_eq!(gen_dirs(&dir).len(), 1);

    // A failure before any stage is a named code and leaves no generation:
    // the session's head is no longer in the main checkout.
    let fx = Fx::with(&["front"]);
    let rep = fx.new_session(&["front"]);
    let ov = overlays(&fx, &[(&rep, "front")]);
    let main = fx.main("front");
    fx.sb.git(&main, &["checkout", "-q", "--orphan", "other"]);
    fx.sb.git(&main, &["rm", "-rq", "--cached", "."]);
    fx.sb.commit(&main, &[("other.txt", b"other\n" as &[u8])], "other");
    let refs = fx.sb.git(&main, &["for-each-ref", "--format=%(refname)"]);
    for r in refs.lines().filter(|r| *r != "refs/heads/other") {
        fx.sb.git(&main, &["update-ref", "-d", r]);
    }
    fx.sb.git(&main, &["reflog", "expire", "--expire=now", "--all"]);
    fx.sb.git(&main, &["gc", "-q", "--prune=now"]);
    let head = rep.manifest.repos[&fx.rn("front")].head.clone();
    assert!(!fx.sb.git_output(&main, &["cat-file", "-e", &format!("{head}^{{commit}}")]).status.success());

    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let e = s.attach(&id(&rep), &fx.rn("front")).expect_err("head is gone");
    assert_ne!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(e.code, Code::NoSuchCommit, "{e:?}");
    assert!(all_below(&fx.root().join("search"), "gen-").is_empty());
    assert!(all_below(&fx.root().join("search"), ".stage-").is_empty());
}
