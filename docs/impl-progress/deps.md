# strata-deps implementation progress

Spec: docs/modules/deps.md. Branch `impl/deps` (worktree git-strata-wt/deps), based on impl/v1 b70b04e.

## Tasks (§8)

| Task | Commit message | Status |
|---|---|---|
| 1 | feat(deps): install-input hash and deps set id | done |
| 2 | feat(deps): open-file scan over fds, mappings and cwd | done |
| 3 | feat(deps): sealed generations, build lock and cold install | |
| 4 | feat(deps): adopt the main checkout's deps dirs as the first generation | |
| 5 | feat(deps): ensure a base's deps set by reuse, adoption or cold install | |
| 6 | feat(deps): drift as an install-input hash comparison | |
| 7 | feat(deps): deps change proposals and main tracking | |
| 8 | feat(deps): approval targets and rollout plans with open-file skips | |

## Changes outside crates/strata-deps

- Workspace `Cargo.toml`: added `crates/strata-deps` to members.
- `strata-types` `RepoConfig::install_argv()` (additive, public): config.md §2.4's
  splitter was private to strata-types; `DepsConfigKey::of`, `toolchain` and the
  cold install need the split argv. It runs the same `check_argv` + `split_command`
  as validation and maps a split failure to `InvalidInput`.

## Decisions where the spec is open (safest reading)

### Task 1
- `inputs_hash` on a root that is not a directory returns `NotDirectory`; a missing
  root returns the classified io error (never "all Absent"), so a vanished workspace
  cannot look like "no inputs".
- `NotFile` for special files carries `detail.path` (repo-relative) plus
  `detail.reason` ∈ {fifo, socket, device}.
- Ignore filtering applies to members (files, symlinks) only; directories are always
  descended, because a tracked file under an ignored directory is never ignored
  (gitio rule) and must count.
- Whether the root is a git worktree is decided by the presence of `<root>/.git`
  (file or dir); without it (exports) nothing is filtered.
- Manifest canonicalization (§4.3) is hand-rolled: recursive sorted keys by UTF-8
  bytes, compact output, independent of serde_json's `preserve_order`.
- `toolchain`: the program is resolved along `env.path` only (first executable
  regular file); cache key is (realpath, inode, size, mtime s+ns), so replacing the
  program by rename invalidates it. `--version` runs with the install environment
  (GIT_* removed, PATH = env.path, stdin /dev/null), 10 s limit. Errors are
  `Unknown` with `detail.reason` = version_failed | version_timeout | version_empty
  (empty stdout and stderr is treated as a failure rather than producing "bun ").
- The fixed empty constants are pinned in tests: empty inputs hash
  `71ce361f…138b`; `set_id(empty, "", "", empty config)` = `ee45535b…21b3`.

### Task 2
- Mapped regions use `PROC_PIDREGIONPATHINFO2` (22, vnode-backed regions only)
  instead of `PROC_PIDREGIONPATHINFO` (8): identical result for files, ~0.3 s
  instead of ~2.4 s for a full scan on this Mac (§9 budget < 1 s). A non-positive
  or short return ends the region walk.
- libproc structs missing from libc (`proc_fileinfo`, `vnode_fdinfowithpath`,
  `proc_regioninfo`, `proc_regionwithpathinfo`) are defined locally `#[repr(C)]`
  with size asserts.
- Per-process errors: ESRCH / EPERM skip the process; EBADF / ENOENT on one fd
  (closed since the listing) skip that fd; anything else is `Unknown` with
  `detail {reason: open_files_scan, call, pid, errno}`.
- `open(dir)` errors other than ENOENT / ENOTDIR / ELOOP are `Unknown` for that
  directory (never an empty list).
- `command` falls back to the basename of `proc_pidpath`, then `pid <n>`, so it is
  never empty.
- Batch form `scan(&[&Path]) -> Result<Vec<Result<Vec<OpenFile>>>>`: one pass over
  processes; per-directory errors (injected or unusable dir) stay per directory.
- `fail_open_files_under(prefix)` matches a scanned dir equal to / below `prefix`
  by path components, lexically or after canonicalizing both.
- Test seams live in `src/seams.rs`; hooks are no-ops without the feature.

### Task 3
- Layout: all deps-store writes go through `strata_apfs` rooted calls under
  `paths.deps()`; helpers live in `src/layout.rs`, lock/seal/attempt plumbing in
  `src/seal.rs`, the install pipeline in `src/build.rs`, recovery listing in
  `src/attempts.rs`. `inputs.rs` now has one shared resolver
  (`resolve_inputs`) used by both `inputs_hash` and the members placed into
  staging, so the hashed set and the placed set cannot diverge.
