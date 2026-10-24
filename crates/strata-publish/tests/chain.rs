//! Task 1: the publish chain, its anchor and the merge base (publish.md §5.4, D1, D2).

mod support;

use strata_deps::{ChainState, main_tip, publish_chain};
use strata_types::CommitOid;
use strata_publish::{advance_anchor, anchor_ref, chain_ref, merge_base};
use strata_types::Code;
use support::*;

const ORIGIN: &str = "refs/strata/remotes/origin/main";

/// A commit on top of `parent` with the parent's tree (distinct message).
fn child(fx: &Fx, parent: &str, msg: &str) -> String {
    let main = fx.main("tool");
    let tree = fx.sb.tree_of(&main, parent);
    fx.sb.git(&main, &["commit-tree", &tree, "-p", parent, "-m", msg])
}

fn set(fx: &Fx, name: &str, oid: &str) {
    fx.sb.git(&fx.main("tool"), &["update-ref", name, oid]);
}

fn del(fx: &Fx, name: &str) {
    fx.sb.git(&fx.main("tool"), &["update-ref", "-d", name]);
}

fn oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.to_string()).unwrap()
}

/// root ← x (origin) with the chain and anchor set as given.
fn layout(fx: &Fx, origin: &str, top: Option<&str>, anchor: Option<&str>) {
    set(fx, ORIGIN, origin);
    match top {
        Some(t) => set(fx, &chain_ref("main"), t),
        None => del(fx, &chain_ref("main")),
    }
    match anchor {
        Some(a) => set(fx, &anchor_ref("main"), a),
        None => del(fx, &anchor_ref("main")),
    }
}

fn state(fx: &Fx) -> ChainState {
    publish_chain(fx.git_of("tool"), "main").unwrap().unwrap().state
}

#[test]
fn merge_base_prefers_local_chain_when_ahead() {
    let fx = Fx::with(&["tool"]);
    let git = fx.git_of("tool");
    let cfg = &fx.config.repos[&repo_name("tool")];
    // Never fetched → NoSuchBranch.
    let e = merge_base(git, cfg).unwrap_err();
    assert_eq!(e.code, Code::NoSuchBranch);

    git.fetch("origin", "main").unwrap();
    let o = fx.read_ref("tool", ORIGIN).unwrap();
    let c1 = child(&fx, &o, "c1");
    let side = child(&fx, &o, "side");
    let o2 = child(&fx, &o, "o2");

    // Leads, anchor on origin → B is the chain top.
    layout(&fx, &o, Some(&c1), Some(&o));
    assert_eq!(state(&fx), ChainState::Leads);
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&c1));

    // Absent → origin.
    layout(&fx, &o, None, None);
    assert_eq!(state(&fx), ChainState::Absent);
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&o));
    // Equal → origin.
    layout(&fx, &o, Some(&o), Some(&o));
    assert_eq!(state(&fx), ChainState::Equal);
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&o));
    // Behind → origin.
    let o3 = child(&fx, &c1, "o3");
    layout(&fx, &o3, Some(&c1), Some(&o));
    assert_eq!(state(&fx), ChainState::Behind);
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&o3));
    // Rewound (no anchor; anchor not on origin) → origin, even though the top leads.
    layout(&fx, &o, Some(&c1), None);
    assert_eq!(state(&fx), ChainState::Rewound);
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&o));
    layout(&fx, &o2, Some(&side), Some(&side));
    assert_eq!(state(&fx), ChainState::Rewound);
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&o2));
    // Diverged → origin.
    layout(&fx, &o2, Some(&side), Some(&o));
    assert_eq!(state(&fx), ChainState::Diverged);
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&o2));
}

#[test]
fn anchor_advances_to_merge_base_not_origin_when_behind() {
    let fx = Fx::with(&["tool"]);
    let git = fx.git_of("tool");
    git.fetch("origin", "main").unwrap();
    let a = fx.read_ref("tool", ORIGIN).unwrap();
    let c = child(&fx, &a, "c");
    let o = child(&fx, &c, "o");

    // Behind: a ← c ← o, anchor a, top c → anchor becomes c (not o).
    layout(&fx, &o, Some(&c), Some(&a));
    assert_eq!(state(&fx), ChainState::Behind);
    assert_eq!(advance_anchor(git, "main").unwrap(), Some(oid(&c)));
    assert_eq!(fx.read_ref("tool", &anchor_ref("main")).as_deref(), Some(c.as_str()));
    assert_eq!(fx.read_ref("tool", &chain_ref("main")).as_deref(), Some(c.as_str()));
    // A second advance has nothing to do.
    assert_eq!(advance_anchor(git, "main").unwrap(), None);

    // Equal with a ≠ o → a becomes o.
    layout(&fx, &o, Some(&o), Some(&a));
    assert_eq!(state(&fx), ChainState::Equal);
    assert_eq!(advance_anchor(git, "main").unwrap(), Some(oid(&o)));
    assert_eq!(fx.read_ref("tool", &anchor_ref("main")).as_deref(), Some(o.as_str()));

    // Diverged: a ← f ← {x (origin), y (top)} → anchor becomes the fork f.
    let f = child(&fx, &a, "f");
    let x = child(&fx, &f, "x");
    let y = child(&fx, &f, "y");
    layout(&fx, &x, Some(&y), Some(&a));
    assert_eq!(state(&fx), ChainState::Diverged);
    assert_eq!(advance_anchor(git, "main").unwrap(), Some(oid(&f)));
    assert_eq!(fx.read_ref("tool", &anchor_ref("main")).as_deref(), Some(f.as_str()));

    // Leads with the anchor already at the merge base → nothing written.
    layout(&fx, &a, Some(&c), Some(&a));
    assert_eq!(state(&fx), ChainState::Leads);
    assert_eq!(advance_anchor(git, "main").unwrap(), None);
    assert_eq!(fx.read_ref("tool", &anchor_ref("main")).as_deref(), Some(a.as_str()));
}

