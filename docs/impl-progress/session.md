# strata-session implementation progress

Branch `impl/session` (worktree `git-strata-wt/session`), from `impl/v1` d40218a.

## Tasks (session.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 refuse invalid new requests before writing | done | d19b845 |
| 2 full sibling layout on per-session shadow repos | done | 03a4bce |
| 3 resolve by cwd or selector, status | done | 426dd58 |
| 4 Unknown scenes and restart leftovers | done | bb583d7 |
| 5 move base and deps with an atomic directory swap | done | b2385ec |
| 6 remove behind discard confirmation and open-file checks | done | (this commit) |
| 7 resolve a failed move by abandoning or accepting it | todo | |

## Ambiguity decisions

- Task 1: tests that need a disk image (`new_refuses_cross_volume_root`,
  `new_refuses_non_apfs_root`) are `#[ignore]`: `hdiutil attach` needs a
  human-approved environment. The refusals themselves are implemented
  (`same_volume` → CrossVolume, `fs_type != "apfs"` → Unsupported).
- Task 1: until Task 2 builds the layout, `new` stops after Phase B with
  Unsupported reason `not_implemented`; Task 1 tests assert "reached the
  build" as Ok-or-that placeholder. Task 2 tightens them to Ok and adds the
  `store_row` id clash (needs a registered base gen) and the gitlink half of
  the pass-through test (needs export).
- Task 1: step 8 (fetch + main tip + tree) runs for every configured repo,
  not only the requested ones, following "Phase B per repo" over the
  configured set; a missing origin/main in any configured repo refuses `new`.
- Task 1: the `common_dir` nesting form is exercised with a linked worktree
  as the configured path and root inside the shared common dir (the only
  construction where the checkout and root are apart but the RepoId is not).
  The `checkout` form through a symlinked parent puts the real checkout
  under `<root>/bases/`, because `Store::open` refuses foreign entries
  directly in root.
- Task 1: nesting compares realpaths of the nearest existing ancestor of
  root plus the not-yet-existing remainder; folding follows the volume's
  case sensitivity.
- Task 1: an lstat ENOTDIR on a seed source counts as missing (same as
  ENOENT); a missing explicit seed is not a refusal at validation time.
- Task 1: test seams: `StopAt` returns Unknown with `detail.seam =
  "session-seam-stop"` and callers return it without failure handling;
  seams are checked when a repo is claimed for dispatch, repos claimed in
  RepoName order; `phase@repo` matches only that repo.
- Task 1: the test lock panics on same-thread re-entry, so a nested lock
  acquisition is caught as a bug rather than a deadlock.
- Task 2: the base loop (§5.2) bounds itself: three deps-set misses in a
  row → Busy `deps_set_race`; staging retired under it twice → Busy
  `base_staging_retired`; for `ensure_base_for` (a given set) the first
  miss → InvalidInput `deps_unsealed`, never installing. A hot-path
  `list_dir` NoSuchPath on the tree directory counts as a miss.
- Task 2: after staging B is retired, named codes from the build pass
  through unchanged; only Unknown is wrapped with the base detail. If
  removing B fails after a successful register, B is left for leftover
  collection rather than failing the call.
- Task 2: staging names are 32 hex; in-flight staging is tracked by an RAII
  guard (`live_staging`) so the leftover sweep (Task 4) can skip it.
- Task 2: seed duplicates compare each entry against all earlier entries
  (case-folded on a case-insensitive volume), `detail.other` = the first.
- Task 2: a seed whose type changes between `lstat` and `attrs_of` is
  refused (explicit, reason `changed`) or skipped (candidate). A
  regular-file seed is also checked through `attrs_of` so a symlinked
  parent in main is caught for every type.
- Task 2: a seed cache whose ancestor in the base is a symlink or not a
  directory is skipped silently (D12, same rule as candidates).
- Task 2: `new` runs Phase B as a second pass over repos after all heads
  are resolved; observation after the layout uses its own 4-worker pool so
  one failed observe does not stop the others and becomes
  `NewWarning::ObserveFailed`. `new` adds `repo` to seed refusal details.
