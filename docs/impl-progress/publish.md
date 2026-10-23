# strata-publish implementation progress

Branch `impl/publish` (worktree `git-strata-wt/publish`), from `impl/v1` 95f7660.

## Tasks (publish.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 chain ref and anchor advance | done | (this commit) |
| 2 path-level stage and merge | todo | |
| 3 read-only checks | todo | |
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