- Build lock `Busy` detail: `{reason: "build_lock_busy", lock, waited}` with
  `waited` in milliseconds. Polling is 50 ms doubling to 1 s, bounded by
  `env.timeout`.
- `.locks` (or `<repo>`) existing as a non-directory → `NotDirectory` with
  `detail.path`; symlink / FIFO lock → `NotFile` (no follow, never blocks).
- `key_of` / `build` with empty `deps_dirs` use the fixed empty-set key and
  `seal_empty` (implemented in `seal.rs`, reused by `ensure` in Task 5).
- `deps_dirs` non-empty but `install` unset/empty → `InvalidInput
  {field: "install", rule: "required"}` before any write.
- Failures after install that are not already `Unknown` with a reason are wrapped
  as `Unknown {reason: "staging_failed", attempt, staging, error}`. Every
  `Unknown` that ends an attempt as failed carries `attempt` and `staging` (added
  when missing), so the kept scene is always named. `clone_into` failing after
  it has created something → `Unknown {reason: "clone_failed"}`.
- Seal: any non-`Exists` rename error, and any failure after the rename
  (chmod 0555, full fsync, record) → `Unknown seal_failed`; `Exists` → `orphan`.
  The seal-time sync count (`test_seams::sync_counts`) counts files and dirs
  below the seal root (the root itself is excluded) plus one full fsync.
- Repo write lock `Busy` while sealing: adoption refuses; a cold build retries
  every 10 ms with no bound (the install already ran; giving up would waste it).
- A refusal whose staging cannot be retired ends the attempt `failed` (scene
  kept) instead of `refused`, so it stays visible to recovery.
- Tests: the FIFO-lock case runs the build on a thread with a 1 s deadline
  (the store is shared through `Arc`); killed builders and foreign lock holders
  are re-execs of the test binary in their own process group (`worker` test).
  The `CrossVolume` preflight needs a disk image (`hdiutil`) and is a separate
  `#[ignore]` test (`build_preflight_cross_volume`).

### Task 4 — adopt (§5.4)

