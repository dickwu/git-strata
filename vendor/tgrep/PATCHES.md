# Vendored tgrep (microsoft/tgrep @ d25ade642cd0a34537cc0791a0abbac57ccbf6f9)

Why: `strata-search` needs `HybridIndex::from_reader(Arc<IndexReader>, &Path)`
(docs/modules/search.md §2 upstream request 1). At d25ade6 it is `pub(crate)`,
and tgrep-core is not on crates.io. design.md §5 says "tgrep-core 作为库依赖
[不可行则 vendor 所需模块]", so the needed crates are vendored here, unchanged
except for the patch below. Nothing in strata-search re-implements the merge.

Contents (from `git archive d25ade6`):

- `tgrep-core/src/` and `LICENSE` (MIT). Not vendored: `tgrep-core/tests/`,
  `tgrep-core/benches/`, `tgrep-cli/`.
- `vendor/ignore/` as is (tgrep's own vendored `ignore`, with its PATCHES.md),
  so tgrep-core's `ignore = { path = "../vendor/ignore" }` resolves unchanged.

Patches:

1. `tgrep-core/src/hybrid.rs`: `pub(crate) fn from_reader` -> `pub fn from_reader`.
   This is exactly the visibility change asked of upstream. Drop the vendor
   copy once upstream publishes it (or an equivalent `from_shared_reader`).
2. `tgrep-core/Cargo.toml`: workspace-inherited fields and dependency versions
   inlined from tgrep's root Cargo.toml at d25ade6; `[[bench]]` targets and the
   `criterion` dev-dependency dropped.

The root git-strata workspace lists `vendor` in `exclude`, so these crates are
path dependencies only: `cargo test --workspace` does not run their tests.
