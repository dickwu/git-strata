# strata-search implementation progress

Branch `impl/search` (worktree `git-strata-wt/search`), from `impl/v1` 95f7660.

## Tasks (search.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 view table and typed refusals | done | e82784a |
| 2 share base index by main checkout tree and profile | done | 75aadbc |
| 3 session overlay shadows the shared base | done | c065675 |
| 4 pin the query snapshot and refuse a replaced root | done | (this commit) |

## Decisions

- tgrep-core is vendored (design §5 "不可行则 vendor 所需模块"). Upstream
  `HybridIndex::from_reader` is `pub(crate)`, and §4.2 requires the view's
  reader to be `generation.base().reader()` itself, not a second `open`.
  `vendor/tgrep/` holds tgrep-core 1.2.4 and its path dependency
  `vendor/ignore`; the only source change is `from_reader` made `pub`
  (`vendor/tgrep/PATCHES.md`). The clone at `/Volumes/GWD/opensource/tgrep`
  is untouched. The root workspace has `exclude = ["vendor"]`. Because of
  this the Task 1 commit also adds `vendor/`, the root `Cargo.toml` and
  `Cargo.lock`, not only `crates/strata-search`.

## Ambiguity decisions

- Task 1: `Sessions::status` reports a non-directory at the workspace path as
  health `Missing`. §5.1 step 4 maps `Missing` to `NoSuchPath`, but §7
  `attach_missing_workspace_is_no_such_path` wants `NotDirectory` for a
  regular file. `attach` maps `Missing` to `NoSuchPath` only when nothing is
  at the path (lstat fails); otherwise step 7's `open_root` names it
  (`NotDirectory`).
- Task 1: a session in `Unknown` state answers `Unknown` with
  `reason = "session_unknown"` and the session scene; a workspace in
  `Unknown` health answers `Unknown` with `reason = "workspace_unknown"` and
  the workspace scene (§5.1 names the code and the scene, not a reason).
- Task 1 tests make `Creating` with `Store::create_session` of a second
  manifest (no `Sessions::new` stops in Creating without a seam), and
  workspace `Unknown` with a store move intent that failed while swapping.
- Task 2: tgrep has no hook between creating its `.stage-*` TempDir and the
  publishing rename, and on a real failure the TempDir deletes itself. The
  `FailEnsureAfterStage` seam therefore stands in for tgrep: it creates
  `<storage>/<identity>/.stage-<hex>` itself and returns `Unknown`
  (`index_build`, `detail.stage`) without calling `ensure`. The product
  path relies on tgrep's own cleanup; a stale stage never blocks a later
  build (tested).
- Task 2: `ensure` errors are classified after `resolve_commit_tree` runs
  first: git NotFound → `Unsupported` `git_missing`; a git failure →
  `NoSuchCommit`; a 64-hex oid → `Unsupported` `sha256`. A failure of
  `ensure` itself is `Unknown` `index_unreadable` when the key's
  `gen-<storage_name>` was already published before the call, else
  `Unknown` `index_build`. The key is built through serde from tgrep's
  public fields (`meta::INDEX_FORMAT_VERSION`, schema 1, because
  `GenerationKey::legacy` and `SCHEMA_VERSION` are crate private). Discover
  failures other than git missing are `Unknown` `index_storage`;
  `with_storage`'s `Unsupported` is `Unsupported` `index_storage`.
- Task 2: `generation_ptr` is `Arc::as_ptr(generation.base().reader())`: one
  generation has one reader, and the same number is comparable with Task 4's
  `paused_reader_ptr`.
- Task 2: generations are held as strong `Arc`s per (repo, tree) for the
  life of `Search` (tgrep's process cache is weak). `predecessor` is any
  other opened tree of that repo. The bare-extension-looking text file
  (`assets/logo.png`) is `Indexed` under the default eligibility.
- Task 2: §7 `base_index_shared_across_sessions_same_tree` "clean files do
  not enter the private live" needs the view overlay (Task 3) and is
  asserted there; Task 2 asserts one `gen-*`, no stage, equal pointers. The
  `max_blob_bytes = 3` half of `same_tree_oid_different_repos_do_not_share`
  builds with tgrep directly in a scratch storage; the `files` listing half
  is `oversized_file_listed_not_indexed` (Task 4).
- Task 2: tests make "head not in the main checkout" by orphaning the main
  checkout and pruning (sessions start from `origin/<default_branch>`, so
  fixtures push to origin first).
- Task 2's commit also carries `tests/views.rs` (the generation directory
  assertion of `unknown_view_is_no_such_view`, deferred from Task 1) and
  this file, beyond §8's `git add` list.
- Task 3: a view's private state is an op map path → `Upsert(bytes)` /
  `Delete`, not a long-lived `LiveIndex`. Every publish builds a fresh
  `HybridIndex::from_reader(generation reader)` and replays the map, and
  publishes it as an immutable `Arc<Snap>` (index + members + root
  identity). A query clones that `Arc` under the view lock and then reads
  without locks, which is how §4.5's "one query, one reader" is kept
  (HybridIndex is not `Clone`, and swapping a reader under a running query
  is what Task 4 must rule out). Cost: a publish re-trigrams every overlaid
  file, from memory, without re-reading the workspace.
