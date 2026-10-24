# strata-daemon implementation progress

Branch `impl/daemon` (worktree `git-strata-wt/daemon`), from `impl/v1` 4e20827.
Not merged.

## Tasks (daemon.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 RPC envelope and method table | done | 36b5f0e |
| 2 Repo lock, session queue, Busy before write | done | 7dfaf8e |
| 3 Single instance, recover, then listen | done | f0c112f |
| 4 Confirm order, pinned flags, launchd plist | done | d236ef6 |
| 5 Anchor after fetch, observe warning, attach alert | done | b3bd021 |
| 6 End to end, read-only without the daemon | done | bf56dd0 |
| 7 `resolve_move` and `chain_resolve` | done | (this commit) |

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

- **launchd never loaded by tests.** `plist::install_agent_with(bin, home,
  &dyn Launchd)` takes the launchctl seam; tests pass a temp HOME and a
  recording fake, and lint the written plist with `plutil -convert json`
  (read-only). The production `install_agent(bin)` (HOME + real
  `/bin/launchctl bootstrap gui/<uid>`) is never called by a test.
- **Plist HOME.** The plist follows the §3 sample exactly (no `HOME` in
  `EnvironmentVariables`; launchd supplies HOME to a user agent); "HOME
  written absolute" is read as the absolute `StandardErrorPath`.
- **Effective flags.** At startup the loaded `Config` gets the pinned
  flags applied (stricter-or-equal to the file), so everything that borrows
  the config sees the effective values. After a runtime `config_relax` the
  borrowed config is stale-strict; handlers (Task 6) read `Core::pinned`.
  Config is only read at startup ("reload" = restart).
- **Pins edge cases.** A repo new to `pins.json` is pinned as configured
  (not a relaxation); pins of repos no longer configured are kept; an
  unreadable `pins.json` is `InvalidInput` `{reason:"pins"}` and the daemon
  does not start (safest: never silently reset pins).
- **V5 interplay.** `deploy_on_push` requires `publish.confirm`, so the
  "deploy_on_push true → false" relaxation tests keep `confirm = true`.
- **Confirm order seam.** `require_validated_with(confirmer, action,
  between)` / `Core::config_relax_with(repo, between)` run `between` after
  `require` and before `validate`; tests use it to see that pins.json is
  unchanged until `validate` and to expire the token (FakeClock) so
  `validate` fails.

- **Where the fetch happens.** The fetch is inside `Sessions::new` (session
  owns it); the daemon advances the anchor after `new` returns, under its
  own `with_repo` take (§5.7 "outside publish").
- **Own anchor step.** `fetch_after::advance_anchor_locked` reimplements the
  publish-side rule (classify → `None`/`Absent`/`Rewound` = no call; else
  `m = merge_base(origin, chain)`, write only if `is_ancestor(a, m)` and
  `m != a`, one `ref_transaction` verifying the chain and CAS-ing the
  anchor) instead of calling the publish helper, so a failed transaction
  maps to `Unknown` `{reason:"anchor_transaction", cause}`.
- **No retry once called.** "不再试一次" wins over the AnchorNotAdvanced
  wording that mentions two attempts: the transaction runs once;
  `AnchorNotAdvanced` is recorded on that `Unknown` and cleared by the next
  successful advance.
- **Observe retry is injectable.** `Core::after_new(report, observe)` takes
  the observer; the handler passes `Core::observe_new`. Retry happens once,
  only for `NewWarning::ObserveFailed`; success drops the warning, failure
  replaces its detail. The publish-side ObserveFailed (stays in
  `Published.warnings`) is covered by Task 6's handler tests.
- **Gitio tracking ref.** The anchor test fetches through gitio
  (`refs/strata/remotes/origin/main`), from a local bare origin only.
- **track.rs** is an extra file beyond §8's list: the only place that calls
  `hint` / `require_full` / `flush` on the overlay tracker (on the request
  thread, never in the FSEvents callback, which only does `inbox.note`).
- **shadow_dir** duplicates session's layout (`sessions/.repos/<id>/<repo>.git`)
  because session's helper is `pub(crate)`.
- **Attach alert** is tested with synthetic errors in the real gitio/session
  shapes (`index-read-back-refused`, `staging-changed`, direct or as
  `cause.detail.reason`).
- **PrePushGuardDisabled** is only reported: one stderr line per shadow dir
  per daemon run (`Warnings::alert_first`), no hook or config write.

- **Handlers live in `handlers.rs`** (extra file). `handlers::serve(d,
  listener, external)` wraps `Daemon::serve` with a `Server` that owns the
  per-(session, repo) `Trackers` and the `Search`; `Daemon::handle` stays
  the status-only handler the Task 3 tests use. `git-stratad` serves
  `handlers::serve`.
- **Unknown on panic.** A handler panic is `Unknown` when the request may
  have written (a thread-local `WRITING` flag set before any lower-layer
  call that can write, or after `mark_visible`), else RPC `Failed`.
  `WRITING` is separate from `mark_visible` so lower layers' own lock takes
  stay bounded (`Busy`) as Task 2 decided.
