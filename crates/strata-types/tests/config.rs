use std::path::PathBuf;

use strata_types::{Code, Config, Paths, PublishTarget, RepoConfig, RepoName, RepoPath};

fn repo(name: &str) -> RepoName {
    RepoName::try_from(name.to_owned()).unwrap()
}

fn minimal(extra_repo: &str) -> String {
    format!(
        r#"
[paths]
root = "/Volumes/GWD/inter/.strata"

{extra_repo}
"#
    )
}

fn bare_repo(name: &str, path: &str) -> String {
    format!(
        r#"
[repo.{name}]
path = "{path}"
default_branch = "main"
deps_dirs = []
publish = {{ branch = "main", push = true, confirm = false }}
"#
    )
}

fn parse(text: &str) -> Result<Config, strata_types::StrataError> {
    Config::parse(text)
}

fn rule(err: &strata_types::StrataError) -> String {
    err.detail.as_ref().unwrap()["rule"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn minimal_config_applies_defaults_and_locates_a_repo() {
    let cfg = parse(&minimal(&bare_repo("tool", "/Volumes/GWD/inter/tool"))).unwrap();
    assert_eq!(cfg.paths.root, PathBuf::from("/Volumes/GWD/inter/.strata"));
    assert_eq!(
        cfg.paths.store(),
        PathBuf::from("/Volumes/GWD/inter/.strata/store")
    );
    assert_eq!(
        cfg.paths.bases(),
        PathBuf::from("/Volumes/GWD/inter/.strata/bases")
    );
    assert_eq!(
        cfg.paths.deps(),
        PathBuf::from("/Volumes/GWD/inter/.strata/deps")
    );
    assert_eq!(
        cfg.paths.sessions(),
        PathBuf::from("/Volumes/GWD/inter/.strata/sessions")
    );
    let tool = cfg.repo(&repo("tool")).unwrap();
    assert!(tool.deps_dirs.is_empty());
    assert!(!tool.needs_human_confirm());
    assert!(tool.pin_unpushed);
    let (seeds, explicit) = tool.seed_files();
    assert!(!explicit);
    assert_eq!(seeds.len(), 4);
    assert_eq!(
        cfg.repo(&repo("missing")).unwrap_err().code,
        Code::NoSuchRepo
    );
}

#[test]
fn omitted_paths_and_deps_lists_use_documented_defaults() {
    let text = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
install_inputs = ["package.json"]
install = "bun install"
publish = { branch = "main", push = true, confirm = false }
"#;
    let cfg = parse(text).unwrap();
    assert_eq!(cfg.paths.root, Paths::default_root());
    let tool = cfg.repo(&repo("tool")).unwrap();
    assert_eq!(tool.deps_dirs, RepoConfig::default_deps_dirs());
    assert_eq!(tool.seed_caches, RepoConfig::default_seed_caches());
    assert!(tool.pin_unpushed);
}

#[test]
fn explicit_empty_seed_files_are_not_the_default_candidates() {
    let text = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = []
seed_files = []
publish = { branch = "main", push = true, confirm = false }
"#;
    let tool = parse(text).unwrap().repo(&repo("tool")).unwrap().clone();
    let (seeds, explicit) = tool.seed_files();
    assert!(explicit);
    assert!(seeds.is_empty());
}

#[test]
fn v1_name_must_match_path_basename_and_table_must_be_nonempty() {
    let err = parse(&minimal(&bare_repo("tool", "/Volumes/GWD/inter/other"))).unwrap_err();
    assert_eq!(err.code, Code::InvalidInput);
    assert_eq!(rule(&err), "V1");
    let empty = Config {
        paths: Paths::default(),
        repos: Default::default(),
    };
    assert_eq!(rule(&empty.validate().unwrap_err()), "V1");
}

#[test]
fn quoted_repo_keys_with_dots_are_legal_names() {
    let text = r#"
[repo."foo.bar"]
path = "/Volumes/GWD/inter/foo.bar"
default_branch = "main"
deps_dirs = []
publish = { branch = "main", push = true, confirm = false }
"#;
    assert!(parse(text).is_ok());
}

#[test]
fn v2_rejects_relative_dot_and_trailing_slash() {
    for path in [
        "tool",
        "/Volumes/GWD/inter/../tool",
        "/Volumes/GWD/inter/tool/",
        "/Volumes//GWD/inter/tool",
        "~/inter/tool",
    ] {
        let err = parse(&minimal(&bare_repo("tool", path))).unwrap_err();
        assert_eq!(rule(&err), "V2", "{path}");
    }
    let err = parse(&format!(
        "{}\n[paths]\nroot = \"relative\"\n",
        bare_repo("tool", "/Volumes/GWD/inter/tool")
    ));
    // paths table written twice is a toml error; use a single document instead.
    let _ = err;
    let text = r#"
[paths]
root = "relative/.strata"

[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = []
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(text).unwrap_err()), "V2");
}

#[test]
fn v3_rejects_root_nested_in_a_checkout_or_the_reverse() {
    let inside_checkout = r#"
[paths]
root = "/Volumes/GWD/inter/tool/.strata"

[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = []
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(inside_checkout).unwrap_err()), "V3");
    let checkout_inside_root = r#"
[paths]
root = "/Volumes/GWD/inter/.strata"

[repo.tool]
path = "/Volumes/GWD/inter/.strata/tool"
default_branch = "main"
deps_dirs = []
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(checkout_inside_root).unwrap_err()), "V3");
}