- Task 2: the §7 row "tracked `.ENV`, explicit `.env` → exists_in_base"
  only holds with `core.ignorecase = false`: gitio's `tree_contains`
  follows core.ignorecase (git identity), which git init sets true on this
  volume, so with the default config step 2 refuses first with `tracked`.
  The test asserts both: `tracked` under ignorecase=true and
  `exists_in_base` under ignorecase=false; `.ENV` is untouched either way.
- Task 2: `new_seed_writes_never_leave_the_session_root` puts the main
  checkout one commit behind origin (real, ignored `conf/` and `.claude/`
  directories) while the base tree tracks them as symlinks to O. With the
  symlinks also in main, §5.3 step 1 (`NotDirectory symlink` for a
  symlinked parent in main) would fire before the destination check the
  row names (`parent_not_directory`); this keeps the spec's step order and
  still exercises the destination rule.
- Task 2: the parts of the §7 T2 rows that need `status` (Task 3) or the
  leftover sweep (Task 4) are added with those tasks. Case-sensitive
  volume variants need an `hdiutil` image and are omitted here.
- Task 2: tests reopening the Store retry Busy for up to 5 s: children
  forked by other test threads briefly share the LOCK description between
  fork and exec.
- Task 3: status runs the filesystem probes (drift, seed checks, mirror
  drift) only for a workspace whose health is Ok; "the workspace exists"
  means its directory, its `.git` and its shadow repo are all there, so a
  missing `.git` or shadow is `Missing` and status itself never fails on
  it. An Unknown-health workspace skips these probes but still reports its
  checkpoint, pending proposals, conflicts, seeds and RestoreScene.
- Task 3: `shadow_at` errors other than NoSuchPath, and seed-source lstat
  errors other than ENOENT/ENOTDIR, pass through as status errors rather
  than being folded into a warning.
- Task 3: `select` turns every ticket validation failure into
  InvalidInput, including the id-like reserved form (`S-ABCDEFGH`) that
  `new` refuses as ReservedName. Ticket matches and `list` sort by
  creation time, ties broken by session id.
- Task 3: a stored scene that does not parse, or a workspace row for a
  repo outside the manifest, is Corrupt.
- Task 3: `resolve` returns None when `<root>/sessions` cannot be opened
  (no session can contain cwd); only store errors are Err. The
  "store unreadable → Err" row is driven by reopening the store (empty
  reader pool) and chmod 000 on `store.db`; the store has no read-fault
  seam.
- Task 3: the T2 row `new_reports_a_failed_observation_as_a_warning` now
  checks that status is unaffected and that a second `observe_main` fills
  the missing record. The `new_failure_after_writing_began_is_unknown`
  status/resolve half comes with Task 4 (Unknown scenes).
- Task 4: `Scene`/`SceneOp` moved from status.rs to scene.rs (re-exported
  unchanged). `detail.cause` is the lower StrataError as is (code,
  message, detail); when its detail carries an `errno` (an apfs-classified
  I/O error, including the seam's injected one), that errno is also lifted
  to `cause.errno`, so both §6.3 readings ("the StrataError" and "errno
  plus the original text") hold.
- Task 4: `new`'s Unknown message omits `/<repo>` (follows the §6.3 `new`
  example); the repo is in `detail.repo`/`detail.path`. `[/<repo>]` is
  left for single-repo operations (move, resolve). For phases with no repo
  (`root`, `finalize`, or `set_session_phase` itself failing) the detail
  has no `repo`/`path`.
- Task 4: `kept` = the session root and each `sessions/.repos/<id>/<repo>.git`
  that exists at the time (lstat); the bare `sessions/.repos/<id>/`
  container is not listed (row 1136 expects exactly the session root when
  no shadow exists yet). An empty list prints as `kept nothing`.
- Task 4: a `set_lifecycle` failure (or `false`) while recording a `new`
  scene is ignored; `new` still returns the Unknown and `recover` marks
  the Creating row next start (§5.1 last paragraph).
- Task 4: `recover` step 1 scene for an interrupted op: `error` is Unknown
  built from the same §6.3 template, `detail.reason = "interrupted"`,
  `detail.cause = {reason: "interrupted", message}` (there is no lower
  error); repo is None. A row with no recorded phase gets the op's first
  phase (`root` for new, `unpin` for remove). A `set_lifecycle` that
  returns false (row moved meanwhile) is a Busy `lifecycle_changed` item in
  `errors`.
- Task 4: `RecoverReport.retired` lists the staging paths as they were
  before retirement (not the trash names). `recover` and `leftovers` both
  skip entries in this process's `live_staging` set (empty at daemon
  start, so recover still retires everything then). `leftovers` lists
  export temps only inside directory entries; a missing `.staging` is no
  leftovers. `IntentRef` and the move fields of `RecoverReport` are defined
  now and filled by Task 5 (recover step 2).
