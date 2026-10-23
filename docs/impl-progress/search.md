# strata-search implementation progress

Branch `impl/search` (worktree `git-strata-wt/search`), from `impl/v1` 95f7660.

## Tasks (search.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 view table and typed refusals | done | (this commit) |
| 2 share base index by main checkout tree and profile | todo | |
| 3 session overlay shadows the shared base | todo | |
| 4 pin the query snapshot and refuse a replaced root | todo | |

## Decisions

- tgrep-core is vendored (design §5 "不可行则 vendor 所需模块"). Upstream
  `HybridIndex::from_reader` is `pub(crate)`, and §4.2 requires the view's
  reader to be `generation.base().reader()` itself, not a second `open`.
  `vendor/tgrep/` holds tgrep-core 1.2.4 and its path dependency
  `vendor/ignore`; the only source change is `from_reader` made `pub`
  (`vendor/tgrep/PATCHES.md`). The clone at `/Volumes/GWD/opensource/tgrep`
  is untouched. The root workspace has `exclude = ["vendor"]`. Because of
  this the Task 1 commit also adds `vendor/`, the root `Cargo.toml` and
  `Cargo.lock`, not only `crates/strata-search`.

## Ambiguity decisions

- Task 1: `Sessions::status` reports a non-directory at the workspace path as
  health `Missing`. §5.1 step 4 maps `Missing` to `NoSuchPath`, but §7
  `attach_missing_workspace_is_no_such_path` wants `NotDirectory` for a
  regular file. `attach` maps `Missing` to `NoSuchPath` only when nothing is
  at the path (lstat fails); otherwise step 7's `open_root` names it
  (`NotDirectory`).
- Task 1: a session in `Unknown` state answers `Unknown` with
  `reason = "session_unknown"` and the session scene; a workspace in
  `Unknown` health answers `Unknown` with `reason = "workspace_unknown"` and
  the workspace scene (§5.1 names the code and the scene, not a reason).
- Task 1 tests make `Creating` with `Store::create_session` of a second
  manifest (no `Sessions::new` stops in Creating without a seam), and
  workspace `Unknown` with a store move intent that failed while swapping.
