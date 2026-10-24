//! Task 3: read-only checks (publish.md §5.5, D3, D4).

mod support;

use std::fs;
use std::path::PathBuf;

use strata_publish::{Staged, check, stage};
use strata_types::{Code, CommitOid, RepoConfig};
use support::*;

const ORIGIN: &str = "refs/strata/remotes/origin/main";

/// A staged layout for `tool` (one edited file) in a session over `repos`.
fn staged(fx: &Fx, repos: &[&str]) -> (Staged, strata_types::SessionId) {
    fx.git_of("tool").fetch("origin", "main").unwrap();
    for r in repos {
        fx.git_of(r).fetch("origin", "main").unwrap();
    }
    let base = CommitOid::try_from(fx.read_ref("tool", ORIGIN).unwrap()).unwrap();
    let s = fx.new_session(repos);
    fx.edit(&s, "tool", &[("src/index.ts", b"export const a = 2\n")]);
    let c = fx.checkpoint(&s, "tool");
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let st = stage(&ctx, &group(vec![batch(&s, "tool", c.id, &["src/index.ts"])]), &base).unwrap();
    (st, s)
}

fn cfg_with(fx: &Fx, checks: &[String]) -> RepoConfig {
    let mut cfg = fx.config.repos[&repo_name("tool")].clone();
    cfg.checks = checks.to_vec();
    cfg
}

/// A check program that records its argv and cwd under `state/`.
fn recorder(fx: &Fx, name: &str, body: &str) -> String {
    let st = fx.sb.state.display();
    fx.sb.write_program(name, &format!("#!/bin/sh\necho \"$0 $*\" >> \"{st}/checks.log\"\n{body}\n"));
    fx.sb.bin.join(name).display().to_string()
}

fn log(fx: &Fx) -> String {
    fs::read_to_string(fx.sb.state.join("checks.log")).unwrap_or_default()
}

#[test]
fn check_mutating_command_is_config_error() {
    let fx = Fx::with(&["tool"]);
    let fmt = recorder(&fx, "fmt", "exit 0");
    let csfix = recorder(&fx, "cs-fix", "exit 0");
    let ok = recorder(&fx, "ok", "exit 0");
    let dir = fx.sb.root.join("nothing-here");
    for checks in [vec![ok.clone(), format!("{fmt} --write src")], vec![format!("{csfix} src"), ok.clone()]] {
        let e = check(&cfg_with(&fx, &checks), &dir).unwrap_err();
        assert_eq!(e.code, Code::CheckMutates, "{e:?}");
    }
    // `cs-fix --dry-run` is read-only.
    let cfg = cfg_with(&fx, &[format!("{csfix} --dry-run src")]);
    cfg.validate_commands().unwrap();
    assert_eq!(log(&fx), "", "no check process started");
    assert!(!dir.exists());
}

#[test]
fn check_failed_does_not_commit() {
    let fx = Fx::with(&["tool"]);
    let (st, _s) = staged(&fx, &["tool"]);
    let layout = st.layout_root().to_path_buf();
    let fail = recorder(&fx, "fails", "echo some output; echo to stderr >&2; exit 3");
    let never = recorder(&fx, "never", "exit 0");
    let e = check(&cfg_with(&fx, &[fail.clone(), never.clone()]), &st.worktree).unwrap_err();
    assert_eq!(e.code, Code::CheckFailed, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["command"], fail);
    assert_eq!(d["exit_code"], 3);
    let out = d["output_tail"].as_str().unwrap();
    assert!(out.contains("some output") && out.contains("to stderr"), "{out}");
    assert!(!log(&fx).contains("never"), "later checks do not run");
    assert!(!layout.exists());
    assert!(!st.worktree.join(".git").exists());
    assert_eq!(fx.read_ref("tool", &strata_publish::chain_ref("main")), None);

    // A layout that cannot be removed → Unknown, not CheckFailed.
    let (st, _s2) = staged(&fx, &["tool"]);
    // The check leaves a directory behind; while removing, it is swapped
    // for a symlink just before it is opened, so the removal cannot finish.
    let inner = st.worktree.join("inner");
    let mk = recorder(&fx, "leaves", &format!("mkdir -p '{}/x' && exit 1", inner.display()));
    let swap = inner.clone();
    strata_apfs::seams::set_before_open(Some(Box::new(move |_, name| {
        if name == "inner" && swap.is_dir() && !swap.is_symlink() {
            fs::rename(&swap, swap.with_file_name("inner.gone")).unwrap();
            std::os::unix::fs::symlink("/nonexistent", &swap).unwrap();
        }
    })));
    let e = check(&cfg_with(&fx, &[mk]), &st.worktree).unwrap_err();
    strata_apfs::seams::set_before_open(None);
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let cause = &e.detail.as_ref().unwrap()["cause"];
    assert_eq!(cause["code"], "CheckFailed");
    assert_eq!(cause["detail"]["exit_code"], 1);
    assert!(st.layout_root().exists());
    fs::remove_dir_all(st.layout_root()).unwrap();
}

