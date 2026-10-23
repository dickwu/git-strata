//! overlay.md §7: overlay_validate_rejects_malformed_shapes.

use strata_overlay::Overlay;
use strata_types::{BaseGen, BlobId, Code, EntryKind, OverlayEntry, RepoPath};

fn p(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

fn blob(c: char) -> BlobId {
    BlobId::try_from(c.to_string().repeat(64)).unwrap()
}

fn base() -> BaseGen {
    BaseGen {
        repo: "app".to_string().try_into().unwrap(),
        tree: "1".repeat(40).try_into().unwrap(),
        deps: "a".repeat(64).try_into().unwrap(),
    }
}

fn file(path: &str, kind: EntryKind) -> OverlayEntry {
    let mode = if matches!(kind, EntryKind::Deleted) {
        0
    } else {
        0o100644
    };
    OverlayEntry {
        path: p(path),
        kind,
        mode,
    }
}

fn modified(path: &str) -> OverlayEntry {
    file(path, EntryKind::Modified { blob: blob('b') })
}

fn deleted(path: &str) -> OverlayEntry {
    file(path, EntryKind::Deleted)
}

fn renamed(path: &str, from: &str) -> OverlayEntry {
    file(
        path,
        EntryKind::Renamed {
            from: p(from),
            blob: blob('c'),
        },
    )
}

fn ov(entries: Vec<OverlayEntry>) -> Overlay {
    Overlay {
        base: base(),
        entries,
    }
}

fn rule_of(o: &Overlay) -> String {
    let err = o.validate().expect_err("malformed overlay must be refused");
    assert_eq!(err.code, Code::InvalidInput, "{err:?}");
    err.detail
        .as_ref()
        .and_then(|d| d["rule"].as_str())
        .unwrap()
        .to_string()
}

#[test]
fn overlay_validate_rejects_malformed_shapes() {
    // V1: duplicate and out of order.
    assert_eq!(rule_of(&ov(vec![modified("a"), modified("a")])), "V1");
    assert_eq!(rule_of(&ov(vec![modified("b"), modified("a")])), "V1");
    // V2: Deleted with a mode, a live entry with mode 0, a bad mode.
    let mut d = deleted("a");
    d.mode = 0o100644;
    assert_eq!(rule_of(&ov(vec![d])), "V2");
    let mut m = modified("a");
    m.mode = 0;
    assert_eq!(rule_of(&ov(vec![m])), "V2");
    let mut m = modified("a");
    m.mode = 0o100600;
    assert_eq!(rule_of(&ov(vec![m])), "V2");
    // V3: from equals its own path, another entry's path, or is shared.
    assert_eq!(rule_of(&ov(vec![renamed("a", "a")])), "V3");
    assert_eq!(rule_of(&ov(vec![modified("a"), renamed("b", "a")])), "V3");
    assert_eq!(
        rule_of(&ov(vec![renamed("b", "a"), renamed("c", "a")])),
        "V3"
    );
    // V4: a and a/b both live.
    assert_eq!(rule_of(&ov(vec![modified("a"), modified("a/b")])), "V4");
    // A deleted file under which a directory now lives is fine.
    ov(vec![deleted("a"), modified("a/b")]).validate().unwrap();

    // A well-formed overlay.
    let good = ov(vec![
        deleted("d"),
        modified("m"),
        renamed("new", "old"),
        deleted("t"),
        file("t/x", EntryKind::Added { blob: blob('d') }),
        file("t/y/z", EntryKind::Untracked { blob: blob('e') }),
    ]);
    good.validate().unwrap();
    assert!(good.hides(&p("d")));
    assert!(good.hides(&p("old")));
    assert!(good.hides(&p("new")));
    assert!(good.hides(&p("m")));
    assert!(!good.hides(&p("other")));
    assert!(good.get(&p("old")).is_none(), "get looks at paths only");
    assert_eq!(good.tombstones(), vec![p("d"), p("old"), p("t")]);
    assert_eq!(good.paired(&p("new")), vec![p("new"), p("old")]);
    assert_eq!(good.paired(&p("old")), vec![p("new"), p("old")]);
    let swap = vec![p("t"), p("t/x"), p("t/y/z")];
    assert_eq!(good.paired(&p("t")), swap);
    assert_eq!(good.paired(&p("t/y/z")), swap);
    assert_eq!(good.paired(&p("m")), vec![p("m")]);
    assert!(good.paired(&p("nothing")).is_empty());
    assert!(
        good.paired(&p("t/y")).is_empty(),
        "a directory is not an entry"
    );
    assert!(!good.is_empty());
    assert!(Overlay::empty(base()).is_empty());
    Overlay::empty(base()).validate().unwrap();
}
