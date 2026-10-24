# strata-publish implementation progress

Branch `impl/publish` (worktree `git-strata-wt/publish`), from `impl/v1` 95f7660.

## Tasks (publish.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 chain ref and anchor advance | done | fd86995 |
| 2 path-level stage and merge | done | 8ad75bd |
| 3 read-only checks | done | 849e297 |
| 4 commit, push, and unknown | done | b11dd10 |
| 5 rebase, assign, and diff | done | (this commit) |
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
  with the original error under `detail.cause = {code: "CheckFailed",
  message, detail}` (§6; changed in Task 4 from `detail.check`). A failed
  `stage::build` cleanup likewise keeps its cause under `detail.cause`.
- The "cannot remove" test uses the apfs `before_open` seam to swap a
  directory for a symlink mid-removal (no chflags/root needed).

### Task 4 (commit, push, unknown)

- Signatures differ from §3 where the listed ones cannot work:
  `commit(git, branch, staged, parent, expected_chain, message)` (the chain
  ref needs the branch; the CAS needs the expected value) and
  `push(git, cfg, repo, staging, confirm)` (the confirm action names the
  repo). `push` order: confirm gate → chain not `Leads` → `NoCommit` → send.
  Standalone `push` returns only `Ok(Pushed)`; a refusal is
  `Err(PushRefused {detail: {kind, message}})`, an uncertain send
  `Err(Unknown)`. Inside `publish` (after the commit is written) both
  become `Unknown` (`detail.kind = "PushRefused"` / reason `push_unknown`).
- Confirm gate (`confirm_gate`): only when `cfg.needs_human_confirm()`; the
  token's action must equal `PushDeploy {repo, branch: publish.branch}`,
  else `NotConfirmed` (reason `missing` / `action_mismatch`). Without the
  need a passed token is ignored. Nothing reads an env var or a flag; the
  gate runs before `stage::build` (no directory, no git write).
- Busy rules (§5.2 step 2): an `Unknown` row of the repo blocks every
  publish of the repo (`Busy publish_unknown`, checked before recording
  and again under the repo lock). A `Running` row blocks only when it
  shares a session with this group in this repo (`Busy publish_open`): a
  repo-wide Running block would serialize Task 6's disjoint concurrent
  publishes, which the session queues and the repo lock already order.
- The release id is the publish row id (`group.id` is overwritten with it).
- Commit message: `strata publish <id> (<repo>)` + blank line +
  `sessions: …` and `paths: N`. Committer/author come from the repo's git
  config (gitio `commit_tree`).
