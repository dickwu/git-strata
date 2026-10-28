//! publish.md D21: the check siblings. Publish supplies every repo named in
//! `check_siblings` at `../<name>` of the staged repo (the layout session's
//! pin, else the sibling's current main-tip base) or refuses up front.

mod support;

use std::fs;
use std::path::PathBuf;

use strata_publish::seams::{self, FailPoint, PublishFault};
use strata_publish::siblings::{self, Prepared, Supply, Why};
use strata_publish::{Published, publish, stage};
use strata_session::Layout;
use strata_types::{Code, CommitOid, ReleaseGroup, SessionId, StrataError};
use support::*;

const ORIGIN: &str = "refs/strata/remotes/origin/main";

/// tool and admin; tool's checks look at `../admin` and record what they saw.
fn fx() -> Fx {
    let mut fx = Fx::with(&["tool", "admin"]);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    fx.git_of("admin").fetch("origin", "main").unwrap();
    let st = fx.sb.state.display().to_string();
    fx.sb.write_program(
        "sibcheck",
        &format!(
            "#!/bin/sh\necho run >> \"{st}/sib.log\"\nif [ -f ../admin/package.json ]; then echo admin-present >> \"{st}/sib.log\"; else echo admin-absent >> \"{st}/sib.log\"; fi\ncat ../admin/marker.txt >> \"{st}/sib.log\" 2>/dev/null\nexit 0\n"
        ),
    );
    let prog = fx.sb.bin.join("sibcheck").display().to_string();
    let rn = fx.rn("admin");
    let cfg = fx.cfg_mut("tool");
    cfg.checks = vec![prog];
    cfg.check_siblings = vec![rn];
    cfg.publish.confirm = false;
    fx
}

fn sib_log(fx: &Fx) -> String {
    fs::read_to_string(fx.sb.state.join("sib.log")).unwrap_or_default()
}

/// A session of `repos` only (D51 `Layout::Only`): no untracked siblings.
fn slim(fx: &Fx, repos: &[&str]) -> SessionId {
    let rs: Vec<_> = repos.iter().map(|r| fx.rn(r)).collect();
    fx.sessions().new_with(None, &rs, Layout::Only).unwrap().manifest.id
}

fn edit(fx: &Fx, repos: &[&str]) -> (SessionId, ReleaseGroup) {
    let s = slim(fx, repos);
    fx.edit(&s, "tool", &[("src/index.ts", b"export const a = 9\n")]);
    let c = fx.checkpoint(&s, "tool");
    (s.clone(), group(vec![batch(&s, "tool", c.id, &["src/index.ts"])]))
}

fn run(fx: &Fx, g: &ReleaseGroup) -> Result<Published, StrataError> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    publish(&ctx, g, None)
}

fn layouts(fx: &Fx, s: &SessionId) -> Vec<PathBuf> {
    let root = fx.store().session(s).unwrap().unwrap().manifest.root;
    fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(".strata-publish-"))
        .collect()
}

fn reason(e: &StrataError) -> String {
    e.detail.as_ref().and_then(|d| d.get("reason")).and_then(|r| r.as_str()).unwrap_or_default().to_string()
}

/// A refusal before the row: no publish row, no layout, nothing on origin.
fn assert_refused_up_front(fx: &Fx, s: &SessionId, origin: &str) {
    assert!(fx.store().publishes().unwrap().is_empty(), "no publish row");
    assert!(layouts(fx, s).is_empty(), "no layout");
    assert_eq!(fx.origin_head("tool", "main"), origin);
    assert!(!sib_log(fx).contains("run"), "no check ran");
}

#[test]
fn sibling_outside_the_session_comes_from_its_main_tip() {
    let fx = fx();
    let (s, g) = edit(&fx, &["tool"]);
    // admin's origin moved after the session was made: the main tip wins.
    fx.land_on_origin("admin", "main", &[("marker.txt", b"admin-main-tip\n")], "admin moves");
    let before = fx.lock.holds_of("admin");
    let p = run(&fx, &g).unwrap();
    let log = sib_log(&fx);
    assert!(log.contains("admin-present") && log.contains("admin-main-tip"), "{log}");
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
    assert!(layouts(&fx, &s).is_empty(), "the layout is removed after the publish");
    assert!(fx.lock.holds_of("admin") > before, "admin was fetched under its own lock");
}

#[test]
fn sibling_in_the_layout_session_uses_its_pin() {
    let fx = fx();
    let (_s, g) = edit(&fx, &["tool", "admin"]);
    fx.land_on_origin("admin", "main", &[("marker.txt", b"admin-main-tip\n")], "admin moves");
    let before = fx.lock.holds_of("admin");
    run(&fx, &g).unwrap();
    let log = sib_log(&fx);
    assert!(log.contains("admin-present"), "{log}");
    assert!(!log.contains("admin-main-tip"), "the pin is used, not admin's main tip: {log}");
    assert_eq!(fx.lock.holds_of("admin") - before, 0, "a pinned sibling is not fetched");
}