- **ToolCall with no paths** prepares a full scan whatever the reason
  (correctness over speed); a tracker whose watch cannot start is
  require_full on every take (`track::watch_or_full`, keeping every inbox
  call in track.rs).
- **Search views** are attached and detached per request (no long-lived
  views). `grep --all` puts attach failures in `skipped`.
- **Push staging** is `<paths.root>/daemon/push/<pid>-<nanos>/` (export +
  attach), removed after `publish::push`. The chain top is read before the
  prompt and rechecked under the repo lock (moved → `NotConfirmed`
  `chain_moved`).
- **Multi-repo publish** runs repos in name order and stops at the first
  error; the error detail carries `published_before`. An `Unknown` result
  adds an `UnknownPublish` warning per Unknown open row (never retried).
- **Not wired (deviation):** `deps_propose` / `deps_approve` reply RPC
  `Failed` "not served yet" (the deps install-and-propose flow through the
  daemon is more than Task 6's minimal implementation; deps' own API is
  tested in its crate). `resolve_move` / `chain_resolve` come in Task 7.
- **Handler-level lock tests.** Task 2 said the RPC-level versions of the
  lock tests would be in tests/daemon.rs; §8 Task 6 lists only the four
  tests below, so those were not added (the lock behaviour is covered by
  tests/locks.rs and the handlers use the same `with_repo` /
  `with_sessions`).
- **No cli crate.** `crates/git-strata` does not exist on this branch, so
  `crates/git-strata/tests/readonly_without_daemon.rs` was not created.
  The contract half lives in strata-daemon: `client::call` (write methods
  with no daemon → `Busy` `{reason:"daemon_down"}`; first written as
  daemon.md §6's `ReadOnly`, changed after the merge on impl/cli because
  cli.md D2 (2026-10-23) explicitly rules `Busy` and drops types.md's
  `ReadOnly` wording) and
  `client::needs_daemon` (= not read-only). `cli_readonly_commands_work_without_daemon`
  does the cli's direct read itself (`Store::open_read_only`, `Sessions`
  status, `Search` over a cold reconcile).
- **Client has no read timeout** (rpc I3 / `no_duplex_read_timeout` scans
  the whole crate); a connection that ends without a reply is `Unknown`
  `reply_lost`, never resent.
- **Oracle compare** ignores empty directories (as git does): restoring a
  deletion may prune its emptied parent.
- **Panic-safe test server.** The tests' `serving` helper stops the server
  even when the body panics (otherwise the scope waits forever); applied to
  tests/startup.rs too.
