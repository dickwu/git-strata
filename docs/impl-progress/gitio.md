# gitio implementation progress (branch impl/gitio)

Spec: docs/modules/gitio.md. Crate: crates/strata-gitio.

## Tasks

| Task | Commit | Notes |
| --- | --- | --- |
| 1 sanitized command, open | f96b3b9 | |
| 2 index + config parsing, read_index | b4c257d | |
| 3 ignore rules, is_ignored | 3d39e43 | |
| 4 fetch, ancestry, CAS refs | e18b094 | |
| 5 tree_of, tree validation, is_ignored_in_export | 4d8bbce | |
| 6 export_tree | 849c6f1 | |
| 7 shadow repositories, attach_worktree, worktree_gitdir | 01cb89b | |
| 8 attach_staging, stage, commit_tree | (this commit) | push part of index_writers_refuse_the_main_checkout lands with Task 9 |

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
- Task 5: `ls_tree_output_parse_is_exact` is a unit test in `src/tree.rs`
  (the parser is internal).
- Task 5: only content-determined tree verdicts (Unsupported / NotText /
  InvalidInput) are cached per TreeOid; NoSuchCommit / Unknown are retried
  (missing objects may arrive by fetch). Verdicts and path tables record the
  core.ignorecase they were judged under and are revalidated if it changes;
  at most 64 path tables are kept (LRU).
- Task 5: a well-formed but unknown ls-tree mode is `Unsupported`; a
  malformed line is `Unknown` (reason `ls-tree-parse`). Collision detail is
  `{reason:"collision", paths:[a,b], path}`. HFS-ignorable Unicode variants of
  `.git` are not checked (only ASCII case-folded `.git`).
- Task 5: the main checkout's ignorecase is read from `<common>/config` only
  (not config.worktree).
- Task 5: the `export_tree` halves of the replace/gitlink/missing-blob/.git/
  overlong/collision tests are added with Task 6.

### Task 6 decisions (export_tree)
- Refusal order: absolute dst → tree validation (tree_table) → dst name present → parent lstat (missing → NoSuchPath; not a dir / symlink → NotDirectory) → dst exists (lstat, so dangling symlinks count) → Exists → inside main work tree / common dir → InvalidInput → bad name → temp name > 255 UTF-16 units → PATH_MAX. The spec lists Exists earlier; parent checks come first because "exists" is meaningless without a parent. All refusals precede the first mkdir.
- A relative dst, and a dst whose last component is `..`/not a single valid RepoPath component, are InvalidInput.
- PATH_MAX (1024) counts the trailing NUL and covers both T/index and every T/tree/<path>.
- Durability order: fsync every regular file, then every dir deepest-first, then T/tree, then one full_fsync(T/tree); symlinks are never opened. The rename seam event is recorded before the rename attempt (so a failed EEXIST rename is still visible to tests).
- Every failure after the first mkdir is Unknown with detail `tree` and `temp` (plus `code` of the underlying error); T is kept. This includes failures after the rename (full_fsync of P, removing T/index or T).
- checkout-index runs without LC_ALL and without a timeout (filters may be slow); a non-required smudge failure is not an error, matching git checkout.
- The autocrlf variant of the byte-for-byte test also converts `*.rot` (auto-detected text) to CRLF; the comparison with a real clone checkout is authoritative.