- `src/adopt.rs`, `tests/adopt.rs` (5 spec tests + worker helper). Test helpers added to `tests/common`: `make_socket` (a re-exec'd worker binds the socket by a relative name, since sockaddr paths are short), `mkfifo`, `apfs_walk`, `Fx::main_deps`.
- Decisions:
  - Every adopt failure after `begin` is a refusal (staging retired, attempt `refused`): the staging only holds a CoW clone of main, no unique data.
  - Step 9 `validate_and_sync` failures with reason `unreadable_file` / `special_file` / `symlink_escapes` are returned as `Drift` with the same reason and path; other validation errors pass through (still refused).
  - W2 walk errors refuse with that error, except `NoSuchPath` (a deps dir vanished mid-clone), which is `Drift changed_during_clone`.
  - W1/W2 compare (path, kind, size, gen count, mtime, link target) per entry, including each deps dir root; an unknown gen count is "different". The first differing path in byte order is reported.
  - A deps dir that is not a directory (file, socket, FIFO, symlink) or sits under a symlinked parent is `NotDirectory` before the lock; checks use `attrs_of`, which never opens (FIFO cannot block).
  - Escaping symlinks/special files found by W1 are `Drift` before any attempt is recorded.
  - Nested seed caches are skipped in both walks and discarded from `seal/` after the clone; main's cache is never touched.

### Task 5 — ensure (§5.6)

- `src/ensure.rs`, `tests/ensure.rs` (5 spec tests + worker helper). The empty-set path (`seal_empty`) was already written in Task 3 and is reused unchanged.
- Decisions:
  - `ensure` runs the full preflight (`inputs_tracked`, `key_of`) itself before the hot-path `find_set`, so a missing input is `InvalidInput` even when a set happens to be sealed; `adopt`/`build` repeat it (cheap, no writes).
  - Adoption retries only on `Drift changed_during_clone`, 3 adoptions in total; any other `Drift`, `NoSuchPath` or `NotDirectory` goes straight to one cold install; every other code is returned as-is (e.g. `Busy` from the repo write lock at adoption's seal, `CrossVolume`, `InvalidInput`).
  - "No command runs" for the empty set is tested with a `bun` that leaves a marker on any invocation (including `--version`).

### Task 6 — drift (§5.8)

- `src/drift.rs` (`Drift`, `drift`), `tests/drift.rs` (2 spec tests). `drift` only compares `inputs_hash(ws)` with `pinned.inputs_hash`; toolchain/platform/config never count.
- Test infra: `Sandbox` now has a `Drop` that makes every directory 0755 before the `TempDir` is removed. Sealed sets/trash entries are 0555, so earlier test runs leaked their sandboxes into `$TMPDIR` (76 leftovers removed). Tests are now run with `TMPDIR=/Volumes/GWD/opensource/.strata-tmp` (internal disk nearly full); no test depends on the internal volume (`/tmp/elsewhere` in adopt tests is only a symlink target string, never created; `build.rs` uses `std::env::temp_dir()`).

### Task 7 — proposals and main tracking (§5.9)

- `src/proposal.rs` (`Proposal`, `ProposalOrigin`, `ProposalState`, `ProposeArgs`, `PublishChain`, `ChainState`, `get_proposal`, `open_proposals`, `propose`, `publish_chain`, `main_tip`, `observe_main`), `tests/proposals.rs` (11 spec tests).
- Decisions:
  - `propose` looks up slot occupants with `proposals(repo, open_only = false)` filtered to `Pending`/`AwaitingPublish`, not `open_only = true`: a draft of a removed (or missing) session still occupies the store's unique slot, so using the filtered list would make every CAS miss and end in a spurious `proposal_race`. `open_proposals` (and `observe_main`'s current-proposal lookup) keep the spec's open-and-live filter.
  - `propose` retries its round 3 times, then `Busy {reason: "proposal_race"}`; `observe_main` makes 4 `set_deps_main` CAS attempts (one + 3 retries), then `Busy main_race`.
  - Landing a draft whose re-key/promote fails → `Unknown {reason: "landing_not_promoted", detail: {proposal: <full Proposal JSON of the recorded draft>, error}}`; a `promote_proposal` returning `false` is reported there as `Busy proposal_race`.
  - In `observe_main`, any error after a proposal was filed is wrapped once as `Unknown main_not_recorded {proposal, error}` (never double-wrapped); before that, errors pass through.
  - Returned `Proposal`s are built from the arguments + store ids/timestamps, not re-read, so a concurrent decision cannot change what the caller sees.
  - Validation reasons: `from_equals_to`, `tree_head_mismatch`, `no_target`, `tree_mismatch` (all `InvalidInput`); `tree_of` errors (e.g. `NoSuchCommit`) pass through.
- Tests: "fetch" is the real gitio fetch from a sandbox bare origin; publish/anchor refs are set with `git update-ref`; chain graphs use `git commit-tree` children. "Not yet fetched" is a fresh bare origin with no fetch. The other-repo case uses a second fixture/store, with main's Pending written via `record_proposal` directly.

### Task 8 — approval targets and rollout plans (§5.10)

- `src/rollout.rs` (`Target`, `RolloutPlan`, `RolloutStep`, `SkipReason`, `ensure_target`, `plan_rollout`, `plan_switch`, `record_rollout`), `tests/rollout.rs` (13 spec tests + `rollout_holder` worker that holds a file open via fd, mmap or cwd in a child process). `proposal::built` became `pub(crate)` so `ensure_target` can return the replacement `Proposal`.
- Decisions:
  - `ensure_target`: if the sealed id differs from `P.to`, the replacement is filed inside `with_write` via `record_proposal(replaces = P)`; if that returns `None` (P was decided concurrently), P is re-read and the result is `NoSuchProposal {proposal, state}` with its current state. `Busy` from the lock is returned as-is.
  - `plan_rollout` checks `P.repo` and every session's `base.repo` against `ctx.repo` (`InvalidInput {reason: wrong_repo}`) and then that P.head is still main's tip (`HeadMoved {reason: main_moved}`).
  - Skip reasons are evaluated in the spec order: Untracked → AlreadyThere → NotReady → OwnChange → Drift → OpenFiles. OwnChange compares inputs hashes (not set ids) against both the base's and the target's inputs. A missing pinned set is `Skip Error` with `Code::Corrupt`; `inputs_hash`/pinned lookup errors are `Skip Error` rather than failing the whole plan.
  - The open-file scan is one batch over all remaining candidates (one `lsof`-style pass); per-session file lists are sorted and deduplicated. A scan error skips every candidate with `Skip Error`.
  - `plan_switch` checks origin first (`not_origin`), then state (`not_open`); for Pending the target is (P.tree, P.to) with head P.head and the main-tip check; for AwaitingPublish the target is (session base tree, P.to). Its plans are always `complete = false`, so `record_rollout` never decides a proposal from a switch.
  - `record_rollout` marks P Applied only when the plan is complete, every `Move` session is in `moved`, and every Skip is Untracked or AlreadyThere; otherwise (or when `decide_proposal` returns false) it just re-reads and returns P.