- Task 4: `SessionSeam::release` now clears `paused` itself, so a
  `wait_paused` right after `release` waits for the next pause instead of
  returning on the old one (needed by the "staging taken twice" case).
- Task 4: base.rs already had the §5.2 failure handling from Task 2
  (retire B, then named code or Unknown with kept empty); Task 4 adds the
  tests for it rather than new code there.
- Task 5: `changed` for a kept old tree (status OldTreeKept, step 19/20)
  compares `reconcile_full` of the old tree against the latest checkpoint
  taken at or before the intent's `started` (the BeforeRollout one); a
  reconcile error counts as changed (never collect what can't be proven
  unchanged). `old_tree = Some(S)` whenever the tree stays, even when not
  recorded with `set_old_tree`.
- Task 5: the open-files recheck before the swap covers D in the
  workspace and in S. A prepare-time Unknown's `kept` always lists S, plus
  `worktrees/<n>` when it exists. A named prepare failure whose closing
  (collect + Refused) fails still returns Unknown with `detail.refused`;
  a successful close returns the original named code with
  `detail.quarantined` when something was set aside. A failed quarantine
  rename is Unknown `quarantine_failed`.
- Task 5: `Exists` from `attach_worktree` still retires `worktrees/<n>`
  (it is this intent's own number, nothing else can own it). Step 20's
  admin generation for an earlier Done intent is derived from the intents
  (the largest earlier Done n, or 1 for the tree `new` attached).
- Task 5: recover step 2 collects a Preparing intent's staging only for an
  Active session; other lifecycles get the state change only. status's
  `open_files` errors on a kept old tree propagate.
- Task 5: carry compares names case-folded when the workspace index has
  `core.ignorecase`. A carry rename failure (the `uchg` case) reports
  `detail.path` as the carried repo path, with the temp name in
  `detail.temp`.
- Task 5: the "shadow worktrees replaced" recover case swaps
  `worktrees/` while the move is paused at `move.carry` (after the
  workspace's own index was read): replaced before the move, the
  workspace's gitfile dangles and gitio refuses earlier with InvalidInput
  `foreign-worktree`, never reaching attach. The test re-creates
  `.git/worktrees/keep` in main after publishing the target, since the
  fixture's git commit/push may prune it.
- Task 5: the §7 carry row is split into three tests (content, races,
  `uchg`). The parts of §7 T5 rows that need `remove` or
  `resolve_failed_move` are asserted in Tasks 6 and 7.
- Task 6: the Dirty check (step 4) runs `reconcile_full` only for a
  tracked workspace that is healthy while the session is Active; an
  Unknown session is already `session_unknown` and gets no overlay scan.
  "Missing" is computed as in status (directory, `.git`, shadow), and a
  manifest repo with no workspace row counts as missing. Old-tree
  `changed` uses the Task 5 rule; a compare error counts as changed.
- Task 6: Dirty `detail.repos.<repo>` is `{paths (first 100), count,
  truncated}`; OpenFiles `detail` also carries `paths` (the two scanned
  directories). Untracked repos always need discard, even when unchanged.
- Task 6: step 5.1 takes the bounded per-repo lock per pending intent
  (Busy passes through, before anything visible). For an Unknown session
  a Preparing intent is only closed as Abandoned; its staging leaves with
  the root in step 8. An Abandoned intent whose staging still exists is
  collected again (Active only).
- Task 6: BeforeRemove checkpoints are taken only for tracked, healthy
  workspaces of an Active session; `RemoveReport.checkpoints` lists them.
- Task 6: a `false` from step 9's Removing→Removed is Corrupt and goes
  through the Unknown path like any other step 7–9 failure; a StopAt seam
  returns as is (the row stays Removing for `recover`).
- Task 6: a Swapping intent (move stopped between swap and record) is
  `move_awaiting_recover` in Dirty `unresolved`; the test does not remove
  it with discard (recover owns it).
