# git-strata (cli) implementation progress

Branch `impl/cli` (worktree `git-strata-wt/cli`), from `impl/v1` 0c59dfb.
Not merged.

## Tasks (cli.md §8)

| Task | Status | Commit |
|---|---|---|
| pre: daemon `Busy daemon_down` (cli.md D2) | done | f19e18f |
| pre: daemon serves `deps_propose` / `deps_approve` | done | 4bee168 |
| 1 JSON envelope and write failures | done | a6784d9 |
| 2 TTY before confirmation (confirm-class commands) | done | 865b2fc |
| 3 subcommands, read-only path, hook idempotency (+ `tests/readonly_without_daemon.rs`) | done | (this commit) |

## Ambiguity decisions

- **Key order.** The envelope line is built by hand as
  `{"ok":…,"data":…,"error":…}` (serde_json's map sorts keys without the
  `preserve_order` feature). `data` and `error` are serde_json's compact
  rendering; a `null` result becomes `{}`.
- **Write failures.** `emit` writes with `write_fmt` and then flushes
  explicitly; either failing is exit 1 plus `git-strata:
  stdout_write_failed: …` on stderr. Rust's stdout treats `EBADF` as a
  successful write, so the "format write" binary test uses a regular file
  under `RLIMIT_FSIZE = 0` (`EFBIG`, `SIGXFSZ` ignored) instead of a
  read-only fd; the flush failure is tested at the library level (a writer
  whose `flush` fails), since the real stdout cannot fail only on flush.
- **`every_command_emits_ok_envelope` in Task 1** covers the library and
  `daemon status` (the only command that exists in Task 1); Task 3 extends
  it to every subcommand.
- **Config unreadable** (not missing): `InvalidInput`, reason
  `config_unreadable`. Missing: `NoSuchPath`, reason `no_config`.
  types.md has no I/O code.

### Task 2

- **Scope split.** Task 2 implements every confirm-class command over the
  RPC (`deps approve`, `push`, `publish` incl. `--assign` and the
  interactive pick, `rm`, `resolve`, `chain`, `config relax`) plus
  `assign` (the same call as `publish --assign`, D13). The remaining
  commands answer `Unsupported` until Task 3.
- **Order of checks.** `resolve`/`chain` choice errors (`no_choice`,
  `both_choices`) come before HOME/config are even read; then the TTY
  check; then local reads; then the first connection. A refused command
  never connects (the tests count connections on a listening socket).
- **Pins.** `pins.json` missing → the config's `needs_human_confirm()`;
  unreadable or a repo not in it → confirm (D21). A pin or the config
  saying "confirm" is enough (stricter wins). No config → confirm.
- **publish gate.** The TTY is required when any batch repo pushes and
  needs confirmation. When the batch cannot be resolved locally (no store,
  unknown session/path) the gate is applied to every configured repo
  before reporting the resolution error, so a resolution error can never
  be used to learn anything without a terminal.
- **publish batch.** `S:PATH` → the session via the read-only store (id or
  ticket); the repo is the session's only repo, else the repo whose
  workspace has the file or whose base manifest lists it (0 →
  `NoSuchPath`, >1 → `AmbiguousMatch`). The cli sends one
  `checkpoint {reason: BeforePublish}` per (session, repo) and then one
  `publish` with group id 0 (the daemon assigns the id).
- **Interactive publish (§5.4).** The list is the cwd session's newest
  checkpoint per repo (read-only store), numbered on stderr; stdin lines are
  numbers or paths, an empty line or EOF ends.
- **deps approve display.** The proposal and the sessions a read-only
  `plan_rollout` would move are read from the store directly (the cli
  opens the store read-only) instead of a `deps_list` RPC, so the socket
  sees exactly one request (`deps_approve`). If the plan cannot be computed
  locally it says so and the daemon decides. `NoSuchProposal` prints the
  state and the replacement and never approves again; `HeadMoved` prints
  the tip.
- **push / chain --restore display.** `origin..top` of the publish chain via
  a new read-only gitio call `Git::commits_between(base, tip, limit)`
  (`git log --first-parent`), first 500 commits then "… and more". This is a
  small addition to strata-gitio (with its own test in
  `tests/replay.rs`); the cli itself still never runs git.
- **chain --restore** checks `no_chain` (no origin ref / no local chain) and
  `not_ancestor` locally before any RPC.