- **`replay_commits` lives in strata-gitio (D18), added on this branch.**
  `Git::replay_commits(exclusive_base, tip, onto)` (crates/strata-gitio/
  src/replay.rs): `rev-list --reverse --first-parent tip --not base`, then
  per commit `merge-tree --write-tree --merge-base=<first parent>` onto the
  running head and `commit-tree` with the original message (identity as
  every gitio commit: config only). A conflict is `Conflict`; no ref is ever
  written (the caller's one transaction does that). Merge commits become
  single-parent commits. A cross-crate, additive change; tests in
  crates/strata-gitio/tests/replay.rs (6).
- **`chain_resolve` Restore = `git.push` + one transaction.** The spec's
  literal steps: plain `git.push(staging, "origin", chain_ref, branch,
  PUSH_TIMEOUT)` (publish's push timeout; `seams::push_timeout` is
  crate-private in strata-publish, so the public `PUSH_TIMEOUT` is used),
  then one `ref_transaction` that verifies the chain is still c and sets
  the anchor to c (the chain is already c). Any transaction failure is
  `Unknown` (`reason: "ref_transaction"`), never `HeadMoved`/`Conflict`.
- **Replay range.** `no_replay_range` when there is no anchor (a Rewound
  chain without `refs/strata/anchor/<b>`) or the (advanced) base is not an
  ancestor of c. The result reports the new chain tip and anchor; no
  replayed-commit count (gitio has no rev-count API, not added).
- **Restore chain check** (`c` still the one seen before the prompt) only
  applies when a prompt happened, like `push`.
- **Observe after `chain_resolve`.** `main_tip` is read under the lock
  before the fetch and after the writes; if it moved, `observe_main` runs
  after the lock (`observe_if_moved`, at most twice) on the new tip's tree
  exported to `<root>/daemon/observe/<pid>-<ns>` (removed afterwards). A
  failure is an `ObserveFailed` entry in the reply's `warnings`.
- **`publishing` flag for `resolve_move`** = an open publish row in phase
  `Running` whose group has a batch of the session. Waited for holding no
  queue or lock, polling, at most the lock wait, then `Busy
  {reason:"publishing"}` (nothing written). The rm handler does not wait
  for it (unchanged from Task 6).
- **Tracker after Accept.** `Trackers::restart` takes the session/repo
  Tracker (if one exists), `reset()`s it, then `watch_or_full`; Abandon
  leaves it. With no Tracker the next request creates a fresh one anyway.
- **`resolve_move` seams.** `resolve::resolve_move(core, session, repo,
  choice, resolve, after_accept)`: the handler passes
  `Sessions::resolve_failed_move` and `Trackers::restart`; the tests pass
  doubles to see the queue held, no repo lock, and reset-after-Accept
  order. A real failed move is session's own test territory.
- **`chain_resolve` precondition before the lock** reads the tracking ref
  as it is (no fetch); a chain that was never fetched (`publish_chain` →
  None) is `not_rewound_or_diverged` too.
- **`Core::git` / `Core::repo_cfg`** became `pub(crate)` for resolve.rs.


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

## Task 4

`confirm_flow.rs`: `flags_of` / `tighten` / `relaxes` / `needs_confirm` /
`apply`, `Pins` (`pins.json` load at startup step 5: first start pins the
config, tightening written back at once, relaxations pending →
`ConfigRelaxPending`; atomic 0600 write), `require_validated(_with)` (§5.3
step 4, no lock held), `recheck` (step 7, `NotConfirmed`
`action_changed`), `config_relax_with` (§5.5: `NoSuchRepo`, `not_relaxed`,
then require → validate → write). `Core::pinned`, `Core::config_relax(_with)`.
`plist.rs`: `plist_text`, `agent_plist_path`, `install_agent_with` /
`uninstall_agent_with` over the `Launchd` seam, production `install_agent`.
tests/confirm_flow.rs (8): the three §8 `config_relax_*` tests, prompt holds
no queue/repo lock (Hang verdict; both locks taken during the prompt),
action mismatch → `NotConfirmed`, no `NotTty` in the source, D1 PATH in
`InstallEnv`, plist keys (Label, KeepAlive, RunAtLoad, Aqua, Interactive,
D1 PATH, absolute StandardErrorPath) via plutil in a temp HOME.

## Task 5

`fetch_after.rs`: `AnchorStep`, `advance_anchor_locked`,
`Core::advance_anchor` / `after_new` / `observe_new` / `main_tips`,
`observe_if_moved` (≤ 2 observes), `attach_outside_reason` /
`alert_attach`, `report_guard_disabled`. `track.rs`: `prepare_tool_call`
(hint, or require_full with no paths, then flush) and `drop_tracker`.
`startup.rs`: `Warnings::alert_first` (once-per-key stderr line).
tests/fetch_after.rs (5): anchor cases (equal / leads / behind / diverged /
absent / rewound / kept / never fetched, Busy under a held repo lock, a
`.lock` file making the transaction fail once → Unknown + warning, then
advance clears it), observe failure does not fail new (seam
`new.observe` EIO), hint/flush off the callback thread (plus source scan),
attach-outside alert leaves the index alone, guard-disabled reported not
repinned.

## Task 6

`handlers.rs`: every §4 method except deps_propose / deps_approve (and
Task 7's two) over `Core` / `Daemon`: new (+ anchor advance, observe
retry, attach alert), status, checkpoint, restore, grep, files, diff,
deps_list, publish, push, assign, rm (discard needs confirm), gc,
config_relax, daemon_status. `client.rs`: the client half of the socket.
tests/daemon.rs (4): an `sh` process edits the workspace → ToolCall
checkpoint over the socket → grep/files see it → another process trashes
it → restore equals an independent `git archive` + same-edits oracle; no
daemon → write methods `Busy daemon_down` (was `ReadOnly`, see above), direct read works; a client
that disconnects before the reply gets exactly one checkpoint, never
replayed; a pre-receive hook refusing the push → `Unknown` `PushRefused`,
chain commit, Unknown row and workspace kept, `UnknownPublish` reported
(also after restart), the same publish `Busy publish_unknown`, no second
push.

## Task 7

`resolve.rs`: `move_choice`, `wait_not_publishing`, `resolve_move`,
`chain_resolve` (+ `ChainResolved`), `replay_base`, `observe_tip`,
`with_staging`; wired into `Server::handle` (deps_propose / deps_approve
still answer "not served yet", see above). gitio: `Git::replay_commits`
(D18) with tests/replay.rs (6).
tests/resolve.rs (8): exclusive choice (both / neither → RPC `Failed`,
session untouched); no TTY/confirm (empty verdict script, no prompt, no
probe, `token` param → `Failed`, source scan); queue held and no repo lock
around `resolve_failed_move`, reset+watch only after Accept, queued behind
another holder; accept-origin Drop / Replay (advanced range: only C
replayed, parent o) / Replay conflict (refs byte-identical, no
transaction) / no anchor → `no_replay_range`, never a push; Restore pushes
only a fast-forward (Rewound to P pushes c, staging removed, no force
argv; unrelated rewind and Diverged → `not_fast_forward`, nothing
written); Restore asks `PushDeploy` (refused → `NotConfirmed` with no
fetch/push; approved → one push; accept-origin never prompts); refs under
the repo lock (not a fork → refused with no fetch; healed by the fetch in
the lock → refused, no transaction; lock held → `Busy lock_wait` with no
git write; over the socket, a bad choice → `Failed`, Drop works); publish
calls `publish::publish`, never `stage`. Every remote is a local bare repo.
