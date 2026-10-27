# strata-store implementation progress

Branch `impl/store` (worktree `git-strata-wt/store`), from `impl/v1` 779669c.

## Tasks (store.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 open, layout, pinned schema and guards | done | 87e5b26 |
| 2 content-addressed blob put, get and verify | done | 1e17403 |
| 3 deps sets, attempts, proposals and main | done | d7282b5 |
| 4 base generations with their manifests | done | b633b29 |
| 5 sessions, workspaces, move intents and retire_dir | done | d692877 |
| 6 atomic checkpoint records with store-assigned ids | done | c06f7e2 |
| 7 publish records and ref audit | done | 832866c |
| 8 busy, reader isolation, crash durability and poisoning | done | 6671503 |
| — metadata engine moved to Turso (user decision 2026-10-23) | done | d52d206 |
| 9 gc with guards, trash and integrity scan | done | 3e116ff |
| — turso bumped to =0.9.0-pre.1, gaps re-checked | done | caaba3b |

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

## Engine: Turso

User decision (2026-10-23): the metadata DB runs on Turso (crate `turso`,
pinned `=0.9.0-pre.1` since 2026-10-23, was `=0.8.3`;
`default-features = false`) instead of rusqlite, so a
future embeddings/vector table can live in store.db. Blobs stay BLAKE3
files. No vector features are added; nothing in the schema check blocks a
later migration that adds such a table (it would bump `user_version`).

- Sync bridge: `src/sql.rs` is a small blocking facade (Connection,
  Statement, Row, `params!`, ToSql/FromSql) over the async Turso API. Its
  `block_on` polls the future on the calling thread; the waker unparks the
  thread and the loop `park_timeout(1ms)`s between polls. No tokio, no
  extra threads. All store modules call only the facade.
- Cross-process: every connection opens with
  `experimental_multiprocess_wal(true)` (creates `store.db-tshm`); without
  it a second process cannot share the file. This is an experimental Turso
  feature, hence the exact version pin. Verified on this Mac: daemon writer
  + CLI read-only child see each other's commits (`tests/engine.rs`).
- WAL: Turso's default; header bytes 18/19 = 2/2 checked at open;
  `journal_mode` reads back `wal`. Clean `close()` runs
  `wal_checkpoint(TRUNCATE)` (Turso does no last-connection checkpoint).
- BEGIN IMMEDIATE / transactions: a second process's BEGIN IMMEDIATE is
  `Busy` at once with `busy_timeout = 0`; readers are not blocked and see
  only committed data. A deferred-FK failure at COMMIT is `Constraint` and
  leaves the tx open (txn.rs rolls back), as with SQLite.
- Durability: `synchronous = FULL` and `fullfsync = 1` are pinned and read
  back; Turso uses that one sync type for commits and checkpoints. SIGKILL
  after a returned commit loses nothing (`tests/crash.rs`). The test VFS
  (`src/vfs.rs`) is now a Turso `IO` wrapper counting WAL syncs.
- Migration (2026-10-27): `user_version` 1..=`LATEST` (2) is accepted,
  `application_id` pinned; anything newer or older is refused unchanged.
  The writer open migrates forward in one `BEGIN IMMEDIATE` after a
  verified clone backup at `store/backups/store-v<from>-<ns>.db`
  (store.md §4.4a); v1 -> v2 adds `sessions.agent`. The `user_version`
  written in the transaction is authoritative (opens read the PRAGMA);
  the post-COMMIT TRUNCATE checkpoint is best effort. Tests:
  `tests/migrate.rs` on a real v1 fixture (`tests/fixtures/store-v1.db`,
  made by the v1 code at main 31769e3).
- Integrity: `quick_check` / `integrity_check` work; a corrupt page is
  `Corrupt`, a non-database file is refused.

Gaps (closest safe behaviour chosen; each is a deliberate difference from
the rusqlite design):

1. No application-defined SQL functions: `strata_gc_active()` is replaced
   by the one-row table `strata_gc_gate(active)` read by the
   `*_delete_gc_only` triggers. gc flips it to 1 inside its own write tx
   and back to 0 before COMMIT. A raw client with write access could flip
   it too (with rusqlite only our process had the function).
2. The builder's read-only flag is not honoured within one process (Turso
   keeps a per-process database registry); readers pin `query_only = 1`
   instead and writes through them fail.
3. No `SQLITE_DBCONFIG_DEFENSIVE`; Turso already refuses writes to
   `sqlite_schema`.
4. `checkpoint_fullfsync`, `recursive_triggers`, `trusted_schema` are
   unknown pragmas (silently ignored), so they are no longer pinned.
   `fullfsync` covers checkpoints too (see Durability).