#[test]
fn anchor_does_not_advance_when_absent_or_rewound() {
    let fx = Fx::with(&["tool"]);
    let git = fx.git_of("tool");
    git.fetch("origin", "main").unwrap();
    let a = fx.read_ref("tool", ORIGIN).unwrap();
    let c = child(&fx, &a, "c");
    let o = child(&fx, &c, "o");
    let side = child(&fx, &a, "side");

    // Absent: no chain, an old anchor stays.
    layout(&fx, &o, None, Some(&a));
    assert_eq!(state(&fx), ChainState::Absent);
    assert_eq!(advance_anchor(git, "main").unwrap(), None);
    assert_eq!(fx.read_ref("tool", &anchor_ref("main")).as_deref(), Some(a.as_str()));

    // Rewound, no anchor: none is created.
    layout(&fx, &o, Some(&c), None);
    assert_eq!(state(&fx), ChainState::Rewound);
    assert_eq!(advance_anchor(git, "main").unwrap(), None);
    assert_eq!(fx.read_ref("tool", &anchor_ref("main")), None);

    // Rewound, anchor not on origin: unchanged.
    layout(&fx, &o, Some(&side), Some(&side));
    assert_eq!(state(&fx), ChainState::Rewound);
    assert_eq!(advance_anchor(git, "main").unwrap(), None);
    assert_eq!(fx.read_ref("tool", &anchor_ref("main")).as_deref(), Some(side.as_str()));
}

#[test]
fn chain_rewound_or_diverged_refuses_before_write() {
    let fx = Fx::with(&["tool"]);
    let git = fx.git_of("tool");
    git.fetch("origin", "main").unwrap();
    let a = fx.read_ref("tool", ORIGIN).unwrap();
    let c = child(&fx, &a, "c");
    let side = child(&fx, &a, "side");
    let o = child(&fx, &a, "o");

    // Rewound: refused, nothing written (chain bytes and absent anchor unchanged).
    layout(&fx, &o, Some(&c), None);
    let refs = fx.refs("tool");
    let e = strata_publish::chain::refuse_unpublishable(git, "main").unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e), Some("chain_rewound"));
    assert_eq!(fx.refs("tool"), refs);

    // Diverged: refused with chain_diverged; the chain ref is unchanged.
    layout(&fx, &o, Some(&side), Some(&a));
    let e = strata_publish::chain::refuse_unpublishable(git, "main").unwrap_err();
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e), Some("chain_diverged"));
    assert_eq!(fx.read_ref("tool", &chain_ref("main")).as_deref(), Some(side.as_str()));

    // Leads, Equal and Absent pass.
    let lead = child(&fx, &o, "lead");
    for (top, anchor) in [(Some(&lead), Some(&o)), (Some(&o), Some(&o)), (None, None)] {
        layout(&fx, &o, top.map(|s| s.as_str()), anchor.map(|s| s.as_str()));
        strata_publish::chain::refuse_unpublishable(git, "main").unwrap();
    }
}

#[test]
fn publish_branch_is_not_default_branch() {
    let mut fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    // origin/release one commit ahead of origin/main.
    fx.sb.git(&main, &["checkout", "-q", "-b", "release"]);
    let rel = fx.sb.commit(&main, &[("REL", b"r\n")], "release");
    fx.sb.git(&main, &["push", "-q", "origin", "release"]);
    fx.sb.git(&main, &["checkout", "-q", "main"]);
    fx.cfg_mut("tool").publish.branch = "release".into();
    let git = fx.git_of("tool");
    git.fetch("origin", "main").unwrap();
    git.fetch("origin", "release").unwrap();
    let cfg = &fx.config.repos[&repo_name("tool")];
    let o_main = fx.read_ref("tool", ORIGIN).unwrap();

    // B is origin/release, not origin/main.
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&rel));
    assert_eq!(chain_ref(&cfg.publish.branch), "refs/strata/publish/release");
    assert_eq!(anchor_ref(&cfg.publish.branch), "refs/strata/anchor/release");

    // A release chain leading origin/release is B; main_tip ignores it
    // (pin_unpushed does not apply when the publish branch differs).
    let top = fx.sb.git(&main, &["commit-tree", &fx.sb.tree_of(&main, &rel), "-p", &rel, "-m", "t"]);
    fx.sb.git(&main, &["update-ref", "refs/strata/publish/release", &top]);
    fx.sb.git(&main, &["update-ref", "refs/strata/anchor/release", &rel]);
    assert_eq!(merge_base(git, cfg).unwrap(), oid(&top));
    assert!(cfg.pin_unpushed);
    assert_eq!(main_tip(cfg, git).unwrap(), Some(oid(&o_main)));
    // Nothing about main's chain was touched.
    assert_eq!(fx.read_ref("tool", "refs/strata/publish/main"), None);
}
