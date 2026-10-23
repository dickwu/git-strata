# gitio implementation progress (branch impl/gitio)

Spec: docs/modules/gitio.md. Crate: crates/strata-gitio.

## Tasks

| Task | Commit | Notes |
| --- | --- | --- |
| 1 sanitized command, open | f96b3b9 | |
| 2 index + config parsing, read_index | b4c257d | |
| 3 ignore rules, is_ignored | (this commit) | |

## Builder decisions (ambiguities)

- tgrep sources (`/Volumes/GWD/inter`) were off-limits to this builder, so
  "copy from tgrep" steps are fresh implementations of the spec; recorded in
  THIRD-PARTY-NOTICES.
- The crate's dev-dependencies include itself with `test-seams`, so plain
  `cargo test -p strata-gitio` / `cargo test --workspace` build the seams
  (same pattern as strata-apfs). `--features test-seams` also works.
- Git version is checked once per process (OnceLock); only a successful
  parse is cached, so a failing check is retried on the next `open`.
- `open` reads `strata.shadow` / `extensions.refStorage` with one
  `config --local --null --get-regexp`; `strata.shadow` uses git's bool
  rules (a bare key is true).
- Task 2: the config parser lives in `src/config.rs` (shared with shadow
  checks), so `config_values_read_like_git` runs as `config::tests::…`
  rather than `index::tests::…`; it compares ~300 config texts against
  `git config --file f --type=bool --get core.ignorecase`.
- Index checksums are not recomputed (no SHA-1 crate cached); a split
  index's shared half must carry the trailing hash named in `link`.
- Index paths that `RepoPath` rejects → `Unknown` reason `bad-path`;
  replacement entries in a split index must have empty names (as git).
- `read_index` errors other than NoSuchPath / IsDirectory / a
  non-canonical worktree argument opening `.git` → `Unknown` with the apfs
  code in `detail.code`.
- Shadow ownership is cached only when positive, keyed by the shadow path.
- Tests were written alongside the implementation (not strictly run red
  first).
- Task 3 (§9 gitignore engine): hand-written port of git's `dir.c` pattern
  rules and `wildmatch.c` (ASCII-only case folding, as git) in
  `src/ignore.rs`; the `ignore` crate was not used because its folding is
  Unicode. Like git, inside `[...]` and after `\` pattern characters are
  not folded (`[A]` does not match `a` even with ignorecase).
- Rule-file reads: `.gitignore` absent / symlink / directory / behind a
  non-directory → no rules; other non-regular → `Unknown` (`detail.path`).
  Shadow `info/exclude` is read rooted and unfollowed: a non-directory
  `info` → no rules, a symlinked one → `Unknown`. Human files (main
  `info/exclude`, global excludes) resolve by realpath; dangling or
  ENOTDIR → no rules. `fsx::read_human` now turns non-absent failures into
  `Unknown` with the resolved path (used by config reads too).
- A relative `core.excludesFile` (rare) is resolved against the common dir.
- "逐条点查与遍历时的逐项调用结果相同": there is no walk API in gitio, so
  `case_sensitive_repo_matches_exactly` checks repeated point queries agree.
- The `fake_shadow` test helper moved to `tests/common` (plus
  `fake_shadow_at` for a chosen workspace path).