#[test]
fn no_checks_means_no_siblings_prepared() {
    let mut fx = fx();
    fx.cfg_mut("tool").checks.clear();
    let (_s, g) = edit(&fx, &["tool"]);
    let before = fx.lock.holds_of("admin");
    run(&fx, &g).unwrap();
    assert_eq!(fx.lock.holds_of("admin") - before, 0);
}

#[test]
fn unfetchable_sibling_refuses_before_the_row_with_an_add_hint() {
    let mut fx = fx();
    fx.cfg_mut("admin").default_branch = "no-such-branch".into();
    let origin = fx.origin_head("tool", "main");
    let (s, g) = edit(&fx, &["tool"]);
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!(reason(&e), "siblings_missing");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["repo"], "tool");
    assert_eq!(d["session"], s.as_str());
    assert_eq!(d["missing"][0]["repo"], "admin");
    assert_eq!(d["missing"][0]["why"], "main_tip_unavailable");
    assert!(d["missing"][0]["cause"]["code"].is_string(), "{d}");
    assert!(d["missing"][0]["next"].as_str().unwrap().contains("or run the add"), "{d}");
    assert_eq!(d["fix"], format!("git-strata add --session {} --repo admin", s.as_str()));
    assert!(e.message.contains("admin"), "{}", e.message);
    assert_refused_up_front(&fx, &s, &origin);
    // The remedy works: once origin has the branch, the add succeeds and the
    // publish then uses the pin.
    fx.sb.git(&fx.main("admin"), &["push", "-q", "origin", "HEAD:no-such-branch"]);
    fx.sessions().add(&s, &[fx.rn("admin")]).unwrap();
    let p = run(&fx, &g).unwrap();
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
    assert!(sib_log(&fx).contains("admin-present"));
}

#[test]
fn busy_sibling_lock_is_busy_with_the_sibling_named() {
    let fx = fx();
    let origin = fx.origin_head("tool", "main");
    let (s, g) = edit(&fx, &["tool"]);
    fx.lock.set_busy(1);
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(e.code, Code::Busy, "{e:?}");
    assert_eq!(e.detail.as_ref().unwrap()["sibling"], "admin");
    assert_refused_up_front(&fx, &s, &origin);
}

#[test]
fn sibling_anchor_failure_is_unknown_before_the_row() {
    let fx = fx();
    let origin = fx.origin_head("tool", "main");
    let (s, g) = edit(&fx, &["tool"]);
    seams::fail_once(fx.git_of("admin"), FailPoint::SiblingAnchor);
    let e = run(&fx, &g).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["reason"], "anchor_transaction");
    assert_eq!(d["sibling"], "admin");
    assert_refused_up_front(&fx, &s, &origin);
}

