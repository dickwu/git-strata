# git-strata (cli) implementation progress

Branch `impl/cli` (worktree `git-strata-wt/cli`), from `impl/v1` 0c59dfb.
Not merged.

## Tasks (cli.md §8)

| Task | Status | Commit |
|---|---|---|
| pre: daemon `Busy daemon_down` (cli.md D2) | done | f19e18f |
| pre: daemon serves `deps_propose` / `deps_approve` | done | 4bee168 |
| 1 JSON envelope and write failures | done | a6784d9 |
| 2 TTY before confirmation (confirm-class commands) | done | (this commit) |

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
