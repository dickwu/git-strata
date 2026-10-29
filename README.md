# git-strata

Per-session workspaces over shared base snapshots, for macOS (APFS, FSEvents,
launchd). Every coding session (human or agent) gets its own APFS clone of
each repo plus its dependency set. Edits are recorded as checkpoints,
committed by `publish`, and sent to the remote only by `git-strata push` or a
publish whose repo is configured to push.

This README is written so an agent can run git-strata without asking a human.
Everything here describes the current code. When in doubt, the binary's
`git-strata <command> --help` is the authority (`-h` is the one-line summary).
Design documents (Chinese) are in [docs/](docs/README.md); the agent skill is
[skills/git-strata/SKILL.md](skills/git-strata/SKILL.md).

`git-strata ...` and `git strata ...` are the same binary.

## Contents

- [Install and daemon](#install-and-daemon)
- [Paths](#paths)
- [Output: the JSON envelope](#output-the-json-envelope)
- [Session lifecycle](#session-lifecycle)
- [Seed files (.env)](#seed-files-env)
- [Dev servers: per-session ports](#dev-servers-per-session-ports)
- [Headless command sequences](#headless-command-sequences)
- [Release builds from a session](#release-builds-from-a-session)
- [Publish conflicts: fix and rerun](#publish-conflicts-fix-and-rerun)
- [Confirmation gate (Touch ID / TTY)](#confirmation-gate-touch-id--tty)
- [Push guards](#push-guards)
- [Error codes and what to do](#error-codes-and-what-to-do)
- [Disk usage (`du`)](#disk-usage-du)
- [Commands](#commands)

## Install and daemon

Two binaries: `git-strata` (the CLI) and `git-stratad` (the per-user daemon).
Write commands go through the daemon over a unix socket.

The daemon runs as the LaunchAgent `com.gwd.git-stratad`, from
`~/Library/LaunchAgents/com.gwd.git-stratad.plist`. The CLI never writes this
plist; installing it is a human step. Its content (from
`strata_daemon::plist::plist_text`):

- `Label` `com.gwd.git-stratad`; `ProgramArguments` the absolute path of `git-stratad`
- `RunAtLoad` true, `KeepAlive` true, `LimitLoadToSessionType` `Aqua`, `ProcessType` `Interactive`, `ExitTimeOut` 900 (a stop waits for in-flight writes instead of killing them)
- `EnvironmentVariables.PATH` `~/.bun/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin`
- `StandardErrorPath` `~/Library/Application Support/git-strata/daemon.err.log`

| Command | What it does | Result |
| --- | --- | --- |
| `git-strata daemon status` | Asks the socket; never fails for a down daemon or missing config | `{running, socket, session?, holders:[], unknown_publishes:[], alerts:[]}` |
| `git-strata daemon start` | `launchctl bootstrap gui/<uid> <plist>` | `{}`; `{already:true}` if loaded; `InvalidInput` `no_plist` if the plist is missing |
| `git-strata daemon stop` | `launchctl bootout`, then waits up to 10 s until launchd drops the agent | `{}`; `Unknown` `launchctl` (`step: bootout_wait`) on timeout |

`start` / `stop` use launchd, not the socket. The daemon reads `config.toml`
only when it starts, so after editing the config run `daemon stop` then
`daemon start`.

Without a running daemon only these work, read-only, straight from the store:
`status`, `grep`, `files`, `diff`, `deps list`, `daemon status`, and
repeating a `checkpoint --tool-call ID` whose checkpoint already exists.
Everything else returns `Busy` with `detail.reason = "daemon_down"`.

### Dev install

`scripts/dev-install.sh` builds both binaries in release mode, stops the
daemon, keeps the previous binaries in a temp directory, copies both into
`~/.cargo/bin` together and starts the daemon again (`--no-start` only builds
and copies).

Signing is opt-in through `STRATA_CODESIGN_IDENTITY`. With it set, both
binaries are signed with that identity and fixed identifiers
(`com.gwd.git-stratad`, `com.gwd.git-strata`), so macOS privacy grants (the
"removable volume" prompt) survive rebuilds; unsigned builds re-prompt after
every install. For local development a self-signed identity is enough:

```sh
openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout key.pem -out cert.pem -subj "/CN=git-strata dev" \
  -addext "basicConstraints=critical,CA:false" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning"
/usr/bin/openssl pkcs12 -export -inkey key.pem -in cert.pem -name "git-strata dev" -out dev.p12
security import dev.p12 -k ~/Library/Keychains/login.keychain-db -T /usr/bin/codesign
security add-trusted-cert -r trustRoot -p codeSign -k ~/Library/Keychains/login.keychain-db cert.pem
security find-identity -v -p codesigning     # lists "git-strata dev"
# then delete key.pem, cert.pem, dev.p12
STRATA_CODESIGN_IDENTITY="git-strata dev" scripts/dev-install.sh
```

`add-trusted-cert` asks for the login password; the first signing may ask to
"Always Allow" codesign to use the key; the first start of a newly signed
daemon asks once for the removable-volume permission.

## Paths

| What | Path |
| --- | --- |
| Config | `~/Library/Application Support/git-strata/config.toml` |
| Pinned safety flags | `~/Library/Application Support/git-strata/pins.json` |
| Daemon socket | `~/Library/Application Support/git-strata/daemon.sock` |
| Daemon stderr log | `~/Library/Application Support/git-strata/daemon.err.log` |
| Data root | `[paths] root` in `config.toml`; default `/Volumes/GWD/inter/.strata` |
| Session workspaces | `<root>/sessions/<session-id>/<repo>/` |

Only `$HOME` moves the first four; there is no flag or env var. The data root
holds `store/`, `bases/`, `deps/` and `sessions/`, and must be on the same
APFS volume as the repos. Agents must not edit anything under the root except
their own session workspace. Config format: [docs/config.md](docs/config.md).

## Output: the JSON envelope

Every call prints exactly one compact JSON line on stdout:

```json
{"ok":true,"data":{...},"error":null}
{"ok":false,"data":{},"error":{"code":"Busy","message":"the daemon is not running; run `git-strata daemon start`","detail":{"reason":"daemon_down","socket":"..."}}}
```

- All three keys are always present. `data` is never null; on an error it may
  still hold partial results (for example repos that already published).
- Exit status is 0 iff `ok` is true, else 1.
- Exception: `git-strata session exec` prints no envelope once its command
  starts; its stdout, stderr and exit status are the command's. A failure
  before the start (bad arguments, `PortInUse`, ...) is an envelope.
- Branch on `error.code`, then `error.detail.reason` where present. Never parse `message`.
- stderr carries only human notices: prompts, alerts, `skipped (not in session ...)`,
  and (only when stderr is a terminal) `pending proposal <id> for <repo>`.
- Proposals still pending for the resolved session ride in
  `data.pending_proposals: [{id, repo}]` (key absent when there are none).
- `--help` / `--version` print plain clap text on stdout and exit 0, with no envelope.
- Bad arguments give `InvalidInput` with `detail.reason` `unknown_command`
  (plus `detail.commands`, and `detail.group` inside `deps` / `daemon` / `config`)
  or `args`.

## Session lifecycle

1. **`new`** creates a session. Two forms:
   - `--repo R` (repeatable or `R1,R2`): only these repos are fetched
     (`git fetch origin <default_branch>`, up to 8 at a time) and get a
     workspace, all **tracked**. Fast; agents should use it. A repo that links
     a sibling by relative path (`../other`) needs that sibling in the session
     too: name it, or `git-strata add` it later.
   - `--repos A,B` (no `--repo`; the original form): every repo in
     `config.toml` is fetched and gets a workspace, because sibling repos link
     to each other by relative path; `A,B` are the **tracked** ones. Only
     tracked repos are watched, checkpointed and published. Untracked siblings
     stay pinned at their base; do not edit them. An edit there is never
     checkpointed and makes `rm` return `Dirty` (`detail.untracked_repos`).
   Giving both is `InvalidInput` (`repo_flags`). The daemon reuses a fetch of
   the same repo younger than 30 s (back-to-back `new`/`add`); publish always
   fetches for itself.
   Result: `data.manifest.id` (the session id, `s-` + 8 base32 chars) and
   `data.manifest.repos.<repo>.{path, tracked, base, head}`.
   **`add --repo R [--session S]`** gives an existing active session
   workspaces for more repos (tracked), built like `new` builds them. Repos it
   already has are listed in `data.already` and left alone. A command naming a
   repo the session lacks fails `NoSuchRepo` with `detail.reason`
   `not_in_session` and `detail.fix` = that `add` command.
2. **Work** in `data.manifest.repos.<repo>.path`. Commands without `--session`
   take the session from the cwd. `--session` accepts an id or a ticket.
3. **`checkpoint`** records edits (`-m MSG`, or `--tool-call [ID]` from a hook).
4. **`status`, `diff`, `grep`, `files`** inspect. `--all` / `--session S`
   read other sessions. There is no `search` command: `grep` searches
   content, `files` lists paths by glob.
5. **`restore CHECKPOINT [PATHS]`** rolls back.
6. **`publish --from SESSION:PATH ...`** commits onto each repo's publish
   branch (after taking a fresh `BeforePublish` checkpoint of each batch).
7. **`push REPO`** pushes the publish chain, only when the human asked.
8. **`rm --session S`** removes the session once its work is published.

**After a publish, sessions follow automatically, but only when it is safe.**
Every publish, push replay, and a daemon catch-up pass (at start and every
60 s) try to move each live session that is behind onto the published tree
(`Auto` policy). A move happens only when the workspace is idle: no process
has a file or cwd open anywhere under it, nothing in it was written within
`rebase.quiet_secs` (default 120 s), and the session's edits project onto the
new tree without conflict (`rm.regenerable` paths excepted). Otherwise the
move is **deferred**, never forced: `status` shows
`behind publish N of REPO: auto-rebase deferred (open_files|recent_writes|conflicts|busy, since HH:MM, attempts k)`
and the catch-up retries with backoff (capped at 30 min). When the publish
changed install inputs (`package.json`, `bun.lock`), the source moves onto the
published tree with the session's old deps set (`rebase.deps_changed =
"move_source"`, the default; `"wait"` keeps the old behaviour) and the deps
proposal stays Pending for the human. Explicit moves (deps approval rollout)
are unchanged. If something writes into the pre-move tree after the swap, that
tree is kept instead of retired (`status`/`ls` list it under `kept_trees`
with the late-written paths); `rm` refuses until it is published or discarded, and the
`rm --discard` prompt names it.

**What `rm` counts as published.** An edit is published when a Done publish
of this session, reachable from origin, has the same bytes (D57), or when a
move onto the newest such publish would discharge the whole entry (for
example the publish merged it with origin's changes in the same file). The
second rule needs that publish's base already built (rm never builds one) and
counts in `detail.landed.<repo>.rebased` / `rebased_onto`. Everything else
makes `rm` refuse with `Dirty` and nothing is removed.

## Seed files (.env)

`seed_files` in `config.toml` (default candidates `CLAUDE.md`, `.env`,
`.env.local`, `.claude/settings.local.json`) are gitignored, untracked files
that a repo needs to run. `new` clones each one present in the main checkout
into every workspace of the session (tracked or not). Today that is
staff-api `.env` and front `.env.local`.

- **Kept current.** Every 2 s (`SEED_REFRESH_EVERY`) the daemon re-clones a
  seed copy whose main-checkout source changed, as long as the copy is still
  exactly what strata wrote (size + mtime, recorded in the store at `new` and
  in the `dev.git-strata.seed` xattr after a refresh). The new copy is staged
  at the layout root and swapped in with one rename; if the copy changed
  meanwhile, the swap is undone.
- **Your edit wins.** A seed you edited or replaced in the workspace is never
  overwritten; `status` warns `SeedStale` while the main checkout's version
  differs; a new session gets the main checkout's version.
- **Never committed or published.** Seeds are excluded from checkpoints, and
  must stay gitignored: a seed that stops being ignored is not refreshed.
  `status` shows only path, source and time, never contents. Do not `cat`,
  print or log seed contents (`.env` holds production credentials).
- Symlink seeds are recreated at `new` and not refreshed.

## Dev servers: per-session ports

Each session workspace leases a block of 4 TCP ports from the daemon when
the session is created (`new`, `add`), so concurrent `tauri dev` / `next dev`
runs in different sessions do not collide. Roles: `web` (base), `connector`
(+1), `connector_mcp` (+2), `aux` (+3). The main checkout has no lease and
keeps the project's own ports. A lease is released when the session is
removed. Range and exclusions: `[dev_ports]` in `config.toml`
([docs/config.md](docs/config.md) §2.6; default `20000-29999`).

```bash
git-strata session ports [--session S] [--repo R] [--check]   # read the lease
git-strata session ports --all                                # every live workspace, read-only
git-strata session exec -- bun run dev                        # PORT=<web> and the rest set
git-strata session exec --tauri -- bun run tauri dev          # also tauri dev --config <devUrl patch>
git-strata session ports --reassign --session S --repo R      # move one lease to another block
```

`exec` replaces itself with the command, run in the workspace with `PORT`,
`STRATA_DEV_PORT`, `STRATA_CONNECTOR_PORT`, `STRATA_CONNECTOR_MCP_PORT`,
`STRATA_AUX_PORT`, `STRATA_SESSION`, `STRATA_REPO`,
`TAURI_CONNECTOR_PROJECT` and `STRATA_TAURI_DEV_CONFIG`
(`{"build":{"devUrl":"http://localhost:<web>"}}`). Committed files are never
edited. Before starting it checks the ports (`--no-check` skips): a listener
from this workspace is `PortInUse` `already_running`; any other listener is
`PortInUse` `foreign` with `detail.fix` = the `--reassign` command. Nothing
is killed and nothing is reassigned automatically. Copies of tauri-connector's
`.connector.json` in cloned `target/` seed caches are deleted when the
session is created. Once a lease exists, `ports` and `exec` read it from the
store and work without the daemon.

`--tauri` accepts `tauri dev`, `cargo|npx|bunx|pnpx tauri dev`, `pnpm exec
tauri dev`, and `bun|pnpm|yarn|npm run <script>` (always with `run`: without
it the package manager may run its own command of that name) when the
script is literally `tauri dev ...` or the bare `tauri` CLI followed by
`dev` in the command (`"tauri": "tauri"` → `bun run tauri dev`, or
`npm run tauri -- dev`). A script name that starts with `-` is refused. Options
that select another package (`--prefix`, `--filter`, `-w`, `--workspace`,
`--cwd`, ...) and, for npm, any option before its `--` are refused
(`InvalidInput` `tauri_launcher`), since the script that runs would not be
the one checked.

`exec` does not set tauri-connector's own variables. tauri-connector
(0.17.7 and later) binds exactly the ports in `TAURI_CONNECTOR_PORT`,
`TAURI_CONNECTOR_MCP_PORT` and `TAURI_CONNECTOR_BRIDGE_PORT`, so map the
lease onto them in an `sh -c` form:

```bash
git-strata session exec -- sh -c 'TAURI_CONNECTOR_PORT=$STRATA_CONNECTOR_PORT \
  TAURI_CONNECTOR_MCP_PORT=$STRATA_CONNECTOR_MCP_PORT \
  TAURI_CONNECTOR_BRIDGE_PORT=$STRATA_AUX_PORT \
  exec bunx tauri dev --config "$STRATA_TAURI_DEV_CONFIG"'
```

The connector's precedence is: a port set in the app's builder code, then
the exact env variable, then `TAURI_CONNECTOR_*_PORT_RANGE`, then a range
set in the builder, then its default ports. An exact port that is busy is
not replaced: the connector does not start and the app keeps running (a
`.connector.json` left by an earlier run then names a dead pid). The ports
it bound are recorded in `.connector.json` under the app's `target/`.

The variables reach only `exec`'s command and its children. A connector
client started separately (or an MCP server already running) selects the
app with `tauri-connector ... --project <tauri_app_root>` from `session
ports`, or runs as `git-strata session exec --no-check -- tauri-connector
...` (with no `--port`, the CLI uses `TAURI_CONNECTOR_PORT`, so map it the
same way in that `sh -c`); never the connector's default ports.

`session ports` lists a workspace with no free block as `"ports": null,
"reason": "ports_exhausted"` next to the others; `exec` there is `Busy`
`ports_exhausted`. `--reassign` sends the base it checked; if another caller
moved the lease in between, the daemon refuses with `Busy` `lease_changed`
and nothing moves.

## Headless command sequences

Everything below needs no human, as long as no repo in the publish is gated
(see [Confirmation gate](#confirmation-gate-touch-id--tty)).

```sh
git-strata daemon status                     # data.running must be true
git-strata daemon start                      # only if not running; then retry once

git-strata new PROJ-123 --repo tool --repo staff-api
#  -> S = data.manifest.id, W = data.manifest.repos.tool.path
git-strata add --session "$S" --repo front   # only if front is needed later
cd "$W"                                      # edit files here
git-strata checkpoint -m "parse config before connect"
git-strata diff                              # vs the base
git-strata publish --from "${S}:src/a.ts" --from "${S}:src/lib"
#  -> data.commits.<repo> = commit oid; data.pushes = pushes done by publish
git-strata rm --session "$S"
```

`diff --session S` outside a workspace and without `--repo` compares the one
repo of S that has changes or checkpoints (`data.repo`); if several do, it is
`InvalidInput` with `detail.reason = "repo"` and `detail.candidates`.

Write `"${S}:path"`, not `"$S:path"`: in zsh `$S:t`, `$S:h`, `$S:r`, ... are
history modifiers applied to `$S`, so `"$S:tool.ts"` silently becomes a
different string.

**`--from SESSION:PATH`.** PATH is relative to the repo root of the workspace
that holds it, with no repo prefix: for `staff-api/app/x.php` in session `S`
write `--from "${S}:app/x.php"`, never `"${S}:staff-api/app/x.php"`. A
directory publishes every checkpointed path under it. The repo is found by
looking the path up in each workspace (on disk or in its base); when several
workspaces have it, the one repo whose checkpoints in the session changed it
is taken. To name the repo outright, write **`--from "${S}:REPO:PATH"`**
(e.g. `--from "${S}:admin:src/utils/a.ts"`). The text between the first and
second `:` counts as REPO only when it is one of the session's repos;
otherwise everything after the first `:` is PATH, so a PATH that itself
starts with `<repo>:` is written `${S}:<repo>:<repo>:...`. Path errors
come back before any TTY check and send nothing to the daemon:

| error | when | `detail` |
| --- | --- | --- |
| `InvalidInput` `from_syntax` | no `:`, empty session, or not a repo path | |
| `InvalidInput` `from_repo_prefix` | PATH starts with a repo name of the session and the rest exists in that repo | `{path, session, repo, expected}`; rerun with `expected` |
| `InvalidInput` `from_path` | no workspace of a multi-repo session has PATH, or `S:REPO:PATH` and REPO's workspace lacks it (or REPO is not in the session) | `{path, session, repos, expected}` |
| `AmbiguousMatch` `from_repo` | several workspaces have PATH and not exactly one of their checkpoints changed it | `{path, session, candidates, changed_in, expected}`; rerun with one of `expected` (`S:REPO:PATH`) |

In a single-repo session any other path is taken as that repo's (the daemon
then reports a path that is not checkpointed as `NoMatch`).

**How long publish takes.** `publish` answers only when everything is done:
checks, the commit, the push (for pushing repos), then the rebase of every
session onto the new base. The push runs `git push` and waits until the
remote closes the connection, which includes the remote's own hooks (a
deploy-on-push remote may keep it open after the branch has already moved);
it is bounded at 120 s, after which the outcome is `Unknown`
(`push_unknown`). The rebase that follows is not bounded. So a publish that
prints nothing for a minute or two after the branch moved on the remote is
still working: wait for its JSON line and do not interrupt or rerun it.

Hook mode (Claude Code `PostToolUse`): `git-strata checkpoint --tool-call`
reads the hook JSON on stdin; see [SKILL.md](skills/git-strata/SKILL.md#hook-checkpoint-after-every-tool-call-claude-code).

## Release builds from a session

A session workspace cannot run a release build, by design:

- its git dir is a strata shadow repo with **no remotes** (origin's URL is
  not copied), so `git pull` / `git fetch` fail;
- `HEAD` is a detached commit (the session's base or last publish), not a
  branch;
- the `pre-push` guard refuses every `git push`;
- the other repos of the session are workspaces too; a build script that
  commits or pushes in its sibling repos would make commits strata never
  sees.

There is no build command, and `git-strata session exec` (see
[Dev servers](#dev-servers-per-session-ports)) only sets dev-port variables
for a command in the workspace; it is not a release path. A release build runs in the
**main checkout**, after the change has been published and pushed:

```sh
git-strata checkpoint -m "release: <what>"               # in the workspace
git-strata publish --from "${S}:path/a" --from "${S}:path/b"
#  -> data.commits.<repo> = C; data.pushes[].outcome.kind must be "Pushed"
#     (repo with publish.push = false: git-strata push <repo>, human on a TTY if gated)
cd <main checkout of the repo>                          # e.g. /Volumes/GWD/inter/tool
git status --porcelain                                  # must be empty
git pull --ff-only                                      # fast-forwards to C
git rev-parse HEAD                                      # must equal C
./build.sh                                              # the repo's own release script
```

The build script runs outside strata: whatever it commits or pushes (its own
repo, sibling repos, deploys) is not covered by strata's checks or
confirmation gate, so it is the human's step unless they explicitly asked the
agent to run it.

## Publish conflicts: fix and rerun

No human is needed unless `detail.reason` is `assigned`.

1. `publish` returns `ok: false`, `error.code = "Conflict"`.
   `error.detail.repo` conflicted and nothing was committed there;
   `error.detail.conflicts` lists its paths with hunks. Repos that already
   landed in this call are in `data.commits` / `data.pushes` and stay landed.
2. Fix the listed paths in that repo's workspace.
3. Rerun the **same** `publish --from ...`. Paths whose content already
   landed are reported in `data.already_published` (`{repo, publish, commit, push}`)
   and not committed again; only the rest is published.
4. Repeat while another repo conflicts.

`Conflict` with reason `assigned`: the path belongs to another session
(`git-strata assign` / `publish --assign`); do not retry from yours.

## Origin moved under the unpushed chain: push replays

A repo with `publish.push = false` keeps its published commits on
`refs/strata/publish/<branch>` until `git-strata push`. When someone else
pushes to origin in between, the chain and origin have **diverged**: the
anchor (origin's tip at the last publish) is still on origin, origin has
commits the chain lacks, and a plain push would be refused as
non-fast-forward.

For a repo that needs **no** confirmation (`publish.confirm = false` in the
pin), `git-strata push REPO` handles this headless:

1. it fetches origin;
2. chain leads origin: pushes as before;
3. diverged: replays the unpushed commits (anchor..chain) onto origin's tip,
   as `chain --accept-origin replay` does. If any commit does not apply, it
   stops with `Conflict` `replay_conflict` (`detail.commit` did not apply onto
   `detail.origin`): no ref is written and nothing is pushed;
4. reruns the repo's checks on the replayed tip, with the repo's sealed deps
   set cloned in (`InvalidInput` `replay_deps_unsealed` when that set was
   never built: open a session on the repo, then push again; `CheckFailed`
   when a check fails). Checks that need sibling repos' layouts may fail
   here; then nothing is written either;
5. moves chain and anchor in one ref transaction, pushes once as a
   fast-forward through the same recording, URL-pinned push as always (never
   forced, never retried; a refusal is `PushRefused` and the next push
   replays again);
6. rebases every live session whose workspace head was one of the replayed
   commits onto its copy, as publish rebases its sessions.

`data` = `{kind: "Pushed", commit, replayed: {from, to, onto, commits,
checks_ran} | null, rebases: [{session, from, to, report}], warnings}`.

```sh
git-strata push staff-api
#  -> ok: data.replayed shows the replay (null when origin had not moved)
#  -> Conflict replay_conflict: fix in a session and publish, or hand
#     `git-strata chain --repo staff-api ...` to the human
```

What stays human: a **rewound** chain (origin dropped commits that were
pushed; `chain_rewound`) is never replayed automatically, since that would
push back commits a human removed; and a repo that needs confirmation never
replays on push (diverged is `NoCommit`), so `chain --repo R --accept-origin
replay|drop` / `--restore` remains the human's command (TTY). `publish` still
refuses `chain_diverged` when origin moved before it; for a repo without
confirmation run `git-strata push REPO` first, then rerun the publish.
Pushing right after each publish keeps the window small.

## Confirmation gate (Touch ID / TTY)

> Keep this section in step with `gate_text!` in
> `crates/git-strata/src/help.rs`, which is the same text in `--help`.
> It describes the current code and is expected to change.

**Which repos are gated.** A repo needs confirmation when
`publish.confirm = true`; that is the only gate
(`RepoConfig::needs_human_confirm`). `deploy_on_push` is information (a push
to that repo deploys) and gates nothing by itself, so a repo may set
`deploy_on_push = true` with `publish.confirm = false` (e.g. staff-api, whose
publish and push run headless). The CLI uses the pinned `confirm` in
`pins.json` ORed with `config.toml`; a missing or unreadable `pins.json`, or
a repo missing from it (new or renamed), counts as needs confirmation. The
daemon pins such a repo with `confirm` on, so deleting `pins.json` or renaming
a repo never loosens the gate; a looser `config.toml` (including a fresh
install's `confirm = false` repos) applies only after `git-strata config relax
REPO` (Touch ID).

**Which commands are gated.** They need a terminal on stdin; most then show a
LocalAuthentication prompt (Touch ID, or the login password) from the daemon:

| Command | Gated when |
| --- | --- |
| `push REPO` | REPO needs confirmation |
| `publish --from ...` | a repo of the batch has `publish.push = true`, needs confirmation, and still has something to commit |
| `rm --session S --discard` | always (TTY + prompt) |
| `deps approve ID` | always (TTY + prompt) |
| `config relax REPO` | always (TTY + prompt) |
| `resolve ...`, `chain ...` | always need a TTY (`chain --restore` may also prompt) |

A `publish` into a repo with `publish.push = false` never asks, even when the
repo needs confirmation; only the later `push REPO` does.

**What an agent sees.** Without a terminal: `NotTty`, `detail`
`{reason: "not_tty", command}`, and nothing was sent to the daemon (for
`publish`, a failed pre-gate step is attached as `detail.plan_error`). A
cancelled, failed or expired prompt is `NotConfirmed` (`detail.reason` e.g.
`cancelled`, `timeout`; the daemon side gives `missing` / `action_mismatch`
when a push arrives without the right confirmation). Exception: `publish`
`NotConfirmed` `plan_changed` means nothing was prompted or written; rerun the
same command once yourself.

There is no flag, env var or config value that skips the gate, and faking a
TTY (`script`, `expect`, a pty) is a bypass. On `NotTty` / `NotConfirmed`,
stop and give the exact command to the human.

**Changing the flags (pins).** At start the daemon tightens pins at once
(`confirm` / `deploy_on_push` ORed, `push` ANDed with the file) and writes
`pins.json`. A loosening (e.g. `confirm` true -> false) is not applied: after
`daemon stop` + `daemon start` the old pin stays and `daemon status` alerts
show `ConfigRelaxPending`. The human then runs `git-strata config relax REPO`
(TTY + prompt), which writes the file's flags into the pin; with nothing to
relax it returns `InvalidInput` `not_relaxed`. Pins of repos removed from the
config are kept. Format:
`{"repos":{"<name>":{"deploy_on_push":bool,"confirm":bool,"push":bool,"regenerable":[...]}}}`
(`regenerable` is written only when non-empty).

**`rm.regenerable` is pinned too.** `[repo.<name>.rm] regenerable = [globs]`
(the `.strata/lww` dialect) lists files a tool regenerates; their unpublished
Modified/Added/Untracked edits in a tracked, healthy workspace do not block
`rm` (they are checkpointed and listed). A narrower list (positive patterns
deleted and/or negations appended at the end) applies at the next start; any
other change is a loosening that needs `config relax REPO`, which applies it
without a daemon restart. A repo with no pin has an empty effective list.
Example (docs only): `regenerable = ["src/constants/permissionMap/generated.json"]`
under `[repo.admin.rm]`. Add the new keys only after both binaries are
upgraded and the daemon restarted.

## Push guards

**Workspace guard.** Each session workspace has a `pre-push` hook that refuses
a plain `git push`:

```
git-strata: push refused in a strata workspace; publish with git-strata publish
```

Never run `git push` (or commit on the main checkout), and never set
`STRATA_PUBLISH` or pass `--no-verify`: the hook only stops accidental pushes
(git-strata's own push sets `STRATA_PUBLISH=1` for it), it is not a security
boundary. Commits come from
`publish`; pushes from `git-strata push` or a pushing publish. `daemon status`
reports a disabled or replaced hook as `PrePushGuardDisabled`.

**git-strata's own push.** It reads origin's push URL from the main checkout
(`git remote get-url --push origin`), refuses before sending if git would not
push to that URL as given, and then runs

```
git push --porcelain --no-follow-tags --end-of-options <url> refs/strata/publish/<branch>:refs/heads/<branch>
```

with the URL pinned (`--config-env=url.<url>.insteadOf=…` and
`pushInsteadOf`) and `GIT_CONFIG_COUNT=1`, `core.hooksPath=<guard hooks dir>`.
The guard `pre-push` checks the real destination, then runs the repo's own
`pre-push`. Refusals send nothing and are `InvalidInput` with
`detail {remote, url, reason}`:

| `reason` | Cause |
| --- | --- |
| `url-rewritten` | a `url.<x>.insteadOf` / `pushInsteadOf` rule rewrites the URL; also at push time ("this push would go to a URL other than the one checked") |
| `url-is-remote-name` | the URL is also a remote name (`remote.<url>.url` / `pushurl`, or a legacy `<git dir>/remotes/<url>` or `branches/<url>` file) |
| `empty-url` | origin's push URL is empty |
| `no-such-remote` | the repo has no `origin` |

Message form: `git would not push to <url> as given (<reason>); nothing sent`.
Agent action: report to the human; the fix is in the main checkout's git
config. Do not edit git config yourself.

Push outcomes: `Pushed` -> ok; `Refused` -> `PushRefused` (message has the
remote's words); uncertain -> `Unknown` (`push_unknown`), and the kept
staging scene is listed by `daemon status` as `UnknownPush`.

## Error codes and what to do

| `error.code` (`detail.reason`) | Agent action |
| --- | --- |
| `Busy` (`daemon_down`) | `git-strata daemon start`, retry once |
| `Busy` (`workspace_changed`) | a file changed while read; retry |
| `Busy` (`publish_unknown`) | an earlier publish of this repo has an unknown outcome; it clears once origin has that commit (checked at each publish), e.g. after the human runs `git-strata push REPO`; a definite remote refusal never blocks |
| `Busy` (`port_unidentified` / `port_probe_error`) | `session exec` could not identify a port's holder; retry shortly |
| `Busy` (`ports_exhausted`) | no free port block in `[dev_ports]`; tell the human |
| `Busy` (`lease_changed`) | `session ports --reassign`: another caller moved the lease since the check; run `session ports --check` again and decide afresh |
| `Busy` (other) | repo lock held; wait, retry later |
| `NotTty` | hand the exact command to the human |
| `NotConfirmed` | report; the human reruns (`plan_changed`: rerun once yourself) |
| `AmbiguousMatch` | ticket matches several sessions; use an id from `detail` (`from_repo`: a `--from` path is in several repos; rerun with one of `detail.expected`, the `S:REPO:PATH` form) |
| `NoSuchSession` | cd into a workspace or pass `--session` |
| `NoSuchRepo` | name not in config; `not_in_session`: run `detail.fix` (`git-strata add --session S --repo R`) |
| `NoSuchBranch` | (`new`) a repo has no `origin/<default_branch>`; tell the human |
| `InvalidInput` | fix arguments per `detail.reason` (`from_repo_prefix` / `from_path`: rewrite `--from` as `detail.expected` says); `chain_diverged` on a repo without confirmation: `git-strata push REPO` replays it (see [Origin moved](#origin-moved-under-the-unpushed-chain-push-replays)), then rerun; `chain_rewound`, or diverged on a gated repo: human runs `chain`; push-guard reasons above: tell the human |
| `Conflict` | fix and rerun the same publish (above) |
| `Conflict` (`assigned`) | another session owns the path; do not retry |
| `NoMatch` | a `--from` path is not checkpointed; checkpoint, rerun |
| `NoCommit` | nothing new; not a failure |
| `Dirty` | `rm` on unpublished work (`detail.repos`, `detail.untracked_repos`, `detail.unresolved`); publish it or ask the human for `rm --discard` |
| `OpenFiles` | a process holds files (`detail` has pids); stop it only if you started it, retry |
| `CheckFailed` | the repo's check failed in staging; fix, checkpoint, publish again |
| `CheckMutates` | config problem; tell the human |
| `HeadMoved` | another publish or main moved first; re-read state, do not loop |
| `PushRefused` | report the remote's words to the human |
| `Unknown` | outcome uncertain (`push_unknown`, `socket_closed`, `launchctl`): do not retry, `gc` or `rm`; tell the human |
| `Binary` / `TooLarge` | per-entry markers in `diff` / `grep`; not a call failure |
| `Corrupt` | store damage; stop, tell the human |
| `PortInUse` | `already_running`: reuse the server this workspace already runs; `foreign`: run `detail.fix` (`session ports --reassign`) for your own session, then `exec` again |
| `Unsupported` (`daemon_too_old`) | the daemon predates dev-port leases; tell the human |

The full closed set of codes: Conflict, HeadMoved, Dirty, Busy, NoSuchPath,
NoSuchBranch, NoSuchCommit, NoCommit, NoMergeBase, IsDirectory, NotDirectory,
NotFile, Binary, NotText, NoMatch, AmbiguousMatch, Exists, NotEmpty,
NameTaken, ReservedName, ReadOnly, TooLarge, InvalidInput, NoSuchSession,
NoSuchRepo, NoSuchCheckpoint, NoSuchProposal, NoSuchView, NotConfirmed,
NotTty, OpenFiles, CrossVolume, Drift, CheckFailed, CheckMutates,
PushRefused, Unknown, Unsupported, Corrupt, PortInUse.

## Disk usage (`du`)

`git-strata du` reports the data root's size from the daemon's cache in about
a second; `git-strata du --refresh` re-sizes everything in the background
(poll `du` until `data.scan.running` is false). Every figure carries `as_of`
(when it was measured) plus `stale` / `pending` unit counts. Read-only; needs
the daemon.

Groups: `store` (`db` = store.db + -wal/-shm, sized every call; `blobs`, `tmp`,
`trash`, `other`), `bases.repos.<repo>`, `deps.repos.<repo>`,
`sessions.list[]` (workspaces + shadow repos). Each has `logical` (file
lengths), `allocated` (blocks per file; a shared clone extent counts once per
file that has it), `private` (APFS private size: blocks only that file owns)
and `shared` = allocated - private.

Accuracy: a unit's real cost is between `private` and `allocated`. Two clones
of the same data both show the shared extent in `allocated` and neither in
`private`, so summing `private` undercounts data shared only between units
(e.g. a session and its base) and summing `allocated` overcounts it. APFS has
no public API for "bytes freed if I delete this tree". Snapshots (Time
Machine) can keep freed blocks; `du` does not see them. `exact: false` means
the volume gave no private size (`private` = `allocated`).

Savings: `savings.live` is logical - private of live sessions (what plain
copies would have cost on top). `savings.cumulative` sums sessions recorded at
`new`/`rm` since `since` (`<root>/daemon/du/sessions.jsonl`); sessions from
before are `unknown`.

Trash: `store.trash.eligible` = entries older than 1 h, with
`reclaim_min`..`reclaim_max` (private..allocated). `git-strata gc` deletes
them, except entries a session, workspace, deps staging, move intent,
alternates file or worktree link still names (reported as refused on
stderr). Before 2026-10-26 nothing emptied `store/trash`.

Store-wide gc is opt-in: `git-strata gc --store --dry-run` reports what it
would reclaim (rows and checkpoints of sessions removed more than 30 days
ago, blobs only they reference, decided deps proposals, closed attempts,
store/tmp leftovers, old trash) and changes nothing; `git-strata gc --store`
does it and runs a budgeted blob integrity scan. Nothing runs it
automatically.

## Commands

Flags, defaults and `data` shapes:
[skills/git-strata/reference.md](skills/git-strata/reference.md). There is no
`--yes`, `--force` or `--human` anywhere.

| Command | Human needed |
| --- | --- |
| `new [TICKET] [--ticket T] (--repo R... \| --repos A,B)` | no |
| `add --repo R... [--session S]` | no |
| `status [--session S \| --all]` | no |
| `checkpoint (--tool-call [ID] \| -m MSG) [--session S] [--path P]...` | no |
| `restore CHECKPOINT [PATHS]... [--session S]` | no |
| `grep PATTERN [--session S \| --all]` | no |
| `files [GLOB] [--session S \| --all]` | no |
| `diff [--base \| --session S \| --between A B] [--repo R]` | no |
| `deps propose [--session S] [--repo R] [--switch]` / `deps list [--repo R]` | no |
| `deps approve ID [--sessions S,S]` | yes |
| `publish --from S:PATH... ` / `publish --assign FILE SESSION` | only when gated |
| `push REPO` | only when gated |
| `assign --file PATH --session S` | no |
| `rm --session S [--discard]` | `--discard` only |
| `gc` | no |
| `du [--refresh]` | no |
| `daemon start \| stop \| status` | no |
| `resolve --session S --repo R (--abandon \| --accept)` | yes |
| `chain --repo R (--accept-origin replay\|drop \| --restore)` | yes |
| `config relax REPO` | yes |
| `session ports [--session S] [--repo R] [--all] [--check] [--reassign]` | no |
| `session exec [--session S] [--repo R] [--tauri] [--no-check] -- CMD...` | no |