- Task 3: `Delete` (mask the base postings) is used for tombstones,
  `Renamed.from`, excluded paths, and for overlay/dirty paths whose
  workspace content search does not index: a symlink (never opened),
  bytes over `MERGE_MAX_BYTES`, bytes with a NUL, or a file that is gone.
  Those paths stay members for `files` (§4.4). A `Delete` of a path the
  reader does not contain is not applied (§7 `candidate_count…`).
- Task 3: §5.4 step 4 reads every non-hidden regular base file on a full
  fill and keeps the base when `matches_worktree_bytes` holds; a ready
  view's refresh rereads only `changed` plus their `paired` groups in both
  the previous and the new overlay (so a `from`-only or `path`-only change
  updates both ends even if the pair split). `attrs_of` / `unchanged_since`
  caching is not used (always reread those paths).
- Task 3: reads are stable reads (fstat before/after, one retry) through
  `open_rooted_file`. At fill/refresh `NoSuchPath` is not a failure; any
  other refusal (symlink, FIFO, directory now at a member path) or an
  unstable read leaves the view not ready (`Unknown` `not_ready`). At grep
  verify, a candidate that is gone or no longer a regular file is skipped.
- Task 3: exclusions are `.git`, `.strata`, `deps_dirs`, `seed_caches`,
  matched as the path itself or a `/`-prefix, folded with `case_fold_key`
  on a case-insensitive volume (`volume_caps(workspace).case_sensitive`,
  read at attach). They apply to base entries and overlay entries alike.
  Gitlinks are not members.
- Task 3: "clean files do not enter the private live" (§7
  `base_index_shared_across_sessions_same_tree`) needs to see inside a
  view; §3 declares no such seam. A `#[doc(hidden)]` `test-seams` method
  `Search::private_paths(view)` (paths with private bytes) was added and is
  asserted in `overlay_entry_shadows_base_path` (and after reverting in
  `commit_inside_session_keeps_overlay_hit`).
- Task 3: `grep` / `files` / `refresh` beyond §8 Task 3's text are already
  complete (root re-check after the query, refresh's root/base checks and
  "no publish when the root changed", not-ready refresh once in grep);
  Task 4 adds the `PauseBeforeVerify` / `swap_view_reader` seams' tests.
  `refresh` with `overlays.last() == None` on a tracked workspace answers
  `Busy` `overlay_not_ready` like `attach`.
- Task 3's commit also carries `src/lib.rs` (attach step 13, the view
  fields, grep/files/refresh), a `cfg_attr` dead-code allow in `src/base.rs`
  (two seam-only helpers), and this file, beyond §8's `git add` list.
- Task 3 tests were written after the implementation in this session (both
  in the same commit); with the Task 2 stubs (`grep`/`files` returned empty)
  they fail.

### Task 4

- No implementation change was needed: the Task 1–3 code already pins
  `(reader, members)` in one `Arc<Snap>` per query and rechecks every root
  after verify. Task 4 adds `tests/snapshot.rs` (13 tests).
- **Base change in `new_query_after_reattach_sees_new_base`.** Sessions has
  no rebase, so the test publishes a new upstream commit (drops `old.ts`,
  adds `new.ts`), builds its base with `Sessions::ensure_base`, and calls
  `Sessions::move_workspace`. Like strata-session's own move tests, it does
  not wrap the call in `fx.lock_write`. The old view gets `Busy`, with
  either `root_changed` or `base_moved` accepted: move replaces the workspace
  dir, so in practice it's `root_changed`. A re-attach gets a new id and sees
  only new-tree hits.
- **"Replace the root".** Tests do this by renaming the workspace dir aside
  and `cp -Rp`'ing it back to the same path, so it has a new inode and the
  same gitfile. That way the session status stays healthy, and a re-attach
  works on the replacement.
- **Reader swap.** Tests swap readers with `swap_view_reader` onto a second
  session's tree (T2 = T1 + `extra.ts`). The paused query's
  `paused_reader_ptr` stays at T1's `generation_ptr` and its ids never
  resolve to `extra.ts`.
- **Candidate count.** "Candidate count" is observed as the member count
  (`files("**").len()`) plus one paused `grep(".")`, whose hit set equals the
  pre-swap answer. tgrep's `num_files_using` isn't exposed through the API
  and the spec only needs membership consistency.
- **Oversized files.** `oversized_file_listed_not_indexed` adds `Untracked`
  entries for the two 64 MiB files to a captured overlay by hand, instead of
  capturing them, so it doesn't copy 128 MiB into the store. Search never
  reads the overlay blob, only the workspace bytes.
- **FIFO handling.** `verify_does_not_follow_symlink` runs under a 60 s
  bound. A FIFO in place of a clean base file is skipped by verify (no hang).
  An explicit refresh of it gives `Unknown not_ready`, and so does the next
  query. A symlink in place of a base file yields no target bytes.
