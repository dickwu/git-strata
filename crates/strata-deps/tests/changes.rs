//! main-sync §7.2: real Git blobs in the existing isolated sandbox.
mod common;
use common::*;
use std::fs;
use strata_deps::{ChangeKind, changes};
use strata_types::CommitOid;
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

// The response intentionally gives up after 200 ms under load. Like the GUI's next poll,
// retry only that transient refusal; parser/data errors remain test failures.
fn ready(
    git: &strata_gitio::Git,
    cfg: &strata_types::RepoConfig,
    from: &CommitOid,
    to: &CommitOid,
) -> strata_deps::Result<strata_deps::Changes> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let result = changes(git, cfg, from, to);
        if let Err(e) = &result {
            let reason = e.detail.as_ref().and_then(|v| v["reason"].as_str());
            if matches!(reason, Some("changes_busy" | "changes_budget" | "timeout"))
                && std::time::Instant::now() < until
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
        }
        return result;
    }
}

#[test]
fn slow_summary_finishes_in_background_and_next_poll_uses_cache() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let sb = Sandbox::new();
    let main = sb.init_project("main");
    let from = CommitOid::try_from(sb.commit(
        &main,
        &[("package.json", br#"{"dependencies":{"slow":"1"}}"#)],
        "before",
    ))
    .unwrap();
    let to = CommitOid::try_from(sb.commit(
        &main,
        &[("package.json", br#"{"dependencies":{"slow":"2"}}"#)],
        "after",
    ))
    .unwrap();
    let git = sb.open(&main);
    let mut cfg = sb.config(&main);
    cfg.install_inputs = rps(&["package.json"]);
    let wrapper = sb.bin.join("git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = cat-file ]; then /bin/sleep 0.4; fi\ndone\nexec /usr/bin/git \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(sb.bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    git.set_env("PATH", Some(&path));
    git.take_git_calls();
    let start = Instant::now();
    let err = changes(&git, &cfg, &from, &to).unwrap_err();
    assert_eq!(err.detail.unwrap()["reason"], "changes_budget");
    assert!(start.elapsed() < Duration::from_millis(600));
    let err = changes(&git, &cfg, &from, &to).unwrap_err();
    assert_eq!(err.detail.unwrap()["reason"], "changes_busy");
    let result = ready(&git, &cfg, &from, &to).unwrap();
    assert_eq!(result.summary, "slow 1→2");
    assert_eq!(
        git.take_git_calls()
            .iter()
            .filter(|c| c.args.iter().any(|a| a == "cat-file"))
            .count(),
        2,
        "only the first background worker reads the two blobs"
    );
    assert_eq!(ready(&git, &cfg, &from, &to).unwrap(), result);
    assert!(
        git.take_git_calls()
            .iter()
            .all(|c| !c.args.iter().any(|a| a == "cat-file"))
    );
}

#[test]
fn unsupported_lockfiles_never_copy_url_credentials_into_fallbacks() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let sb = Sandbox::new();
    let main = sb.init_project("main");
    let from = CommitOid::try_from(sb.commit(&main, &[], "before")).unwrap();
    let to = CommitOid::try_from(sb.commit(
        &main,
        &[
            ("pnpm-lock.yaml", b"resolution: https://user:token@host/pkg"),
            ("yarn.lock", b"resolved https://user:token@host/pkg"),
        ],
        "after",
    ))
    .unwrap();
    let git = sb.open(&main);
    let mut cfg = sb.config(&main);
    cfg.install_inputs = rps(&["pnpm-lock.yaml", "yarn.lock"]);
    let result = ready(&git, &cfg, &from, &to).unwrap();
    assert!(result.packages.is_empty());
    assert_eq!(
        result.summary,
        "pnpm-lock.yaml changed (no parser), yarn.lock changed (no parser)"
    );
}

#[test]
fn lockfile_and_manifest_parser_fixtures() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for file in [
        "bun.lock",
        "package-lock.json",
        "composer.lock",
        "pubspec.lock",
        "Cargo.lock",
        "package.json",
        "composer.json",
        "pubspec.yaml",
        "Cargo.toml",
    ] {
        let sb = Sandbox::new();
        let main = sb.init_project("main");
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/changes");
        let old = fs::read(fixture.join("from").join(file)).unwrap();
        let new = fs::read(fixture.join("to").join(file)).unwrap();
        let manifest = match file {
            "composer.lock" => Some("composer.json"),
            "Cargo.lock" => Some("Cargo.toml"),
            _ => None,
        };
        let mut old_files = vec![(file, old.as_slice())];
        let mut new_files = vec![(file, new.as_slice())];
        let old_manifest = manifest.map(|m| fs::read(fixture.join("from").join(m)).unwrap());
        let new_manifest = manifest.map(|m| fs::read(fixture.join("to").join(m)).unwrap());
        if let Some(m) = manifest {
            old_files.push((m, old_manifest.as_ref().unwrap()));
            new_files.push((m, new_manifest.as_ref().unwrap()));
        }
        let from = CommitOid::try_from(sb.commit(&main, &old_files, "before")).unwrap();
        let to = CommitOid::try_from(sb.commit(&main, &new_files, "after")).unwrap();
        let git = sb.open(&main);
        let mut cfg = sb.config(&main);
        cfg.install_inputs = rps(&[file]);
        let result = ready(&git, &cfg, &from, &to).unwrap_or_else(|e| panic!("{file}: {e:?}"));
        assert!(!result.truncated, "{file}: {result:?}");
        assert_eq!(result.packages.len(), 3, "{file}: {result:?}");
        let react = result.packages.iter().find(|p| p.name == "react").unwrap();
        assert_eq!(react.from.as_deref(), Some("18.2.0"), "{file}");
        assert_eq!(react.to.as_deref(), Some("19.0.0"), "{file}");
        assert_eq!(react.kind, ChangeKind::Changed);
        assert!(react.direct, "{file}");
        assert_eq!(
            result
                .packages
                .iter()
                .find(|p| p.name == "old")
                .unwrap()
                .kind,
            ChangeKind::Removed
        );
        assert_eq!(
            result
                .packages
                .iter()
                .find(|p| p.name == "zod")
                .unwrap()
                .kind,
            ChangeKind::Added
        );
        assert!(result.summary.chars().count() <= 120);
        assert_eq!(
            ready(&git, &cfg, &from, &to).unwrap(),
            result,
            "cached {file}"
        );
    }
}

#[test]
fn fallback_caps_and_cache_use_blobs_not_heads() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let sb = Sandbox::new();
    let main = sb.init_project("main");
    let git = sb.open(&main);
    let mut cfg = sb.config(&main);
    cfg.install_inputs = rps(&["bun.lockb", "package.json", "patches"]);
    let from = CommitOid::try_from(sb.commit(&main, &[("bun.lockb", b"old")], "before")).unwrap();
    let to = CommitOid::try_from(sb.commit(
        &main,
        &[
            ("bun.lockb", b"new"),
            ("package.json", br#"{"dependencies":{"z":"^2"}}"#),
            ("patches/a.patch", b"new patch"),
        ],
        "after",
    ))
    .unwrap();
    let result = ready(&git, &cfg, &from, &to).unwrap();
    assert!(result.packages.iter().any(|p| p.name == "z" && p.direct));
    assert_eq!(
        result.packages.iter().filter(|p| p.name == "z").count(),
        1,
        "manifest fallback not duplicated"
    );
    assert!(
        result
            .summary
            .contains("patches/a.patch changed (no parser)"),
        "{result:?}"
    );
    let source_only =
        CommitOid::try_from(sb.commit(&main, &[("src/index.ts", b"// source only")], "source"))
            .unwrap();
    git.take_git_calls();
    assert_eq!(ready(&git, &cfg, &from, &source_only).unwrap(), result);
    assert!(
        !git.take_git_calls()
            .iter()
            .any(|c| c.args.iter().any(|a| a == "cat-file")),
        "blob-pair cache avoids reading/parsing blobs again"
    );

    let large = vec![b'x'; 16 * 1024 * 1024 + 1];
    let oversized =
        CommitOid::try_from(sb.commit(&main, &[("large.lock", &large)], "oversized")).unwrap();
    cfg.install_inputs = rps(&["large.lock"]);
    git.take_git_calls();
    let result = ready(&git, &cfg, &source_only, &oversized).unwrap();
    assert_eq!(result.summary, "large.lock changed (no parser)");
    assert!(result.truncated);
    assert!(
        !git.take_git_calls()
            .iter()
            .any(|c| c.args.iter().any(|a| a == "cat-file")),
        "oversized blobs never read"
    );
}

#[test]
fn removed_lockfile_and_manifest_context_invalidate_cache() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let sb = Sandbox::new();
    let main = sb.init_project("main");
    let git = sb.open(&main);
    let mut cfg = sb.config(&main);
    cfg.install_inputs = rps(&["composer.lock"]);
    let old = br#"{"packages":[{"name":"org/a","version":"1"}]}"#;
    let new = br#"{"packages":[{"name":"org/a","version":"2"}]}"#;
    let from = CommitOid::try_from(sb.commit(
        &main,
        &[("composer.lock", old), ("composer.json", b"{}")],
        "old",
    ))
    .unwrap();
    let to = CommitOid::try_from(sb.commit(&main, &[("composer.lock", new)], "new")).unwrap();
    assert!(!ready(&git, &cfg, &from, &to).unwrap().packages[0].direct);
    let direct = CommitOid::try_from(sb.commit(
        &main,
        &[("composer.json", br#"{"require":{"org/a":"2"}}"#)],
        "direct",
    ))
    .unwrap();
    assert!(ready(&git, &cfg, &from, &direct).unwrap().packages[0].direct);
    sb.git(&main, &["rm", "composer.lock"]);
    let removed = CommitOid::try_from(sb.commit(&main, &[], "removed")).unwrap();
    let r = ready(&git, &cfg, &direct, &removed).unwrap();
    assert_eq!(r.packages[0].kind, ChangeKind::Removed);
    assert_eq!(r.packages[0].from.as_deref(), Some("2"));
    assert_eq!(r.packages[0].to, None);
}
