# strata-session implementation progress

Branch `impl/session` (worktree `git-strata-wt/session`), from `impl/v1` d40218a.

## Tasks (session.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 refuse invalid new requests before writing | done | d19b845 |
| 2 full sibling layout on per-session shadow repos | done | 03a4bce |
| 3 resolve by cwd or selector, status | done | (this commit) |
| 4 Unknown scenes and restart leftovers | todo | |
| 5 move base and deps with an atomic directory swap | todo | |
| 6 remove behind discard confirmation and open-file checks | todo | |
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