#[test]
fn stage_supplies_the_siblings_too() {
    let fx = fx();
    let (_s, g) = edit(&fx, &["tool"]);
    let base = CommitOid::try_from(fx.read_ref("tool", ORIGIN).unwrap()).unwrap();
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let st = stage(&ctx, &g, &base).unwrap();
    assert!(st.layout_root().join("admin/package.json").is_file());
    strata_publish::remove_layout(
        &fx.root(),
        &strata_types::RepoPath::try_from(
            st.layout_root().strip_prefix(fx.root()).unwrap().to_string_lossy().into_owned(),
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn a_collected_prepared_base_is_base_collected() {
    let fx = fx();
    let s = slim(&fx, &["tool"]);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let mut prep = siblings::prepare(&ctx, Some(&s)).unwrap();
    assert!(prep.missing.is_empty());
    let g = prep.ready.get(&fx.rn("admin")).unwrap().clone();
    // A generation the store does not have stands in for one collected.
    let tree = strata_types::TreeOid::try_from("0".repeat(40)).unwrap();
    prep.ready.insert(fx.rn("admin"), strata_types::BaseGen { tree, ..g });
    let e = siblings::place(ctx.store, ctx.git, ctx.cfg, ctx.repo, Some(&s), &prep).unwrap_err();
    assert_eq!(reason(&e), "siblings_missing");
    let m = &e.detail.as_ref().unwrap()["missing"][0];
    assert_eq!(m["why"], "base_collected");
    assert!(m["next"].as_str().unwrap().contains("retry the publish"), "{m}");
}

#[test]
fn a_clone_of_a_vanished_base_is_clone_failed() {
    let fx = fx();
    let dst = fx.sb.root.join("layout");
    fs::create_dir_all(&dst).unwrap();
    let supply = Supply { repo: fx.rn("admin"), src: fx.sb.root.join("gone") };
    let m = siblings::clone_supplies(&[supply], &dst).unwrap_err();
    assert_eq!(m.why, Why::CloneFailed);
    assert_eq!(m.cause.code, Code::NoSuchPath, "{:?}", m.cause);
    let e = siblings::missing_error(&fx.rn("tool"), None, &[m]);
    let d = e.detail.unwrap();
    assert_eq!(d["fix"], "retry `git-strata push tool`");
    assert_eq!(d["session"], serde_json::Value::Null);
    assert!(d["missing"][0]["next"].as_str().unwrap().contains("retry `git-strata push tool`"), "{d}");
}

#[test]
fn a_pinned_sibling_with_no_base_dir_is_corrupt() {
    let fx = fx();
    let s = fx.new_session(&["tool", "admin"]);
    let mut cfg = fx.config.repos[&fx.rn("tool")].clone();
    cfg.check_siblings = vec![fx.rn("admin")];
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let prep = Prepared::default();
    let ok = siblings::place(ctx.store, ctx.git, &cfg, ctx.repo, Some(&s), &prep).unwrap();
    assert!(ok.is_empty(), "a pinned sibling is cloned from its pin, not supplied: {ok:?}");
    // The pin's directory goes missing from the store.
    let g = fx.base_of(&s, "admin");
    let dir = fx.store().base_dir(&g);
    let hidden = dir.with_extension("hidden");
    fs::rename(&dir, &hidden).unwrap();
    let e = siblings::place(ctx.store, ctx.git, &cfg, ctx.repo, Some(&s), &prep).unwrap_err();
    fs::rename(&hidden, &dir).unwrap();
    assert_eq!(e.code, Code::Corrupt, "{e:?}");
    assert_eq!(e.detail.as_ref().unwrap()["sibling"], "admin");
}

#[test]
fn a_base_removed_after_place_refuses_with_the_layout_gone() {
    let fx = fx();
    let origin = fx.origin_head("tool", "main");
    let (s, g) = edit(&fx, &["tool"]);
    let p = seams::pause(fx.git_of("tool"), PublishFault::PauseAfterSiblingPlace);
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| run(&fx, &g));
        p.wait_until_paused();
        // Collect the prepared admin base out from under the publish.
        let bases = fx.root().join("bases").join("admin");
        let hidden = fx.root().join("bases").join("admin.hidden");
        fs::rename(&bases, &hidden).unwrap();
        p.release();
        let r = h.join().unwrap();
        fs::rename(&hidden, &bases).unwrap();
        r
    });
    let e = r.unwrap_err();
    assert_eq!(reason(&e), "siblings_missing", "{e:?}");
    let m = &e.detail.as_ref().unwrap()["missing"][0];
    assert_eq!(m["why"], "clone_failed");
    assert!(layouts(&fx, &s).is_empty(), "the layout was removed");
    assert_eq!(fx.origin_head("tool", "main"), origin);
    // next = retry: with the base back the same publish lands.
    let p = run(&fx, &g).unwrap();
    assert_eq!(fx.origin_head("tool", "main"), p.commit.as_str());
}

#[test]
fn an_add_between_prepare_and_place_uses_the_new_pin() {
    let fx = fx();
    let s = slim(&fx, &["tool"]);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let prep = siblings::prepare(&ctx, Some(&s)).unwrap();
    assert!(prep.ready.contains_key(&fx.rn("admin")));
    fx.sessions().add(&s, &[fx.rn("admin")]).unwrap();
    let supplies = siblings::place(ctx.store, ctx.git, ctx.cfg, ctx.repo, Some(&s), &prep).unwrap();
    assert!(supplies.is_empty(), "admin now comes from the pin, cloned once: {supplies:?}");
}

#[test]
fn a_sibling_is_fetched_under_its_own_lock_before_the_publishing_repo() {
    let fx = fx();
    let (_s, g) = edit(&fx, &["tool"]);
    let n = fx.lock.holds().len();
    run(&fx, &g).unwrap();
    let hs = fx.lock.holds();
    let new = &hs[n..];
    let first_tool = new.iter().position(|h| h.repo.as_str() == "tool").expect("tool locked");
    assert!(new[..first_tool].iter().any(|h| h.repo.as_str() == "admin"), "admin first: {new:?}");
    // Never nested: every admin hold ended before tool's lock was taken.
    for h in new[..first_tool].iter() {
        assert!(h.end.is_some_and(|e| e <= new[first_tool].start), "{new:?}");
    }
}

