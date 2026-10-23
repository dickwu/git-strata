# strata-publish implementation progress

Branch `impl/publish` (worktree `git-strata-wt/publish`), from `impl/v1` 95f7660.

## Tasks (publish.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 chain ref and anchor advance | done | fd86995 |
| 2 path-level stage and merge | done | 8ad75bd |
| 3 read-only checks | done | (this commit) |
| 4 commit, push, and unknown | todo | |
| 5 rebase, assign, and diff | todo | |
| 6 one merge under the repo lock | todo | |

## Lower-layer additions (Task 1 commit)

publish.md §2 item 3 and §4.6 ask the lower layers for pieces they did not
have; they land in the Task 1 commit with the crate scaffold:

- gitio `publish_io.rs`: `Git::ref_transaction(&[RefUpdate])` (one
  `update-ref --stdin` start/prepare/commit; a ref not at its `old` →
  `HeadMoved {ref, expected, actual}`, nothing moved), `tree_items`,
  `blob_bytes` (reads B without exporting it), `object_bytes`.
- gitio `push.rs`: `push_published(staging, remote, src_ref, dst_branch,
  anchor_ref, timeout)`: after a successful push one ref transaction moves
  the remote-tracking ref and the anchor together; only then `Pushed`. A
  failed transaction after the remote accepted → `Ok(Unknown)`. `push()` is
  unchanged in behaviour (its send path became the shared `push_send`).
- gitio seams: `PUSH_BEFORE_SEND`, `PUSH_BEFORE_RECORD` (test-seams only).
- store: `set_publish_outcome(id, phase, outcome)` (rewrite the outcome of a
  row in a given phase; used to keep the commit in an `Unknown` row) and
  `publishes()` (every row, for `assigned` and restart reporting).

These touch shared crates; a parallel worker (search) may conflict there.

## Ambiguity decisions

- `Ctx` gains `sessions: &Sessions` (not in the §3 list): §5.8 needs
  `ensure_base_for` and `move_workspace`, which only the session handle has.
- Task 1 `chain_rewound_or_diverged_refuses_before_write`: until `publish`
  exists (Task 4) the test drives `chain::refuse_unpublishable`, the
  predicate `publish` uses both outside and inside the lock, and asserts the
  refs are byte-identical. Task 4 adds the end-to-end half (no row commit,
  no push).
- Task 1 `publish_branch_is_not_default_branch`: asserts B, the chain/anchor
  names and that `main_tip` ignores a leading release chain. The push-target
  half (`origin/<publish.branch>`) is asserted in the Task 4 push tests.
- The test `PublishLocks` double: `with_repo` delegates to the same
  `TestLock` deps uses (one repo write lock); `with_sessions` is a separate
  per-session queue with a bounded wait → `Busy`. Holds record wall-clock
  start/end so Task 5 can check no install overlaps a hold.

### Task 2 (stage and merge)

- `stage()` decides the whole merge in memory (`stage::plan`) before any
  directory exists; `stage::build` then creates the layout (D4). `publish`
  (Task 4) calls the two halves separately so the confirm gate sits between.
- Base side bytes come from the session's pinned base directory (rooted
  read, never following a symlink); `ours` from the checkpoint blob in the
  store; `theirs` (B) through `tree_items` / `object_bytes`. Identity is
  compared by BLAKE3 when both sides have one, by git oid when both have
  one, else by bytes (only when both are ≤ `MERGE_MAX_BYTES`). Two oversize
  sides that cannot be compared by id are treated as different → a
  whole-file `Conflict(kind = "too_large")` (conservative).
- Selection: a batch path first matches entries exactly (`path` or a
  `Renamed.from`), else every entry below it as a directory prefix; nothing
  → `NoMatch`. A `Renamed` deletes its `from` (unless the checkpoint has an
  entry there). An unselected entry that is an ancestor/descendant of a
  selected one → `Conflict(reason = "type_pair")`, before the merge.
- A batch naming only an empty directory is `NoMatch` (the overlay has no
  entry for it); the directory is never written.
- Conflict detail: `{reason: "merge", conflicts: [{path, kind, session,
  checkpoint, hunks: [...]}]}`; kinds `text`, `binary`, `too_large`,
  `modify_delete`, `add_add`, `link`, `mode`, `type`. Every conflicting
  path is collected before stopping. Hunk text is capped at 4096 bytes at a
  UTF-8 boundary (lossy for non-UTF-8 text); binary/oversize give one
  `whole_file` hunk with empty text and the two sizes.
- `.lww`: rules (and `%include`s, ≤ 16 files) are read from B through
  git objects. Between two batches of one session the larger
  `CheckpointId` wins; otherwise the later-applied batch wins; against
  upstream B (no batch wrote the current result) the batch wins.
- A file result where B still has surviving entries below that path →
  `Conflict(kind = "type")`. The reverse (a new path below a B file that is
  not deleted) is not pre-checked; gitio `stage` / the rooted writes refuse
  it.
- Modes written: only `100644` / `100755` / `120000` (any exec bit → 755);
  files are created 0600 and chmodded explicitly, so umask and setuid bits
  never reach the tree. Directory modes are not written.
- The setuid fixture is a workspace file chmodded 4755 / 2644 (the store
  refuses a checkpoint entry with special bits, so the "should not happen"
  case cannot be recorded through `record_checkpoint`).
- `stage_goes_through_gitio_stage` allows gitio's own `export_tree`
  `read-tree` (private `GIT_INDEX_FILE`) besides `attach_staging`/`stage`.

### Task 3 (checks)

- `check(cfg, staged)` runs `validate_commands` itself as well (the gate in
  `publish` runs it earlier, before any directory). Each check line is split
  by the config splitter (reached through `install_argv` on a clone, since
  `split_command` is private), run with `cwd = staged`, no shell, stdin
  `/dev/null`, inherited environment minus every `GIT_*` variable (the
  staging gitfile decides the repository). No timeout is applied (the spec
  names none).
- The first failing check stops the run; later checks do not run.
  `CheckFailed.detail = {command, index, exit_code, signal, output_tail,
  checks_ran}`; `output_tail` is the last 8 KiB of stdout followed by stderr
  (two pipes, not interleaved), lossy UTF-8. A spawn failure counts as a
  failed check (`exit_code: null`, `spawn_error`).
- The layout root is `staged.parent()`, removed with `remove_tree_rooted`
  relative to its parent. Incomplete removal → `Unknown` (reason cleanup)
  with the check detail under `detail.check`.
- The "cannot remove" test uses the apfs `before_open` seam to swap a
  directory for a symlink mid-removal (no chflags/root needed).