- **chain output.** Replay/drop → `{repo, choice, refs_moved:true}`;
  restore → `{repo, choice:"restore", outcome}` with outcome `Pushed`
  when the daemon reports `pushed:true` (a refusal or unknown push comes
  back as the daemon's error).
- **assign group.** The newest publish record whose outcome mentions the
  path, else the newest publish, else 0.
- **Connect timeout (D6).** `UnixStream::connect` has no timeout knob; a
  unix-socket connect to a path with no listener fails immediately
  (`ENOENT`/`ECONNREFUSED`), so no 200 ms timer is needed. Recorded as a
  deviation in form, not in behaviour.
- **strata-confirm dependency.** cli.md §2 says the cli's deps do not
  include strata-confirm; `crates/git-strata/Cargo.toml` does not list it,
  but it arrives transitively through the strata-daemon library the cli
  uses for paths. The cli never calls it (the rg check in Task 3 covers
  the source).

### Task 3

- **Files.** `src/main.rs` already existed from Task 1 (the plan says
  "Create"); Task 3 adds `src/cmds.rs` (RPC-or-local dispatch),
  `src/local.rs` (the §5.1 read-only path) and `src/launch.rs`
  (`daemon start|stop`). One `[[bin]]`, no `src/bin`.
- **Which commands fall back when the daemon is down.** `status`, `grep`,
  `files`, `diff`, `deps list` and `daemon status` (the RPC table's
  read-only rows plus `daemon status`, which reports `running:false`).
  Every other command is `Busy`, `detail.reason = "daemon_down"` (D2),
  with "run `git-strata daemon start`" in the message. `checkpoint` is a
  write, so the hook is `Busy daemon_down` too.
- **Base-mode `diff` is always local** (the overlay against base, read
  from the workspace and store); `diff --between` uses the daemon when it
  is up and the local read otherwise.
- **`diff --between` on unrecorded edits.** strata-publish's preview reads
  both sides' blobs from the store; an edit that has no checkpoint yet has
  a blob id from reconcile but no blob, and that entry comes back with
  `code: "Corrupt"` instead of a preview. Not fixed in the cli (it is
  publish's `diff_between`); the tests checkpoint first. **Open issue,
  reported upstream.**
- **add/add between sessions** is `Conflict` (no base), not `Binary`,
  even for a binary file: upstream publish rule order.
- **Binary detection.** strata-search skips a file with a NUL anywhere
  (cli.md says the first 8000 bytes); the cli reports such a file in
  `skipped` with code `Binary`, so the result is the search crate's.
- **Lossy UTF-8.** Search D13 drops non-UTF-8 lines; the cli runs its own
  lossy pass for those so each invalid sequence becomes one U+FFFD
  (`lossy_utf8_one_fffd_per_bad_sequence`).
- **`grep` skipped entries** for a workspace that cannot be attached carry
  no path (only the repo and the code). `files --all` skips a session view
  that errors with one stderr line.
- **`new`.** A ticket given both positionally and as `--ticket` →
  `InvalidInput` "ticket"; `--repos` is required, empty → `InvalidInput`
  "repos".
- **`deps propose`** with no proposal answers `{}` plus one stderr line.
- **Hook path set.** A hook payload whose paths map to no tracked repo
  checkpoints every tracked repo of the session (full checkpoint).
- **`daemon status`** holders include the method and session.
- **`daemon start|stop`** run `launchctl` from `PATH` (tests put a fake
  first; nothing is loaded into the real domain). `start` needs the plist
  (`InvalidInput` `no_plist`); `stop` does not. Already loaded / not loaded
  → `{already:true}`. A launchctl failure is `Unknown`, reason
  `launchctl`. Neither touches the socket.
- **`daemon_up_read_does_not_open_sessions`** is tested indirectly: with
  the daemon up the main checkout is moved away and reads still answer
  (via the daemon); the control shows the local path needs it.
- **Stale socket.** A stopped daemon leaves `daemon.sock`; connecting is
  refused, which is "down". Both the stale and the removed socket are
  tested.
- **Test build.** The test binary has strata-daemon with `test-seams` as a
  dev-dependency feature (the tests run an in-process daemon); the
  released `git-strata` never starts a daemon and does not enable it.
- **`tests/readonly_without_daemon.rs`** (daemon.md §8 Task 6's cli half):
  `cli_readonly_commands_work_without_daemon` (status, grep, files answer;
  publish and push are `Busy daemon_down`).
- **`every_command_emits_ok_envelope`** now drives, against an in-process
  daemon: new, status, status --all, checkpoint -m, grep, files, diff,
  diff --between, deps propose, deps list, daemon status, restore,
  publish --from, gc, rm. Not driven there: `assign` (needs a publish
  conflict), `push`, `resolve`, `chain`, `config relax`, `deps approve`
  (confirm-class, need a TTY; tests/confirm_gate.rs).
- **Check.** `rg -o 'git-strata [a-z]+' docs/modules/cli.md | sort -u | wc -l` = 17 (≥ 14).
