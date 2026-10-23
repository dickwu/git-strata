# strata-store implementation progress

Branch `impl/store` (worktree `git-strata-wt/store`), from `impl/v1` 779669c.

## Tasks (store.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 open, layout, pinned schema and guards | done | (see git log) |

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

## Blockers / notes

- None in strata-apfs so far.
- Merge watch-items: root `Cargo.toml` gains only the
  `"crates/strata-store"` members entry; no `[workspace.dependencies]`
  are added (crate deps are declared in the crate's own Cargo.toml).