- The chain move is one ref transaction: chain `expected → new`, and the
  anchor `→ B` only when the tracking ref equals B (B = origin's tip). A
  CAS failure (`HeadMoved`) is never retried: `Unknown`, reason
  `head_moved`, `detail.head_moved {ref, expected, actual}`, commit kept in
  `detail.commit` and in the row.
- Every error after `commit_tree` returns is wrapped as `Unknown`
  (`detail.cause` holds the original) and the row goes `Running → Unknown`
  with `outcome.error`; an error before that closes the row `Done` with
  `outcome.stopped = <code>`. A store failure while closing → `Unknown`.
- `PUSH_TIMEOUT` = 120 s: gitio exposes no push default, so this mirrors
  its fetch timeout. A timeout is `Unknown`; the chain stays.
- Layout removal failing after a successful push → `Err(Unknown)` with the
  commit in the detail; the row is `Unknown` (the success is not reported
  as `Done` while a scene is left).
- Test seams (`test-seams`): `seams::pause(git, PublishFault::…)`
  (`PauseBeforeCommit`, `PauseAfterCommit`, `PauseDuringPush`,
  `PauseBeforePushRecorded`; the last two wrap gitio's push pauses),
  `seams::fail_once(git, FailPoint::{BeforeRefs, BeforePush, BeforeDone})`
  (injected failures use `Code::Unknown` reason `injected`; there is no
  `Io` code) and `seams::set_push_timeout`. Keyed by the git common dir.
- The post-write section (after the queue is released) waits on the repo
  lock without a bound (retries `Busy` every 20 ms); in Task 4 it holds an
  empty body that Task 5 fills with the rebase. `observe_main` errors there
  become a `ObserveFailed` warning, not a failure.
- Tests: `tests/push.rs` (15). Remotes are bare repos under the sandbox;
  push refusal/timeout use a `pre-receive` hook in that bare repo. Two tests
  beyond §8's list: `running_row_of_another_session_does_not_block` and
  `publish_refuses_rewound_and_diverged_chain` (the end-to-end half of
  Task 1's refusal test).
- `pushed_sets_anchor_to_pushed_commit`'s crash case makes the gitio record
  transaction fail by moving the tracking ref while `PauseBeforePushRecorded`
  holds the call (the remote already accepted): `Unknown`, the anchor stays
  at the previous commit; after the bare origin is rewound and fetched the
  chain reads `Rewound`, and `advance_anchor` does not move it.

### Task 5 (rebase, assign, diff)

- Store change (Task 5 commit, authorized by publish.md §2 item 4): the
  `publishes_update_guard` trigger refused every same-phase update, so
  `set_publish_outcome` (added in Task 1, untested) could never write.
  It now also allows an `unknown`/`done` row to change only its `outcome`
  (phase and `ended_ns` unchanged, outcome actually different); `running`
  rows still cannot. Covered in store `guards.rs` and `records.rs`.
  store.md §4.3's trigger text is NOT updated (progress is recorded only
  here); store opens compare `sqlite_schema` row by row, so a store created
  before this commit reads as `Corrupt` (no deployed stores exist; no
  `user_version` bump).
- Post-publish rebase (`rebase::after_publish`) runs per batch session in
  queue order, after the queue is released and after one `observe_main`.
  Order per session: outside every lock, old base → `ensure_base_for` →
  deps key; if the key equals the old deps the workspace is re-targeted
  with those deps, otherwise `deps::ensure` (seal) runs best effort (a
  failure is a warning). Then under sessions+repo locks (`under_locks`,
  unbounded 20 ms retry on `Busy`): base changed meanwhile →
  `InvalidInput base_changed`; a landed draft is promoted (`propose
  Session`); same deps → `move_workspace`; otherwise the promoted proposal
  or `propose(Main)` → `Waiting`. A missing sealed set →
  `Corrupt deps_unsealed`.
- `ensure_base_for` can return a lock `Busy` (no `detail.reason`) right
  after the write; `base_for` retries that only. Other `Busy` passes up.
- When a draft is promoted, `propose(Main)` is not called again.
- `move_workspace` `OpenFiles` is a report (`OpenFiles {detail}`), not a
  failure: the publish stays `Done`.
- A rebase error (or `FailPoint::BeforeRebase`) stops the remaining
  rebases; the row goes `Unknown` with the commit and publish returns
  `Err(Unknown)` with `detail.commit`.
- `outcome.rebase` is a list of `{session, kind, ...}` (`RebaseReport`
  serialized, `proposal` replaced by its id); `outcome.paths` is the staged
  path list (used to clear assignments). `Published.rebases` carries the
  reports. A `Conflict` row also stores `outcome.conflicts`, and the
  `Conflict` error carries `detail.publish = <row id>` (what `assign`
  names).
- `assign` also checks the session exists (`NoSuchSession`), which the §4
  codes table does not list. It writes `{path, session, at}` (`at` = ms) to
  `outcome.assigned` with a phase CAS, 16 retries, then `Busy assign_race`.
- An assignment is cleared by a later `Done` row with a commit whose
  `outcome.paths` contains the path and whose `ended` ≥ `at`. The §5.7
  check in `stage::plan` refuses a batch selecting an assigned path from
  another session: `Conflict reason=assigned`, conflict kind `assigned`.
- `diff_between`: reconciles both overlays; with different bases the older
  side (ancestor by head) is projected with `overlay::rebase` onto the
  newer; neither an ancestor → `InvalidInput bases_unordered`. Previews
  follow §5.3 (modify/delete, add/add, link, mode → conflict entries;
  `TooLarge`; binary); merge3 hunks are capped, `start` = our start line.
- `rebase_session` (public) runs `observe_main` itself when main's tip is
  `head`; inside `publish` it is run once before the rebases.
- The Task 4 `with_repo_unbounded` helper was replaced by `under_locks`.
- Tests: `tests/rebase.rs` (13), the §8 list. `push.rs`'s
  `injected_failure_after_commit_is_unknown` now expects the rebase to have
  moved the base before `BeforeDone`.
