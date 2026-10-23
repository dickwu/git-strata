# gitio implementation progress (branch impl/gitio)

Spec: docs/modules/gitio.md. Crate: crates/strata-gitio.

## Tasks

| Task | Commit | Notes |
| --- | --- | --- |
| 1 sanitized command, open | f96b3b9 | |
| 2 index + config parsing, read_index | b4c257d | |
| 3 ignore rules, is_ignored | 3d39e43 | |
| 4 fetch, ancestry, CAS refs | (this commit) | |

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
- Task 4: `read_ref`/`update_ref`/`delete_ref` validate names locally
  (prefix `refs/strata/` plus git's check-ref-format rules) so refusals run
  no subprocess; `fetch` branch names use `git check-ref-format` as specified.
- Task 4: a failed update-ref/`-d` is `HeadMoved` when stderr says
  "but expected"/"reference already exists" *or* the re-read actual value
  differs from the expected one (covers deleting an already-gone ref);
  otherwise `Unknown`.
- Task 4: `is_ancestor`/`merge_base` failures other than exit 0/1 resolve each
  oid; a missing one is `NoSuchCommit` (detail.commit), else `Unknown`.
- Task 4: `fetch` of an unconfigured remote ("does not appear to be a git
  repository") is `InvalidInput` (reason `no-such-remote`).
- Task 4: the "paths starting with `-` or containing `*`/`:` are literal" half
  of `option_like_arguments_are_refused_not_executed` concerns path APIs
  (tree_contains, stage) and is covered with those tasks; "revision
  `--help`" is refused by `CommitOid` construction (and `read_ref`).
- `global_excludes_path`'s config query no longer sets `LC_ALL=C` (§5.3 lists
  only fetch/update-ref/commit-tree/rev-parse/ls-tree).
