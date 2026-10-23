//! overlay.md §7: lww_glob_vectors, lww_rules_load_from_strata_lww.

use std::fs;
use std::path::Path;

use strata_overlay::{LwwRules, lww_matches};
use strata_types::RepoPath;

fn p(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

fn m(rules: &str, path: &str) -> bool {
    lww_matches(&LwwRules::parse(rules), &p(path))
}

#[test]
fn lww_glob_vectors() {
    // `a/**` covers direct children and deeper ones (leading `/` dropped too).
    for r in ["a/**", "/a/**"] {
        assert!(m(r, "a/b"), "{r}");
        assert!(m(r, "a/b/c"), "{r}");
        assert!(!m(r, "ab/c"), "{r}");
        assert!(!m(r, "x/a/b"), "{r}");
    }
    // `**/b` needs whole directory names.
    assert!(m("**/b", "b"));
    assert!(m("**/b", "x/b"));
    assert!(m("**/b", "x/y/b/c"));
    assert!(!m("**/b", "ab"));
    assert!(!m("**/b", "xb"));
    assert!(!m("**/b", "x/ab"));
    // Consecutive `**/` may be zero directories.
    assert!(m("a/**/**/b", "a/b"));
    assert!(m("a/**/**/b", "a/x/b"));
    assert!(m("a/**/**/b", "a/x/y/z/b"));
    assert!(m("a/**/b", "a/b"));
    assert!(!m("a/**/b", "a/xb"));
    // `*` and `?` stay within a name.
    assert!(m("src/*.lock", "src/x.lock"));
    assert!(!m("src/*.lock", "src/d/x.lock"));
    assert!(m("src/?.c", "src/a.c"));
    assert!(!m("src/?.c", "src/ab.c"));
    assert!(!m("a?b", "a/b"));
    assert!(!m("x/a*b", "x/a/b"));
    // A pattern without `/` matches the name at any depth.
    assert!(m("Cargo.lock", "Cargo.lock"));
    assert!(m("Cargo.lock", "crates/x/Cargo.lock"));
    assert!(m("*.lock", "deep/er/yarn.lock"));
    assert!(!m("Cargo.lock", "Cargo.lock.bak"));
    // A matched ancestor directory matches everything below it.
    assert!(m("generated", "generated/x/y.rs"));
    assert!(m("src/gen", "src/gen/a/b"));
    assert!(!m("src/gen", "src/generated/a"));
    assert!(m("dist/", "dist/app.js"));
    // The last matching rule decides; `!` clears.
    assert!(!m("*.lock\n!yarn.lock", "yarn.lock"));
    assert!(m("*.lock\n!yarn.lock", "Cargo.lock"));
    assert!(m("!yarn.lock\n*.lock", "yarn.lock"));
    assert!(m("gen/**\n!gen/keep\ngen/keep/x", "gen/keep/x"));
    assert!(!m("gen/**\n!gen/keep", "gen/keep/y"));
    // Case-only differences do not match.
    assert!(!m("Cargo.lock", "cargo.lock"));
    assert!(!m("SRC/**", "src/a"));
    // NFC: a decomposed pattern matches the (NFC) path.
    assert!(m("cafe\u{301}.txt", "caf\u{e9}.txt"));
    // Nothing matches with no rules.
    assert!(LwwRules::parse("").is_empty());
    assert!(!m("", "a"));
}

fn write(root: &Path, rel: &str, body: &[u8]) {
    let f = root.join(rel);
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    fs::write(f, body).unwrap();
}

#[test]
fn lww_rules_load_from_strata_lww() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path();

    // Nothing there: empty rules.
    assert!(LwwRules::load(tree).unwrap().is_empty());
    // The LayerFS location `.lww` at the root does nothing.
    write(tree, ".lww", b"*.lock\n");
    assert!(LwwRules::load(tree).unwrap().is_empty());

    // `.strata/lww` as a directory: empty.
    fs::create_dir_all(tree.join(".strata/lww")).unwrap();
    assert!(LwwRules::load(tree).unwrap().is_empty());
    fs::remove_dir(tree.join(".strata/lww")).unwrap();
    // Not UTF-8: empty.
    write(tree, ".strata/lww", b"*.lock\n\xff\xfe\n");
    assert!(LwwRules::load(tree).unwrap().is_empty());

    // Comments, blank lines and a lone `!` do nothing.
    let text = "# comment\n\n!\n*.lock\n!yarn.lock\na/**/**/b\nsrc/gen/\n%include rules/more\n";
    write(tree, ".strata/lww", text.as_bytes());
    write(tree, "rules/more", b"vendor/**\n%include rules/deeper\n");
    write(tree, "rules/deeper", b"/top.bin\n");
    let loaded = LwwRules::load(tree).unwrap();
    let parsed = LwwRules::parse(text);
    let all = LwwRules::parse(&format!("{text}vendor/**\n/top.bin\n"));
    for path in [
        "Cargo.lock",
        "x/Cargo.lock",
        "yarn.lock",
        "a/b",
        "a/x/b",
        "ab",
        "src/gen/x",
        "src/x",
        "vendor/a/b",
        "vendor",
        "top.bin",
        "x/top.bin",
        "README.md",
    ] {
        assert_eq!(
            lww_matches(&loaded, &p(path)),
            lww_matches(&all, &p(path)),
            "{path}"
        );
    }
    // `parse` ignores `%include`; on paths the include does not touch they agree.
    for path in [
        "Cargo.lock",
        "yarn.lock",
        "a/b",
        "a/x/b",
        "src/gen/x",
        "README.md",
    ] {
        assert_eq!(
            lww_matches(&loaded, &p(path)),
            lww_matches(&parsed, &p(path)),
            "{path}"
        );
    }
    assert!(lww_matches(&loaded, &p("vendor/a/b")));
    assert!(!lww_matches(&parsed, &p("vendor/a/b")));
    assert!(lww_matches(&loaded, &p("top.bin")));
    assert!(
        !lww_matches(&loaded, &p("x/top.bin")),
        "a leading / anchors"
    );

    // A missing include reads as empty; the rest still loads.
    write(tree, ".strata/lww", b"%include nope/missing\n*.lock\n");
    assert!(lww_matches(&LwwRules::load(tree).unwrap(), &p("a.lock")));

    // A chain of includes: from the 17th file on, `%include` is ignored.
    let mut chain = String::from("%include inc/1\n");
    write(tree, ".strata/lww", chain.as_bytes());
    for i in 1..=20 {
        chain = format!("f{i}\n%include inc/{}\n", i + 1);
        write(tree, &format!("inc/{i}"), chain.as_bytes());
    }
    let r = LwwRules::load(tree).unwrap();
    for i in 1..=15 {
        assert!(
            lww_matches(&r, &p(&format!("f{i}"))),
            "file {i} is within the 16"
        );
    }
    for i in 16..=20 {
        assert!(
            !lww_matches(&r, &p(&format!("f{i}"))),
            "file {i} is beyond the 16"
        );
    }

    // A symlinked rules file is not a regular file: empty.
    fs::remove_file(tree.join(".strata/lww")).unwrap();
    write(tree, "real", b"*.lock\n");
    std::os::unix::fs::symlink("../real", tree.join(".strata/lww")).unwrap();
    assert!(LwwRules::load(tree).unwrap().is_empty());
}
