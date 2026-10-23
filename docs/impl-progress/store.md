# strata-store implementation progress

Branch `impl/store` (worktree `git-strata-wt/store`), from `impl/v1` 779669c.

## Tasks (store.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 open, layout, pinned schema and guards | done | 87e5b26 |
| 2 content-addressed blob put, get and verify | done | 1e17403 |
| 3 deps sets, attempts, proposals and main | done | d7282b5 |
| 4 base generations with their manifests | done | b633b29 |
| 5 sessions, workspaces, move intents and retire_dir | done | (see git log) |

## Ambiguity decisions

- `Seam::InitBeforeRename` fires before any `Store` exists, so it is armed
  through the thread-local test-seams fn `Store::arm_next_open(seam)`; the
  next `Store::open` on that thread consumes it and returns
  `Unknown{op:"init", outcome:"not_written"}`.
- `Store::debug_pragma(writer, name)` (test-seams only) reads a pragma back
  from the writer or a pooled reader so `open_pins_durable_pragmas` can
  check the pinned values.
- When the clonefile probe reports `Unsupported`, the `LOCK` file is removed
  too if this call created it (alongside the dirs it created), so the
  volume is left exactly as found.
- `open_creates_layout_on_empty_root`'s "reopen leaves it unchanged" check
  compares `store.db` bytes and the file set, ignoring `-wal`/`-shm` and the
  `clean-shutdown` marker (which open removes by design).
- `read_only_open_refuses_every_write` grows with each task: every write
  method checks `read_only` first and returns `ReadOnly{reason:"read_only_handle"}`
  before touching anything (tests in tests/open.rs and later task files).
- The quick_check test corrupts the `blobs` root page (page-type byte) of a
  checkpointed store; a clean reopen skips quick_check (and removes the
  marker), an explicit `quick_check()` reports `Corrupt{reason:"sqlite"}`,
  and an unclean reopen fails with the same error.
- The schema-drift test edits `sqlite_schema` via `PRAGMA writable_schema`
  on a raw non-DEFENSIVE connection (drop a CHECK, change a column type),
  and uses DROP TABLE / DROP TRIGGER for the other two cases.
- Task 2: `Seam::TempNameTaken` makes the collision real: the first temp
  name is created as a regular file containing `taken`, so the clone /
  `create_new` actually hits `EEXIST` and the retry path runs; that file is
  left in `store/tmp` like any stale temp.
