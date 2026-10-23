# strata-session implementation progress

Branch `impl/session` (worktree `git-strata-wt/session`), from `impl/v1` d40218a.

## Tasks (session.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 refuse invalid new requests before writing | done | (this commit) |
| 2 full sibling layout on per-session shadow repos | todo | |
| 3 resolve by cwd or selector, status | todo | |
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
