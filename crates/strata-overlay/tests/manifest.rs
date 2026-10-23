//! Task 3: `build_manifest` (§5.9).

mod common;

use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;

use common::*;
use strata_overlay::build_manifest;
use strata_types::Code;

#[test]
fn build_manifest_matches_exported_tree() {
    let w = World::new();
    let tree = w.base.r#gen.tree.clone();
    let dir = w.tmp.join("export");
    w.git.export_tree(&tree, &dir).unwrap();
    // What the base generation directory also holds: the deps set clone,
    // seed caches and a `.git`.
    put(
        &dir,
        "node_modules/left-pad/index.js",
        b"module.exports = 1\n",
    );
    put(&dir, "src-tauri/target/debug/app", b"\x7fELF");
    put(&dir, ".git", b"gitdir: /nowhere\n");
    let before = w.store_files();

    let m = build_manifest(&dir, &tree, &w.config).unwrap();
    assert_eq!(m.tree, tree);

    // ls-tree: "<mode> <type> <oid>\t<path>"
    let listed = w.git_ok(&w.main, &["ls-tree", "-r", "-z", "HEAD"]);
    let mut want = BTreeSet::new();
    for rec in listed.split('\0').filter(|r| !r.is_empty()) {
        let (meta, path) = rec.split_once('\t').unwrap();
        let mode = u32::from_str_radix(meta.split(' ').next().unwrap(), 8).unwrap();
        let p = dir.join(path);
        let bytes = if mode == 0o120000 {
            fs::read_link(&p).unwrap().as_os_str().as_bytes().to_vec()
        } else {
            fs::read(&p).unwrap()
        };
        let e = m
            .entries
            .get(&rp(path))
            .unwrap_or_else(|| panic!("missing {path}"));
        assert_eq!(e.blob.as_str(), blake3_hex(&bytes), "{path}");
        assert_eq!(e.mode, mode, "{path}");
        assert_eq!(e.size, bytes.len() as u64, "{path}");
        want.insert(path.to_owned());
    }
    let got: BTreeSet<String> = m.entries.keys().map(|p| p.as_str().to_owned()).collect();
    assert_eq!(got, want);
    for p in [
        "README.md",
        "bin/run.sh",
        "link",
        "empty.txt",
        "src/deep/x/y.txt",
    ] {
        assert!(got.contains(p), "{p}");
    }
    assert_eq!(m.entries[&rp("bin/run.sh")].mode, 0o100755);
    assert_eq!(m.entries[&rp("README.md")].mode, 0o100644);
    assert_eq!(m.entries[&rp("link")].mode, 0o120000);
    assert_eq!(m.entries[&rp("empty.txt")].size, 0);
    assert!(
        got.iter()
            .all(|p| !p.starts_with("node_modules") && !p.starts_with("src-tauri") && p != ".git")
    );

    // Deterministic, and the Store is untouched.
    assert_eq!(build_manifest(&dir, &tree, &w.config).unwrap(), m);
    // The fixture's recorded manifest is the same one.
    assert_eq!(*w.base.manifest, m);

    // A FIFO in the tree is Unsupported.
    let fifo = CString::new(dir.join("src/pipe").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    let e = build_manifest(&dir, &tree, &w.config).unwrap_err();
    assert_eq!(e.code, Code::Unsupported, "{e:?}");
    // ...but not under an excluded dir.
    fs::remove_file(dir.join("src/pipe")).unwrap();
    let fifo = CString::new(dir.join("node_modules/pipe").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    assert_eq!(build_manifest(&dir, &tree, &w.config).unwrap(), m);

    assert_eq!(w.store_files(), before);
}