5. Without recursive triggers, `INSERT OR REPLACE` deletes the old row
   without firing BEFORE DELETE/UPDATE guards, so a raw REPLACE bypasses
   the delete and immutability triggers. The store never uses REPLACE;
   `tests/guards.rs` records this as "ENGINE GAP" instead of failing.
6. STRICT tables cannot type `(a IN (..)) = (b IS NULL)` in CHECK; four
   CHECKs (deps_attempts.error, move_intents.ended_ns and conflicts,
   checkpoints.reason_text) are spelled in the equivalent AND/OR form.
7. Turso stores the CREATE text normalized and adds
   `__turso_internal_seq_*` tables, so a store.db made by the old
   rusqlite build fails the schema check (`Corrupt{schema}`). No deployed
   stores exist. (Since 2026-10-27 `user_version` is 2; see Migration.)
8. There is no READONLY_RECOVERY error code to map; that case is not
   reachable.
10. After another process changes the schema (a migration), an existing
   connection fails each explicit `BEGIN` read with "Database schema
   changed" until it runs one autocommit statement. `read_txn` runs
   `SELECT count(*) FROM sqlite_schema` outside a transaction and retries
   the read once (`tests/migrate.rs::read_only_handle_survives_another_process_migrating`).
11. `GLOB` and `length()` stop at an embedded NUL, so the
   `sessions.agent` CHECK adds `length(CAST(agent AS BLOB)) = length(agent)`
   (also rejects non-ASCII); `tests/guards.rs` checks the CHECK against
   `AgentName` case by case.