#[test]
fn zero_checks_reports_zero_ran() {
    let fx = Fx::with(&["tool"]);
    let (st, _s) = staged(&fx, &["tool"]);
    assert_eq!(check(&cfg_with(&fx, &[]), &st.worktree).unwrap().ran, 0);
    let ok = recorder(&fx, "ok", "exit 0");
    assert_eq!(check(&cfg_with(&fx, &[ok.clone(), format!("{ok} again")]), &st.worktree).unwrap().ran, 2);
    assert!(st.worktree.exists(), "a passing check keeps the layout");
}

#[test]
fn sibling_layout_matches_workspace() {
    let fx = Fx::new();
    let (st, s) = staged(&fx, &["front", "tool"]);
    let out = fx.sb.state.join("sibling.txt");
    let probe = recorder(
        &fx,
        "probe",
        &format!(
            "{{ pwd -P; cd ../front && pwd -P; cd - >/dev/null; git rev-parse --show-toplevel; \
             git ls-files --error-unmatch bun.lock; cat ../front/package.json | head -1; }} > '{}' 2>&1",
            out.display()
        ),
    );
    let ran = check(&cfg_with(&fx, &[probe]), &st.worktree).unwrap();
    assert_eq!(ran.ran, 1);
    let text = fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let canon = |p: PathBuf| fs::canonicalize(p).unwrap().display().to_string();
    assert_eq!(lines[0], canon(st.worktree.clone()));
    assert_eq!(lines[1], canon(st.layout_root().join("front")));
    assert_ne!(lines[1], canon(fx.ws(&s, "front")));
    assert_eq!(lines[2], canon(st.worktree.clone()), "{text}");
    assert_eq!(lines[3], "bun.lock", "{text}");
    assert_eq!(lines[4], "{");
    assert!(st.worktree.join(".git").exists());
    assert!(!fx.ws(&s, "front").starts_with(st.layout_root()));
}

/// Review H3: a check that runs an executable present only in the
/// publishing repo's sealed deps set (`node_modules/.bin/lint`) finds it in
/// staging. The staged copy is a private clone (writing it leaves the
/// sealed set alone) and the deps dir does not enter the staged tree.
#[test]
fn staging_has_the_publishing_repos_sealed_deps() {
    let fx = Fx::with(&["tool"]);
    fx.sb.set_install_script(&format!(
        "{DEFAULT_INSTALL}\nprintf '#!/bin/sh\\nexit 0\\n' > node_modules/.bin/lint && chmod +x node_modules/.bin/lint || exit 1\n"
    ));
    let (st, s) = staged(&fx, &["tool"]);
    let lint = st.worktree.join("node_modules/.bin/lint");
    assert!(lint.is_file(), "the sealed deps are in staging");
    let ran = check(&cfg_with(&fx, &["node_modules/.bin/lint".to_string()]), &st.worktree).unwrap();
    assert_eq!(ran.ran, 1);

    // A private clone: the sealed set is untouched by a write in staging.
    let rec = fx.store().session(&s).unwrap().unwrap();
    let set = fx.store().deps_dir(&repo_name("tool"), &rec.manifest.repos[&repo_name("tool")].base.deps);
    let sealed = set.join("node_modules/x/index.js");
    let before = fs::read(&sealed).unwrap();
    fs::write(st.worktree.join("node_modules/x/index.js"), b"changed in staging\n").unwrap();
    assert_eq!(fs::read(&sealed).unwrap(), before);

    // Not part of the commit tree.
    let names = fx.sb.git(&fx.main("tool"), &["ls-tree", "-r", "--name-only", st.tree.as_str()]);
    assert!(!names.contains("node_modules"), "{names}");
}