#[test]
fn v4_rejects_illegal_branch_names() {
    for branch in [
        "",
        "HEAD",
        "@",
        "-main",
        "refs/heads/main",
        "foo..bar",
        "foo@{1}",
        "foo//bar",
        "foo.lock",
        "a/b.lock",
        "has space",
        "has~tilde",
    ] {
        let text = format!(
            r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "{branch}"
deps_dirs = []
publish = {{ branch = "main", push = true, confirm = false }}
"#
        );
        let err = parse(&text).unwrap_err();
        assert_eq!(rule(&err), "V4", "{branch}");
    }
}

#[test]
fn deploy_on_push_is_information_and_confirm_is_the_only_gate() {
    let text = r#"
[repo.staff-api]
path = "/Volumes/GWD/inter/staff-api"
default_branch = "master"
deps_dirs = []
deploy_on_push = true
publish = { branch = "master", push = false, confirm = false }
"#;
    // V5 is gone: deploy_on_push = true with confirm = false parses and
    // does not ask a human.
    let cfg = parse(text).unwrap_or_else(|err| panic!("{err} {:?}", err.detail));
    let staff = cfg.repo(&repo("staff-api")).unwrap();
    assert!(staff.deploy_on_push);
    assert!(!staff.needs_human_confirm());
    let mut built = RepoConfig {
        path: "/Volumes/GWD/inter/staff-api".into(),
        default_branch: "master".into(),
        deps_dirs: vec![],
        seed_caches: vec![],
        seed_files: Some(vec![]),
        install_inputs: vec![],
        install: None,
        checks: vec![],
        publish: PublishTarget {
            branch: "master".into(),
            push: true,
            confirm: false,
        },
        deploy_on_push: true,
        pin_unpushed: true,
    };
    assert!(!built.needs_human_confirm());
    built.publish.confirm = true;
    assert!(built.needs_human_confirm());
    built.deploy_on_push = false;
    assert!(built.needs_human_confirm());
}

#[test]
fn v6_rejects_git_duplicates_and_nesting_inside_one_list() {
    let text = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = [".git"]
install_inputs = ["package.json"]
install = "bun install"
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(text).unwrap_err()), "V6");
    let nested = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = ["node_modules", "node_modules/pkg"]
install_inputs = ["package.json"]
install = "bun install"
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(nested).unwrap_err()), "V6");
}

#[test]
fn v7_allows_seed_cache_under_deps_and_rejects_other_overlap() {
    let ok = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = ["node_modules"]
seed_caches = ["node_modules/.cache"]
install_inputs = ["package.json"]
install = "bun install"
publish = { branch = "main", push = true, confirm = false }
"#;
    assert!(parse(ok).is_ok());
    let equal = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = ["node_modules"]
seed_caches = ["node_modules"]
install_inputs = ["package.json"]
install = "bun install"
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(equal).unwrap_err()), "V7");
    let seed_file_under_deps = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = ["node_modules"]
seed_files = ["node_modules/.env"]
install_inputs = ["package.json"]
install = "bun install"
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(seed_file_under_deps).unwrap_err()), "V7");
}

#[test]
fn v8_requires_install_and_inputs_exactly_when_deps_dirs_are_present() {
    let deps_without_install = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = ["node_modules"]
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(deps_without_install).unwrap_err()), "V8");
    let install_without_deps = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = []
install = "bun install"
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(install_without_deps).unwrap_err()), "V8");
    let inputs_without_deps = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = []
install_inputs = ["package.json"]
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(inputs_without_deps).unwrap_err()), "V8");
}

#[test]
fn design_6_7_deps_repos_must_publish_the_default_branch_and_pin_unpushed() {
    let unpinned = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = ["node_modules"]
install_inputs = ["package.json"]
install = "bun install"
pin_unpushed = false
publish = { branch = "main", push = true, confirm = false }
"#;
    let err = parse(unpinned).unwrap_err();
    assert_eq!(err.code, Code::InvalidInput);
    assert_eq!(rule(&err), "design-6.7");
    let other_branch = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = ["node_modules"]
install_inputs = ["package.json"]
install = "bun install"
publish = { branch = "dev", push = true, confirm = false }
"#;
    assert_eq!(rule(&parse(other_branch).unwrap_err()), "design-6.7");
    let no_deps = r#"
[repo.staff-api]
path = "/Volumes/GWD/inter/staff-api"
default_branch = "master"
deps_dirs = []
pin_unpushed = false
publish = { branch = "release", push = false, confirm = true }
"#;
    assert!(parse(no_deps).is_ok(), "{:?}", parse(no_deps).err());
}