- Task 2: files of 16 MiB and up are hashed with `memmap2` over the open
  handle plus `blake3::Hasher::update_rayon`; smaller ones with a
  `read_at` loop. This adds the crate-level dep `memmap2 = "0.9"` (already
  in Cargo.lock as 0.9.12 via blake3's `mmap` feature).
- Task 2: `get_blob`, `get_blob_rooted`, `read_blob`, `verify_blob` work on a
  read-only handle (they write nothing in the store); only the three puts
  check `read_only`.
- Task 2: a `get` failure after the clone landed (meta reset or the
  `open_rooted_file` of the rooted variant) is `Unknown{op:"get",
  outcome:"partial", path}` with `cause`/`cause_detail` keeping the
  original code and detail.
- Task 2: a failed directory barrier after a successful rename is
  `Unknown{outcome:"uncertain"}` (the blob is in place but its durability
  is unknown).
- Task 2: `put_blob_rooted` refuses special files as `NotFile` with reason
  `fifo` / `socket` / `device` (from `attrs_of`, before any clone); a kind
  swapped in after `attrs_of` is caught by `clone_from_rooted`'s handle check.
- Task 2: the 1 000 000 × `a` known-answer vector
  (`616f575a…ff8b`) was fixed with an independent pure-Python BLAKE3
  reference that reproduces the published "" and "abc" vectors; `b3sum`
  is not installed and installing it would touch ~/.cargo outside the
  worktree.
- Task 2: the 500 MB dense roundtrip runs by default (about 5 s on APFS);
  "get is a zero-copy clone" is not asserted directly (no portable probe),
  only byte equality and hole reads.
- Task 3: the write helper (`txn.rs`) checks `read_only` then `poisoned`
  before taking L2, re-checks `poisoned` under L2, runs `BEGIN IMMEDIATE`,
  and on COMMIT distinguishes a definite failure (deferred-FK constraint,
  busy/locked: rolled back, `outcome = not_written`) from anything else
  (`Unknown{reason:"commit_failed", outcome:"uncertain"}` + poison).
  `PauseBeforeCommit` and a plain `CommitSyncFails` injection are wired in
  already; Task 8 may replace the latter with a real VFS fault.
- Task 3: the next id is `max(sqlite_sequence.seq, max(id)) + 1` (the
  `max(id)` only matters if someone edits `sqlite_sequence` by hand); at
  i64::MAX → `TooLarge{limit}`.
- Task 3: `record_deps_set` checks `Exists` before the attempt match (after
  the pure Empty/attempt/dirs validation), so re-recording a sealed set
  reports `Exists` even though its attempt is no longer Running.
- Task 3: `set_deps_attempt_pid` on an attempt that is no longer Running
  returns `false` even when a pid was recorded earlier ("not Running" wins
  over "pid already set", as the §7 test requires).
- Task 3: InvalidInput reasons used: `no_such_attempt`, `pid_already_set`,
  `origin_mismatch`, `from_equals_to`, `tree_head_mismatch`, `no_origin`,
  `no_such_proposal`, `self_supersede`, `repo_mismatch`; field errors for
  `pid`, `to`, `inputs_hash`/`from_inputs`/`to_inputs` (64 lowercase hex,
  checked before the transaction rather than surfacing as a CHECK failure).
- Task 3: `proposals(open_only)` tests the session condition with a raw
  `sessions` row (Task 5 adds the session API); a Removed session's
  AwaitingPublish is filtered by `lifecycle <> 'removed'` in the same query.
- Task 4: `record_base_gen` validates everything it can before the
  transaction: `manifest.tree != gen.tree` → `InvalidInput{field:"tree"}`;
  a mode outside {100644, 100755, 120000} → `InvalidInput{field:"mode", path}`.
  Inside BEGIN IMMEDIATE: `deps_unregistered` (deps set must exist for the
  same repo) is checked before the manifest comparison.
- Task 4 `manifest_mismatch` `detail.path`: entries are compared pairwise in
  path order (byte order, the same as `RepoPath`'s `Ord` and SQLite BINARY);
  at the first unequal pair the smaller of the two paths is reported (when
  only blob/mode/size differ, the two paths are the same). If one list is
  a prefix of the other, the first extra path is reported.
- Task 4: when the tree's manifest is already registered and the base_gens
  row already exists, the transaction is rolled back (no write at all);
  `base_gens.id` is left to SQLite's rowid assignment.
- Task 4: `manifest()` cross-checks `base_trees.files` against the entry
  count and re-validates mode/size/path/blob, reporting `Corrupt{reason:"bad_row"}`.

- Task 5 layout checks: `create_session` requires `manifest.root` to be
  exactly `<root>/sessions/<id>` (text form of the configured root) and each
  workspace path `<session root>/<repo>`; a base must be registered and
  `base.repo` must equal the map key. Duplicate id → `Exists{reason:"store_row"}`,
  root/path taken by another session → `Exists{reason:"root_taken"|"path_taken"}`.
- Task 5 staging shape (`begin_move` staging, `set_old_tree` path): text
  `<root>/sessions/<id>/<name>`, one component, not `.`/`..`, and not the
  name of one of the session's workspaces → otherwise
  `InvalidInput{reason:"staging_shape"}`. This also excludes `<root>/store`.
- Task 5 guard order in `begin_move`: NoSuchSession (missing or Removed) →
  lifecycle → NoSuchRepo → untracked → health_unknown → repo_mismatch /
  base_unregistered → staging_shape → open intent `Exists{n}`; n =
  max(generation, max intent n) + 1 (`TooLarge{limit:"move_generation"}` on
  overflow).
- Task 5 `set_move_state` only takes the Preparing → Swapping|Refused|Abandoned
  edges (the others belong to finish/fail/resolve); it is compare-and-swap on
  state and returns false when the row is not in `from`. Entering Swapping
  needs an Active session. An unknown n → `InvalidInput{reason:"no_such_intent"}`;
  a wrong state for phase/finish/fail/old_tree → `InvalidInput{reason:"intent_state"}`.
- Task 5 `resolve_move` with an n that is not the latest intent or not in
  state Unknown (including n beyond i64) → `no_failed_move`; Abandon keeps the
  intent's conflicts and the workspace scene; Accept copies the intent's
  conflicts and sets `old_tree = staging`.
- Task 5 `retire_dir` path check: text must start with `<configured root>/`,
  the rest must be a valid NFC `RepoPath` with no empty/`.`/`..` components
  and match one of the 7 table rows, else `InvalidInput{reason:"path_shape"}`;
  a row whose live-row check fails → `InvalidInput{reason:"not_retirable",
  row, check}`. Leading-zero / zero numbers (`worktrees/02`, `.staging/0`)
  are shape failures. A symlink anywhere (from apfs NotFile/NotDirectory
  `reason:"symlink"`) → `InvalidInput{reason:"symlink"}`; sockets/devices/files →
  `NotDirectory`. Trash name is `store/trash/<unix-ms>-<hex32>`; the trash
  dir is F_FULLFSYNCed (counted in `dir_barriers`). `RetirePrepFails{errno}`
  fires after the open/fstat and before `make_removable`.
- Task 5 row 3 for a missing attempt row: accepted only if the number is ≤
  the `sqlite_sequence` value of `deps_attempts` (issued, then gc'd).

## Blockers / notes

- None in strata-apfs so far.
- Merge watch-items: root `Cargo.toml` gains only the
  `"crates/strata-store"` members entry; no `[workspace.dependencies]`
  are added (crate deps are declared in the crate's own Cargo.toml).
- New crate-level dep: `memmap2 = "0.9"` (Task 2).
- One unexplained failure of one `tests/open.rs` test in a single full
  `cargo test -p strata-store` run during Task 3 (output not captured);
  not reproduced in 26 further runs. Watching.