/// `run` with `arm` applied to the publish's own `Sessions` (the session
/// seams live on that instance).
fn run_armed(
    fx: &Fx,
    g: &ReleaseGroup,
    arm: impl FnOnce(&strata_session::Sessions<'_>),
) -> Result<Published, StrataError> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    arm(&ss);
    let ctx = fx.ctx(&d, &ss);
    publish(&ctx, g, None)
}

#[test]
fn a_sibling_cold_install_failure_refuses_before_the_row() {
    let fx = fx();
    let origin = fx.origin_head("tool", "main");
    let (s, g) = edit(&fx, &["tool"]);
    // admin's lockfile moved on origin: its main-tip base needs a cold install.
    fx.land_on_origin("admin", "main", &[("bun.lock", b"lock admin v2\n")], "admin deps move");
    fx.sb.set_install_script("echo install broke >&2; exit 1\n");
    let e = run(&fx, &g).unwrap_err();
    // ensure_base's Unknown comes back whole, with the sibling named.
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["sibling"], "admin");
    assert_eq!((d["op"].as_str(), d["phase"].as_str()), (Some("base"), Some("deps")), "{d}");
    assert_eq!(d["cause"]["detail"]["reason"], "install_failed", "{d}");
    assert!(d["cause"]["detail"]["attempt"].is_number() && d["kept"].is_array() && d["retired"].is_array(), "{d}");
    assert_refused_up_front(&fx, &s, &origin);
    // deps keeps the failed attempt (`previous_attempt`): a retry stays
    // Unknown until it is resolved; it never publishes without the sibling.
    fx.sb.set_install_script("exit 0\n");
    let again = run(&fx, &g).unwrap_err();
    assert_eq!(again.code, Code::Unknown, "{again:?}");
    assert_eq!(again.detail.as_ref().unwrap()["cause"]["detail"]["reason"], "previous_attempt", "{again:?}");
    assert_refused_up_front(&fx, &s, &origin);
}

#[test]
fn an_uncertain_sibling_base_registration_is_unknown_keeping_its_detail() {
    let fx = fx();
    let origin = fx.origin_head("tool", "main");
    let (s, g) = edit(&fx, &["tool"]);
    fx.land_on_origin("admin", "main", &[("marker.txt", b"admin-main-tip\n")], "admin moves");
    let e = run_armed(&fx, &g, |ss| {
        ss.arm(strata_session::SessionSeam::FailAt { phase: "base.register".into(), errno: libc::EIO })
    })
    .unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["sibling"], "admin");
    assert_eq!(d["op"], "base", "ensure_base's own detail is kept: {d}");
    assert_eq!(d["phase"], "register");
    assert!(d["kept"].is_array() && d["cause"].is_object(), "{d}");
    assert_refused_up_front(&fx, &s, &origin);
}

#[test]
fn the_sibling_anchor_is_advanced_so_a_later_rewind_reads_rewound() {
    let fx = fx();
    let admin = fx.main("admin");
    let url = fx.sb.git(&admin, &["remote", "get-url", "origin"]).trim().to_string();
    let a = fx.origin_head("admin", "main");
    // admin published T (anchor A) and T then landed on origin; admin's
    // origin ref is still the stale A, so its chain reads Leads.
    let t = fx.land_on_origin("admin", "main", &[("marker.txt", b"admin-t\n")], "admin's publish lands");
    fx.sb.git(&admin, &["fetch", "-q", &url, "main"]);
    fx.sb.git(&admin, &["update-ref", ORIGIN, &a]);
    fx.sb.git(&admin, &["update-ref", &strata_publish::chain_ref("main"), &t]);
    fx.sb.git(&admin, &["update-ref", &strata_publish::anchor_ref("main"), &a]);
    let state = |fx: &Fx| strata_deps::publish_chain(fx.git_of("admin"), "main").unwrap().unwrap().state;
    assert_eq!(state(&fx), strata_deps::ChainState::Leads);
    let (_s, g) = edit(&fx, &["tool"]);
    run(&fx, &g).unwrap();
    // The sibling fetch moved admin's anchor up to what origin now holds ...
    assert_eq!(fx.read_ref("admin", ORIGIN).as_deref(), Some(t.as_str()));
    assert_eq!(fx.read_ref("admin", &strata_publish::anchor_ref("main")).as_deref(), Some(t.as_str()));
    // ... so when origin is later rewound to A the chain is Rewound, not a
    // Leads that would push T again.
    fx.sb.git(std::path::Path::new(&url), &["update-ref", "refs/heads/main", &a]);
    fx.git_of("admin").fetch("origin", "main").unwrap();
    assert_eq!(fx.read_ref("admin", ORIGIN).as_deref(), Some(a.as_str()));
    assert_eq!(state(&fx), strata_deps::ChainState::Rewound);
}