### Task 7 decisions (shadow, attach_worktree, worktree_gitdir)
- `ensure_shadow` refusal order: relative → non-canonical (`InvalidInput` reason `not-canonical`) → inside the main work tree / common dir (`InvalidInput` reason `inside-main`) → parent missing (`NoSuchPath`) / not a directory (`NotDirectory`) → dir exists: returned when it is an owned shadow, else `Exists` (an unreadable existing dir is also `Exists`, never rewritten). Main mirrors are read before anything is created, so a FIFO/directory `info/exclude` refuses with its typed code (`NotFile`, …) and creates nothing.
- Shadow build: `git -c init.defaultRefFormat=files init --quiet --bare --template= --object-format=sha1` (no LC_ALL, per the §5.3 list), alternates = realpath of `<common>/objects`, mirrors copied only when present in main, `hooks/pre-push` 0755. The config is rendered by gitio (not `git config` calls) into `config.strata-<nonce>`, fsynced and swapped in, `strata.shadow = true` written last. Any build failure is `Unknown`; the half-built dir is left in place (and is `Exists` next time).
- Copied config: the 10 listed `core.*` keys plus every `filter.*.*`/`diff.*.*`/`merge.*.*` key; values are re-quoted with git's `write_pair` rules; a value-less key is written bare. Subsection case is preserved, section/key names are lower-cased as `git config --list` reports them.
- Ownership: `strata.shadow` must parse as git-true; a value that is not a boolean is `Unknown` reason `bad-config` (it is a config git itself would reject), `false` is not-owned (`InvalidInput`). Alternates' first line must canonicalize to main's objects dir.
- `shadow_drift`: a shadow-side mirror that cannot be read as a regular file (FIFO, symlink, dir) counts as drift; a main-side failure is `Unknown`. Config drift compares the copied groups and also reports any `include.*`, `includeIf.*`, `extensions.*` key in the shadow config. The pre-push guard check runs `git rev-parse --git-path hooks` in the workspace (LC_ALL=C, 10 s timeout); a non-zero exit is `Unknown` reason `hooks-lookup`, a timeout `Unknown` reason `timeout`. A missing `.git` skips the guard check.
- `attach_worktree`: admin-dir creation errors pass `NotDirectory`/`Exists` through, anything else is `Unknown`. The temp index is built at `<tmp>/.git/index` with `read-tree -c core.splitIndex=false --index-output` and `update-index -q --refresh` (both no LC_ALL, 600 s timeout, hooks off, fsmonitor off). Before copying, `<tmp>/.git` is re-verified (dev/ino via a rooted reopen, ctime unchanged); any change → `Unknown` reason `staging-changed` (checked before reporting a read-tree failure); a non-regular index → `Unknown` reason `index-read-back-refused`. The copy keeps the index mtime so the racy-clean check stays valid.
- `worktree_gitdir` returns the gitdir verbatim-joined (no normalisation of `..` or a trailing `/`) and never follows a symlinked `.git`.
- `fsx::read_human_typed` added (typed codes) with `read_human` as its Unknown-wrapping form; `fsx::read_gitfile` shared by `index::locate` and `worktree_gitdir`.
- tests/repo.rs `open_resolves_repo_id_from_main_checkout_only` now builds its workspace with ensure_shadow + attach_worktree.

### Task 8 decisions (attach_staging, stage, commit_tree)

- `attach_staging` and `stage` require lexically canonical absolute paths
  (`InvalidInput`, `reason = "not-canonical"`): rooted apfs opens need it.
- Refusal order in `attach_staging`: absolute/canonical → `tree_of(head)`
  → inside the main checkout (`inside-main`) → staging missing/not a dir
  → `<staging>/.git` exists → `gitdir` exists → gitdir's parent
  missing/not a dir (`NoSuchPath`/`NotDirectory`).
- Passing a human's linked worktree as `staging` (its `.git` gitfile
  points into the common dir) is `InvalidInput` (`reason =
  "linked-worktree"`) rather than `Exists`, per the test table row of
  `index_writers_refuse_the_main_checkout`.
- The private git dir is created rooted at realpath(parent of gitdir);
  `commondir` holds the realpath of the common dir, `gitdir` holds
  `<staging>/.git` as given, the gitfile holds `gitdir` as given.
- The staging index is built with `-c core.splitIndex=false` (as in
  `attach_worktree`) and git writes it in place (spec §5.6 step 3).
- Staging judgement failures are `InvalidInput` with `reason =
  "not-staging"`; `commondir` is read rooted with a PATH_MAX+2 cap.
- `stage`: deletions run before writes; an empty half is skipped (no
  update-index call); `write-tree` always runs. No `LC_ALL` on
  update-index / write-tree.
- `commit_tree`: on failure, identity (`auto-detection is disabled`) is
  checked first, then the tree (`cat-file -t` must say `tree`; missing →
  `NoSuchCommit` with `detail.tree`), then the parent via
  `missing_commit_or`; anything else `Unknown`.