fn with_check(line: &str) -> String {
    let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = []
checks = ["{escaped}"]
publish = {{ branch = "main", push = true, confirm = false }}
"#
    )
}

#[test]
fn v9_rejects_shell_metacharacters_and_v10_rejects_mutating_checks() {
    assert_eq!(
        rule(&parse(&with_check("tsc && echo ok")).unwrap_err()),
        "V9"
    );
    for line in [
        "prettier --write .",
        "sh -c \"prettier --write .\"",
        "composer cs-fix -- src/Foo.php",
    ] {
        let err = parse(&with_check(line)).unwrap_err();
        assert_eq!(err.code, Code::CheckMutates, "{line}");
        assert_eq!(rule(&err), "V10", "{line}");
    }
    for line in [
        "composer cs-fix -- --dry-run --diff src/Foo.php",
        "vendor/bin/php-cs-fixer check --diff",
        "bunx tsc --noEmit --incremental false",
    ] {
        assert!(
            parse(&with_check(line)).is_ok(),
            "{line}: {:?}",
            parse(&with_check(line)).err()
        );
    }
}

#[test]
fn validate_commands_reports_repo_null_until_config_validate_fills_it() {
    let cfg = RepoConfig {
        path: "/Volumes/GWD/inter/tool".into(),
        default_branch: "main".into(),
        deps_dirs: vec![],
        seed_caches: vec![],
        seed_files: Some(vec![]),
        install_inputs: vec![],
        install: None,
        checks: vec!["prettier --write .".into()],
        publish: PublishTarget {
            branch: "main".into(),
            push: true,
            confirm: false,
        },
        deploy_on_push: false,
        pin_unpushed: true,
    };
    let err = cfg.validate_commands().unwrap_err();
    assert_eq!(err.code, Code::CheckMutates);
    assert!(err.detail.unwrap()["repo"].is_null());
}

#[test]
fn unknown_keys_are_v11_and_toml_syntax_errors_keep_the_parser_message() {
    let text = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = []
extra = true
publish = { branch = "main", push = true, confirm = false }
"#;
    let err = parse(text).unwrap_err();
    assert_eq!(err.code, Code::InvalidInput);
    assert_eq!(rule(&err), "V11");
    assert!(
        err.detail.unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("unknown field")
    );
    let err = parse("[").unwrap_err();
    assert_eq!(rule(&err), "toml");
    assert!(err.detail.unwrap()["message"].is_string());
}

#[test]
fn only_the_first_violation_in_file_order_is_reported() {
    let text = r#"
[repo.zzz]
path = "/Volumes/GWD/inter/zzz"
default_branch = "main"
deps_dirs = []
checks = ["prettier --write ."]
publish = { branch = "main", push = true, confirm = false }

[repo.aaa]
path = "/not-the-basename"
default_branch = "main"
deps_dirs = []
publish = { branch = "main", push = true, confirm = false }
"#;
    let err = parse(text).unwrap_err();
    assert_eq!(rule(&err), "V10");
    assert_eq!(err.detail.unwrap()["repo"], "zzz");
    let text = r#"
[repo.zzz]
path = "/Volumes/GWD/inter/other"
default_branch = "main"
deps_dirs = []
publish = { branch = "main", push = true, confirm = false }

[repo.aaa]
path = "/Volumes/GWD/inter/aaa"
default_branch = "main"
deps_dirs = []
checks = ["prettier --write ."]
publish = { branch = "main", push = true, confirm = false }
"#;
    let err = parse(text).unwrap_err();
    assert_eq!(rule(&err), "V1");
    assert_eq!(err.detail.as_ref().unwrap()["repo"], "zzz");
}

#[test]
fn repo_path_lists_reject_illegal_entries_at_parse() {
    let text = r#"
[repo.tool]
path = "/Volumes/GWD/inter/tool"
default_branch = "main"
deps_dirs = ["../outside"]
install_inputs = ["package.json"]
install = "bun install"
publish = { branch = "main", push = true, confirm = false }
"#;
    assert_eq!(parse(text).unwrap_err().code, Code::InvalidInput);
    let _ = RepoPath::try_from("ok".to_owned()).unwrap();
}

#[test]
fn default_inter_config_parses() {
    let text = include_str!("../default-config.toml");
    let cfg = parse(text).unwrap_or_else(|err| panic!("{err} {:?}", err.detail));
    assert!(cfg.repos.len() > 10, "{}", cfg.repos.len());
    assert!(
        cfg.repo(&repo("tool")).unwrap().needs_human_confirm()
            || !cfg.repo(&repo("tool")).unwrap().deploy_on_push
    );
    // staff-api: deploys on push but publishes headless (2026-10-26).
    assert!(!cfg.repo(&repo("staff-api")).unwrap().needs_human_confirm());
    assert!(cfg.repo(&repo("staff-api")).unwrap().deploy_on_push);
    assert!(!cfg.repo(&repo("staff-api")).unwrap().publish.push);
    assert!(cfg.repo(&repo("front")).unwrap().deploy_on_push);
    assert!(cfg.repo(&repo("front")).unwrap().needs_human_confirm());
}
