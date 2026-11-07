//! Auto-close §10 row 12 clauses Claude r6 still marked missing:
//! (1) per-repo `Some(true)` under global `false` enables a close;
//! (2) `docs/config.md` Appendix A TOML still parses as a valid overlay.
//! Temp-dir fixtures only for (1). No production edits.
mod support;

use std::fs;
use std::path::Path;

use serde_json::json;
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::Daemon;
use strata_store::Lifecycle;
use strata_types::{Config, RepoName};
use support::close::{self, mark_of, ok};
use support::*;

/// Global off, this session's only repo opted in → a qualifying publish closes.
///
/// Complements `auto_close_disabled_judges_nothing`, which keeps a two-repo
/// session open when the sibling still inherits global `false`. Without this
/// scene a `resolves` that only ever opted *out* would still pass every test
/// (Claude r6 blocker 1; `docs/config.md:84`).
#[test]
fn per_repo_true_under_global_false_closes() {
    let mut fx = Dfx::with(&["tool"]);
    fx.config.publish.auto_close = false;
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .auto_close = Some(true);
    fx.write_config();

    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let root = core.store.session(&s).unwrap().unwrap().manifest.root.clone();

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine\n").unwrap();
    let out = ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));

    assert_eq!(out["closed"], true, "{out}");
    let entry = &out["auto_close"]["sessions"][s.as_str()];
    assert_eq!(entry["closed"], true, "{entry}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    assert!(
        core.gits[&repo_name("tool")]
            .read_ref(&format!("refs/strata/pins/{s}"))
            .unwrap()
            .is_none(),
        "pin should be gone"
    );
    assert!(!root.exists(), "session root should leave the live path");
    assert!(mark_of(&core, &s).is_none(), "no session_marks row after close");
}

/// Global off with no per-repo override stays disabled (control for the scene
/// above: inheritance, not a second opt-in path).
#[test]
fn global_false_without_per_repo_stays_disabled() {
    let mut fx = Dfx::with(&["tool"]);
    fx.config.publish.auto_close = false;
    fx.write_config();

    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine\n").unwrap();
    let out = ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));

    assert_eq!(out["closed"], false, "{out}");
    let entry = &out["auto_close"]["sessions"][s.as_str()];
    assert_eq!(entry["reasons"], json!(["disabled"]), "{entry}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    assert!(mark_of(&core, &s).is_none(), "disabled writes no mark");
}

/// Concatenate every ` ```toml ` fence under `## 附录 A.` … `## 附录 B.`.
fn appendix_a_toml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/config.md");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = text
        .find("## 附录 A.")
        .unwrap_or_else(|| panic!("docs/config.md missing Appendix A heading"));
    let rest = &text[start..];
    let end = rest
        .find("## 附录 B.")
        .unwrap_or_else(|| panic!("docs/config.md missing Appendix B heading after A"));
    let section = &rest[..end];
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(rel) = section[cursor..].find("```toml") {
        let open = cursor + rel;
        let after_fence = section[open..]
            .find('\n')
            .map(|i| open + i + 1)
            .unwrap_or_else(|| panic!("toml fence without newline at {open}"));
        let close = section[after_fence..]
            .find("```")
            .map(|i| after_fence + i)
            .unwrap_or_else(|| panic!("unclosed toml fence at {after_fence}"));
        blocks.push(&section[after_fence..close]);
        cursor = close + 3;
    }
    assert!(
        !blocks.is_empty(),
        "Appendix A has no ```toml fences in docs/config.md"
    );
    blocks.join("\n")
}

fn repo(name: &str) -> RepoName {
    RepoName::try_from(name.to_owned()).unwrap()
}

/// Design §10 row 12 / Claude r6: Appendix A's documented overlay still parses.
/// `examples/config.toml` is a different file (`example_config_parses`); this
/// one reads `docs/config.md` itself.
#[test]
fn appendix_a_of_config_md_parses() {
    let cfg = Config::parse(&appendix_a_toml()).unwrap_or_else(|err| {
        panic!("Appendix A parse failed: {err} {:?}", err.detail);
    });

    assert_eq!(cfg.repos.len(), 22, "Appendix A covers the 22-repo inter workspace");
    // Appendix A leaves `[publish]` and per-repo `auto_close` unset → product defaults.
    assert!(
        cfg.publish.auto_close,
        "absent [publish] table must default auto_close = true"
    );
    for (name, rc) in &cfg.repos {
        assert_eq!(
            rc.publish.auto_close, None,
            "{name}: Appendix A inherits global auto_close, no per-repo override"
        );
    }

    // Keys the doc itself documents for the first front-end trio (§2.2 / A.2).
    let admin = cfg.repo(&repo("admin")).unwrap();
    assert_eq!(admin.default_branch, "master");
    assert_eq!(admin.publish.branch, "master");
    assert!(admin.publish.push);
    assert!(!admin.publish.confirm);
    assert!(!admin.deploy_on_push);
    assert_eq!(admin.publish.auto_close, None);

    let front = cfg.repo(&repo("front")).unwrap();
    assert_eq!(front.default_branch, "main");
    assert!(front.publish.confirm, "front push deploys; confirm required");
    assert!(front.deploy_on_push);
    assert_eq!(front.publish.auto_close, None);

    let tool = cfg.repo(&repo("tool")).unwrap();
    assert!(!tool.publish.confirm);
    assert!(!tool.deploy_on_push);
    assert!(
        tool.check_siblings.len() >= 7,
        "tool check_siblings lists the seven linked packages: {:?}",
        tool.check_siblings
    );

    assert_eq!(
        cfg.paths.root,
        std::path::PathBuf::from("/Volumes/GWD/inter/.strata")
    );
}
