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