9. (Not an engine gap; closed.) The flaky `Busy` on `store/LOCK` right
   after a close + reopen came from `tests/open.rs` spawning a `sleep`
   child while sibling tests in the same binary had stores open. On macOS
   a spawned child holds a reference to every parent descriptor,
   `O_CLOEXEC` included, until its exec finishes image activation (100 ms+
   under load); a `flock` lives on the open file description, so a sibling
   test that closed and reopened in that window saw `Busy` (hit
   `open_creates_layout_on_empty_root` and `open_refuses_non_wal_store`
   too, not only the lock test). Fix: the test moved to its own binary
   `tests/lock_inherit.rs`, and it waits for a `sh -c 'echo ready; exec
   sleep 60'` child to print before closing, so its exec is known done.
   The 2 s Busy retry is gone. The same race hit `tests/crash.rs`
   (`commit_io_error_is_unknown_and_poisons_writer` vs. the crash child),
   so `tests/support` now has a spawn window: `run_child`, `spawn_child`
   (until the child's first stdout byte), `hdiutil` and `touch` hold it
   exclusively, `Env::open`/`try_open` hold it shared. Real-world note: a daemon spawning children
   can make a CLI racing a daemon restart see a transient `Busy`, which
   callers already treat as retryable.

### Turso 0.8.3 → 0.9.0-pre.1 (2026-10-23)

The `turso` crate's public API is unchanged (doc comments only); the bump
built with no source changes. Each gap was re-probed with a throwaway test
against the new engine (not kept).

| # | Gap | 0.8.3 | 0.9.0-pre.1 | Design now |
|---|-----|-------|-------------|------------|
| 1 | App-defined SQL functions | none | **changed, still open for us**: `turso_core::OpenOptions::extension_function(ExternalFunc::new_native_scalar(..))` registers native scalars database-wide at open, but the public `turso::Builder` (via `turso_sdk_kit`) has no hook. A trigger calling an unknown function is created, then fails on DELETE with "no such function". | gate table `strata_gc_gate` kept; not moving to unstable core internals |
| 2 | Read-only flag in-process | ignored | still ignored (a read_only Builder in the writer's process can INSERT) | readers keep `query_only = 1` |
| 3 | DEFENSIVE | none; `sqlite_schema` writes refused | same | unchanged |
| 4 | `checkpoint_fullfsync`, `recursive_triggers`, `trusted_schema` | unknown, ignored | still accepted and read back nothing (`cell_size_check` too) | not pinned; `fullfsync` covers checkpoints |
| 5 | REPLACE fires delete triggers | no | still no (`INSERT OR REPLACE` and `REPLACE INTO`, with `recursive_triggers` 0 or 1) | guards.rs ENGINE GAP leniency kept |
| 6 | Original CHECK `(a IN (..)) = (b IS NULL)` in STRICT | rejected | still rejected ("cannot determine type of expression in CHECK constraint; use CAST") | AND/OR form kept |
| 7 | Schema text kept verbatim | normalized, `__turso_internal_seq_*` added | same | unchanged |
| 8 | READONLY_RECOVERY error code | none | none | unchanged |
| 9 | LOCK `Busy` flake | flaky, 2 s retry | root-caused (test spawn + `flock`, not engine) | own binary + ready handshake, test spawn window; retry dropped |
| – | Multi-process WAL | `engine.rs` / `crash.rs` green | green: 5 full `cargo test -p strata-store` runs (engine, crash, gc 5× each), all pass | `experimental_multiprocess_wal` kept |

Verdict: no regression and no closed gap that changes the design; the pin
moves to `=0.9.0-pre.1` (a pre-release, so watch for 0.9.0 final).
Results: `cargo test -p strata-store` 133 passed / 0 failed / 3 ignored
(×6 runs after the spawn-window fix); `cargo test --workspace` 301 / 0 / 5
(×2); standalone repeats: `lock_inherit` 30/30, `open` 40/40 + 10/10,
`crash`, `engine`, `gc`, `concurrency` 10/10 each.

## Blockers / notes

- Store crate is clippy-clean (`--all-targets`, with and without
  `test-seams`); the remaining workspace clippy warnings are in
  strata-apfs, which this worktree does not touch.

- None in strata-apfs so far.
- Merge watch-items: root `Cargo.toml` gains only the
  `"crates/strata-store"` members entry; no `[workspace.dependencies]`
  are added (crate deps are declared in the crate's own Cargo.toml).
- New crate-level dep: `memmap2 = "0.9"` (Task 2).
- Crate deps after the engine move: `turso = "=0.9.0-pre.1"` (default
  features off; was `=0.8.3`), `memmap2`, `libc`; `rusqlite` is gone from deps and tests.
- One unexplained failure of one `tests/open.rs` test in a single full
  `cargo test -p strata-store` run during Task 3 (output not captured);
  not reproduced in 26 further runs. Watching.
- Task 6: validation (step 1) runs before the read-only / poison checks, as
  §5.8 orders it; an incoherent overlay is `InvalidInput{reason:"entries",
  why}` with `why` in duplicate_path, rename_to_self,
  rename_source_has_entry, duplicate_rename_source, mode. Two `Renamed`
  entries with the same `from` are refused too (the DDL has a unique index
  on `from_path`).
- Task 6: a session in any lifecycle but `removed` takes checkpoints
  (`before_remove` checkpoints are written while `removing`); `removed` and
  missing are both `NoSuchSession`.
- Task 6: `cp.base.repo != cp.repo` is `InvalidInput{reason:"base_repo"}`;
  a base not registered is `InvalidInput{reason:"base_unregistered"}`; an
  untracked workspace is `InvalidInput{reason:"untracked"}`. The base is not
  required to equal the workspace's current base.
- Task 6: the `AfterCheckpointRow` seam fails with an injected EIO on
  store.db: `Unknown{op:"record_checkpoint", outcome:"not_written"}`, and
  the whole transaction rolls back.
- Task 6: a repeated `ToolCall{id}` rolls back and returns the existing id
  without lstat-ing anything new beyond step 3 (blob checks still run first,
  per the §5.8 step order). `Manual{message}` with the same text is not a
  tool call.
- Task 6: blob rows registered by a checkpoint get `created_ns = now()`
  (not `cp.at`).
- Task 6: `checkpoint(session, id)` binds `u64::from(id) as i64`, so an id
  built with `CheckpointId::from(-5i64)` finds a negative row and reports
  `Corrupt{bad_row}` rather than `NoSuchCheckpoint`.
- Task 6: `late_source_failure_writes_no_checkpoint` checks that the k−1
  blobs stay in `blobs/` (unregistered); the "gc collects them past the age
  floor" half is covered in Task 9 (gc does not exist yet).
- Task 7: `record_publish` ignores `group.id` and assigns max+1 (overflow is
  `TooLarge{limit:"release_group_id"}`). Empty `batches` or an empty `paths`
  list is `InvalidInput`; each batch's session must exist and not be
  `removed` (`NoSuchSession`), its `(session, checkpoint)` must exist
  (`NoSuchCheckpoint`), and the checkpoint's repo must equal `batch.repo`
  (`InvalidInput{reason:"checkpoint_repo"}`). Validation runs before the
  read-only check, as in Task 6.
- Task 7: `set_publish_phase` refuses statically illegal transitions (only
  running→done, running→unknown, unknown→done) with
  `InvalidInput{reason:"transition"}`. An unknown publish id is
  `InvalidInput{reason:"no_such_publish"}` (a caller bug), whereas an
  existing publish in a different phase returns `Ok(false)` (lost CAS).
  `outcome: None` keeps any earlier outcome (unknown→done after a
  running→unknown with an outcome keeps it); `ended_ns` is always the
  latest transition time.
- Task 7: `record_ref_change` validates the op/outcome pairing and the
  `refs/strata/` prefix for `update_ref` up front (`InvalidInput`), and a
  `publish` that does not exist is `InvalidInput{reason:"no_such_publish"}`.
  `confirmed` is stored as the serde JSON of `Action`. `ref_changes` orders
  by `(at_ns, id)`; an unknown publish gives an empty list.
- Task 7: `publish_record` rebuilds batches from `publish_items` ordered by
  `(batch, item)`; gaps, a batch whose items disagree on
  session/repo/checkpoint, or a publish with no items are `Corrupt{bad_row}`.
- Task 8: the instrumented VFS (`src/vfs.rs`, test-seams only) is a shim
  `strata-instr` over the default `unix` VFS; under test-seams every store
  connection (writer, readers, init temp db) opens through it. Counters are
  per database, keyed by the main db's full path as SQLite reports it
  (`Connection::path()` of the writer), so `sync_counts()` reports only this
  store's WAL syncs even with several stores in one test process.
  `Seam::CommitSyncFails` sets a fail flag that makes the next WAL `xSync`
  return `SQLITE_IOERR_FSYNC`; it is armed just before `COMMIT` and
  cleared right after, so a real SQLite commit error drives the
  `uncertain` + poison path (no faked error).
- Task 8: "every write function" includes the blob puts: `put_blob`,
  `put_blob_rooted` and `put_bytes` check `writable()` and refuse with
  `Unknown{reason:"poisoned"}` once the writer is poisoned, like the
  SQLite writes. Reads (including `get`/`read_blob`/`verify_blob`) work.
- Task 8: COMMIT errors that are constraint/busy/locked are `not_written`
  (SQLite keeps the transaction open; we roll back); any other COMMIT error
  is `uncertain` and poisons. After reopen the uncertain checkpoint may or
  may not be present; the test accepts either and checks the replay yields
  exactly one row for the tool call.
- Task 8: child-process helpers re-exec the test binary with
  `STRATA_STORE_TEST_CHILD=1`; children write marker lines on raw stdout
  preceded by a newline and the parent matches the marker anywhere in a
  line (libtest prints `test name ... ` without a newline first). A
  `ChildProc` kills its child on drop so a failing parent never leaves a
  lock holder behind.
- Task 9: gc's delete triggers are opened by the Turso gate row (gap 1):
  each gc batch sets `strata_gc_gate.active = 1` first thing in its write
  tx and back to 0 just before COMMIT; a rollback restores 0 by itself.
- Task 9 guards: (a) owned paths = staging of open intents
  (preparing/swapping/unknown) and of running/failed attempts; (b) the age
  floor (default 3600 s, `Seam::AgeFloor`) uses the later of mtime and
  ctime; (c) blob and tmp sweeps hold L3 exclusive, so an in-flight put
  (even with an old source mtime) is never swept, and a symlink put does not
  retake L3.
- Task 9 order in `gc`: removable sessions → proposals → closed attempts →
  old gc_runs (newest per scope kept) → unreferenced blob rows, then orphan
  blob files → tmp → trash (age from the `<ms>-` name prefix) → scrub
  (budget 2 GiB, resume cursor in `gc_runs.scrub_after`). `gc_repo`:
  candidate gens/deps, make_removable on each root (failure → `stuck`, row
  kept), delete rows with a pin re-check per batch, retire into
  `store/trash/<ms>-<hex32>`, then sweep unregistered key-shaped dirs
  (including stale `bases/<repo>/.staging/<hex32>`; `.staging` itself
  stays). A stop after retire is `Unknown{outcome:"partial"}`, recorded in
  `gc_runs` as `partial`.
- Task 9 proposals: a decided proposal past keep is deleted unless a kept
  proposal reaches it through `superseded_by`; a decided row that points
  at a kept (pending) row is itself deletable. Deletion follows reference
  order, never id order (`GcBatchRows{rows:1}` test).
- Task 9 test adjustments: `begin_move` only accepts staging under the
  session root, so "an intent whose staging sits under bases/" cannot be
  built; the owned-path test uses a valid session staging instead. APFS is
  case-insensitive, so the "uppercase hex .staging name is not swept" case
  uses a name that does not fold onto any lowercase one. All 25 gc tests run
  by default (no hdiutil or human needed; uchg via `lchflags`).
- Task 9 also removed three dead helpers in errors.rs (`detail_str`,
  `json_err`, `R`) so `cargo build` is warning-free.
