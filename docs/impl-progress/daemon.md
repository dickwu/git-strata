# strata-daemon implementation progress

Branch `impl/daemon` (worktree `git-strata-wt/daemon`), from `impl/v1` 4e20827.
Not merged.

## Tasks (daemon.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 RPC envelope and method table | done | 36b5f0e |
| 2 Repo lock, session queue, Busy before write | done | (this commit) |

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
