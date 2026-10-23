# strata-store implementation progress

Branch `impl/store` (worktree `git-strata-wt/store`), from `impl/v1` 779669c.

## Tasks (store.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 open, layout, pinned schema and guards | done | 87e5b26 |
| 2 content-addressed blob put, get and verify | done | (see git log) |

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

## Blockers / notes

- None in strata-apfs so far.
- Merge watch-items: root `Cargo.toml` gains only the
  `"crates/strata-store"` members entry; no `[workspace.dependencies]`
  are added (crate deps are declared in the crate's own Cargo.toml).
- New crate-level dep: `memmap2 = "0.9"` (Task 2).
