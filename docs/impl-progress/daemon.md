# strata-daemon implementation progress

Branch `impl/daemon` (worktree `git-strata-wt/daemon`), from `impl/v1` 4e20827.
Not merged.

## Tasks (daemon.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 RPC envelope and method table | done | (this commit) |

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

## Task 1

`rpc.rs`: the 19-method table (`Method`, with `is_read_only` for I5),
typed `deny_unknown_fields` params, `parse_request`, `Reply::{ok, error,
rejected, too_large}`, `read_frame(_limited)` with `MAX_FRAME`,
`serve_connection` (the per-connection loop the server uses) and
`ready` (readiness = the socket accepts a connection). The `no_*` tests
assert shapes (reply keys, refused envelope extras, line framing over a
real unix socket in a short `/tmp` dir) plus source scans of the crate for
the banned transports (TCP, encryption, nonces, container hosts).
