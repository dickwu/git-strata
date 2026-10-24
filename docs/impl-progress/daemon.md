# strata-daemon implementation progress

Branch `impl/daemon` (worktree `git-strata-wt/daemon`), from `impl/v1` 4e20827.
Not merged.

## Tasks (daemon.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 RPC envelope and method table | done | 36b5f0e |
| 2 Repo lock, session queue, Busy before write | done | 7dfaf8e |
| 3 Single instance, recover, then listen | done | (this commit) |

## Ambiguity decisions

- **Envelope strictness.** Unknown top-level envelope fields (anything but
  `id`, `method`, `params`) are RPC `Failed`, like unknown params fields
  (the `no_auth_*` tests: no scope/token/observed marker). Fields typed
  `T | null` in §4 must be present (a missing one is `Failed`); only
  `daemon_status.verify` has a default (`false`), as §4 says.
- **`resolve_move` with both or neither choice** is refused while parsing
  params (RPC `Failed`), so `resolve_failed_move` is never reached.
- **`TooLarge` reply id** is `null`: the oversized line is never parsed.
  The reader drains the line to its newline and the connection continues.
- **Session-typed params** (`checkpoint.session`, `files.session`, …) are
  `SessionId`s (validated by serde, `s-` + 8 base32); `status.session` stays
  a selector string (`Sessions::select`) because §4 types it as a string
  the CLI passes through from the user.
- **Lock wait in tests.** `LOCK_WAIT` (30 s) is the production bound;
  `Locks::new(repos, wait)` takes the bound so tests use 300 ms. The
  `LongHold` warning threshold is the same bound.
- **Which take is bounded.** Per request (thread-local `RequestScope`), only
  the first lock take before `mark_visible()` is bounded and can be `Busy`
  (detail `{reason:"lock_wait", lock, holder, holder_session}`); every later
  take, and any take after a visible write, waits FIFO. A take outside any
  request scope is bounded. A reentrant take of a lock the same request
  already holds is `Busy` `{reason:"reentrant"}` rather than a deadlock.
- **Shutdown** fails queued waiters with `ReadOnly` `{reason:"shutting_down"}`
  (no dedicated code in types.md); the current holder runs to completion.

- **`Daemon<'a>` borrows a `Core`.** `Sessions` borrows config, gits,
  store, locks, env and the BeforeCheckpoint, so startup is `prepare` (steps
  2–7, owned `Core`) then `Daemon::start(&core)` (steps 8–11). The spec's
  `Daemon` has no lifetime; this split is the only way to hold `Sessions`
  once without self-reference.
- **TEST_SEAMS in tests.** Cargo feature unification turns `test-seams` on
  for the lib in every test build, so the `git-stratad` binary the tests can
  run is always a seams build. `release_daemon_has_no_test_seams` runs it and
  asserts it refuses (exit ≠ 0, one JSON error line, reason `test_seams`,
  nothing created under HOME). All other daemon tests run the server
  in-process (`prepare` / `Daemon::start` / `bind` / `serve`).
- **Second instance** is exercised in-process: `flock` locks are per open
  file description, so a second `open` + `flock(LOCK_NB)` in the same process
  is refused exactly like another process. `FD_CLOEXEC` is asserted.
- **Socket path in tests** is a short temp dir under `/tmp` (`sd-XXXX/d.sock`)
  because `sockaddr_un` holds 104 bytes; the config dir, store root and repos
  live in the test's temp dir. Production is `<config dir>/daemon.sock`.
- **`Options.install_path`** (production `DEFAULT_PATH`) is a seam so tests
  install with the fake `bun`; it feeds `InstallEnv.path`.
- **Step 4 realpath.** Config validation (V1: name = last component) already
  refuses most aliases; the daemon then refuses a path that differs from its
  realpath with `InvalidInput` `{reason:"not_realpath", realpath}`.
  `paths.root` must be its realpath when it exists (Store creates it).
- **gc failure** at startup becomes `VerifyFailed{message:"gc <repo>: …"}`
  (no dedicated warning variant in §4).
- **Interim handlers.** In Task 3 only `daemon_status` is served; every other
  method replies RPC `Failed` "not served yet" until Task 6 wires them.
- **Quarantine scan** reads `sessions/.repos/*/*.git` with lstat only:
  every entry of `strata-quarantine/`, a non-directory `worktrees`, and a
  non-directory `worktrees/<n>`. Each warning is recorded and alerted once
  per daemon run.
- **T2 test fix.** `no_nonce_record_limit` matched `FnOnce` in locks.rs (it
  lowercases to "fnonce"); the scan now strips `FnOnce` first. Fixed in the
  Task 3 commit (no amend).

## Task 1

`rpc.rs`: the 19-method table (`Method`, with `is_read_only` for I5),
typed `deny_unknown_fields` params, `parse_request`, `Reply::{ok, error,
rejected, too_large}`, `read_frame(_limited)` with `MAX_FRAME`,
`serve_connection` (the per-connection loop the server uses) and
`ready` (readiness = the socket accepts a connection). The `no_*` tests
assert shapes (reply keys, refused envelope extras, line framing over a
real unix socket in a short `/tmp` dir) plus source scans of the crate for
the banned transports (TCP, encryption, nonces, container hosts).

## Task 2

`locks.rs`: ticket-FIFO `Fifo` (mutex + condvar, poison cleared), one per
repo (write lock) and one per session (queue, created on demand), `Locks`
with `with_repo` / `with_sessions` (ascending session order, places the
request already holds are skipped), RAII release on return, `Err` and panic,
`holds()` / `long_holds()` / `in_flight()` / `queued()` for `daemon_status`
and `status` (read-only paths never take a lock), `shutdown()`, and the
`strata_deps::RepoWriteLock` / `strata_publish::PublishLocks` impls.
tests/locks.rs (14) drive the locks with stand-in request threads; the
handler-level versions (real checkpoint/rm/publish/deps_approve through
the RPC) are in Task 6's tests/daemon.rs.

## Task 3

`startup.rs`: `refuse_test_seams` (step 1), `InstanceLock` (flock,
`O_CLOEXEC`, `Exists` `daemon_running`), `prepare` (lock → config parse +
validate → realpaths → `Store::open` → `Git::open` per repo with RepoId
collision → `NameTaken` `repo_id`), `Daemon::start` (`Sessions::open` once →
recover → gc per repo under its write lock with `GC_KEEP` → Running→Unknown
and an `UnknownPublish` warning per Unknown row, never a push or replay →
quarantine scan → poisoned store), `bind` (dir 0700, stale socket removed
under the flock, socket 0600), `serve` (accept loop; on stop: locks
shutdown, idle connections closed for read, in-flight requests finish),
`Core::close` (`Store::close`, unlink socket, release flock), and
`daemon_status`. `main.rs`: seams refusal, HOME layout, SIGTERM/SIGINT set
an atomic, one JSON error line on failure. Pins (step 5) come in Task 4.
tests/startup.rs (10): the seven §8 tests plus the non-directory worktrees
scan, config/realpath refusals and RepoId collision (a linked worktree).
