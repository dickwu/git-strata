---
name: git-strata
description: Use when working in a repo managed by git-strata (macOS), or when the user asks you to start, checkpoint, diff, restore, publish or remove a git-strata session. Gives each agent session its own isolated APFS-cloned workspace over a shared base snapshot; covers the CLI workflow, the JSON envelope, error codes, and the commands an agent must hand to a human.
---

# git-strata

git-strata gives every coding session its own workspace: an APFS clone of a
shared, sealed base snapshot of each repo (plus its dependency set). One
session's edits never change another session's files, but they are readable:
`status --all`, `grep`/`files` with `--session S` or `--all`, and
`diff --session S` / `--between` show other sessions' content. Edits are
recorded as checkpoints, combined and
committed by `publish`, and sent to the remote only by `git-strata push` or a
publish that is configured to push. macOS only (APFS, FSEvents, launchd).

Use it when the user (or the repo's setup) works in git-strata sessions, when
several agents edit the same repos in parallel, or when you are asked to
checkpoint / restore / publish. `git strata ...` and `git-strata ...` are the
same binary. Full command and flag table: [reference.md](reference.md).

## Prerequisites

- `git-strata` is on `PATH`. The config lives in
  `~/Library/Application Support/git-strata/config.toml`; the daemon socket is
  `~/Library/Application Support/git-strata/daemon.sock`, the pinned safety
  flags `~/Library/Application Support/git-strata/pins.json` (no env var or
  flag changes them; only `$HOME` does). Workspaces live under the data root
  (`[paths] root`, default `/Volumes/GWD/inter/.strata`) as
  `<root>/sessions/<session-id>/<repo>/`.
- The daemon reads `config.toml` only at start; a config change needs
  `git-strata daemon stop` then `git-strata daemon start`.
- Write commands need the daemon. Check with `git-strata daemon status`
  (`data.running`). Start it with `git-strata daemon start` (loads the per-user
  LaunchAgent `com.gwd.git-stratad`; `data.already = true` if it was up). If
  that returns `InvalidInput` with `detail.reason = "no_plist"`, the LaunchAgent
  is not installed: tell the human; do not write a plist yourself.
- Without the daemon, only these work (read-only, straight from the store):
  `status`, `grep`, `files`, `diff`, `deps list`, `daemon status`, and a
  repeated `checkpoint --tool-call ID` whose checkpoint already exists. Every
  other command returns `Busy` with `detail.reason = "daemon_down"`.

## Workflow

```sh
# 1. New session (ticket is just a label). Name the repos you will touch with
#    --repo (repeatable): only those get a workspace, all tracked. This is the
#    fast form; use it. A repo that links a sibling by relative path (../x)
#    needs that sibling too: pass it, or add it later (step 1b).
#    (Without --repo, `--repos A,B` gives EVERY configured repo a workspace
#    and tracks only A,B; do not edit the untracked siblings.)
git-strata new PROJ-123 --repo tool --repo hr-core
#    data.manifest.id                  -> session id, e.g. "s-k3q7xm2a"
#    data.manifest.repos.<repo>.path   -> that repo's workspace directory
#    data.warnings                     -> [] when fine
#    data.manifest.agent               -> the agent recorded for this session
#    The session records which AI agent created it. Claude Code, Codex and
#    Cursor are detected from their environment. Any other agent: run
#    `export STRATA_AGENT=<name>` (e.g. aider; 1-64 of [A-Za-z0-9._-],
#    starting with a letter or digit) before `new`, or pass --agent <name>.
#    A warning of kind AgentNotRecorded means the running daemon is older
#    than this CLI; the session is fine, only the agent is not recorded.

# 1b. Need another repo later? Add it to the same session (tracked):
git-strata add --repo front            # or: --session S --repo front
#    data.added.<repo>.path; data.already lists repos it had already.

# 2. Work inside the workspace. Commands without --session use the cwd.
cd "<data.manifest.repos.tool.path>"
#    ... edit files normally ...

# 3. Checkpoint (one of --tool-call / -m, never both)
git-strata checkpoint -m "parse config before connect"
git-strata checkpoint -m "only these" --path src/a.ts --path ../hr-core/x.php

# 4. Inspect
git-strata status                    # this session; --all for every session
git-strata diff                      # this session vs its pinned base (= --base)
git-strata diff --between s-aaaa1111 s-bbbb2222 --repo tool   # two sessions
git-strata grep 'TODO'             # this session; --session S or --all
git-strata files 'src/**/*.ts'       # glob optional

# 5. Undo to a checkpoint (all paths, or only the listed ones)
git-strata restore 42
git-strata restore 42 src/a.ts

# 6. Publish checkpointed content into a commit
git-strata publish --from s-k3q7xm2a:src/a.ts --from s-k3q7xm2a:src/b.ts

# 7. Push the publish chain (only if the human asked; may need a TTY)
git-strata push tool

# 8. Remove the session once its work is published
git-strata rm --session s-k3q7xm2a
```

Notes:

- `--session` takes a session id or a ticket. A ticket shared by several
  sessions gives `AmbiguousMatch`; use an id from `detail`. `--session` and
  `--all` together is `InvalidInput` (`both_selectors`).
- `checkpoint` may record more than one repo of the session: every path is
  mapped to the session repo whose workspace contains it; paths outside the
  session are skipped. With no paths, every tracked repo of the session is
  scanned. `data.checkpoints` has one row per repo recorded.
- `-m` records a new checkpoint only when something changed since the
  latest one; with no change it returns that latest checkpoint (its id and
  message, not yours). `--tool-call ID` is idempotent per (session, repo, ID):
  every new ID records a row (even with no change), and repeating it returns
  the same rows.
- `publish` publishes the selected paths' current content: before sending
  the publish it records a fresh `BeforePublish` checkpoint of each
  `--from` batch, so edits made to a selected file up to that moment are
  included. There is no option to publish an older checkpoint; to keep an
  edit out, revert it (or `restore` the path) before publishing.
  Without `--from` it is interactive on a TTY and `InvalidInput`
  (`no_batch`) otherwise, so agents always pass `--from SESSION:PATH`.
  PATH is relative to the repo root with no repo prefix (`staff-api/app/x.php`
  is `--from "${S}:app/x.php"`). Write `"${S}:path"`, never `"$S:path"`: zsh
  reads `$S:t`, `$S:h`, ... as modifiers. Path mistakes come back before any
  TTY check: `InvalidInput` `from_repo_prefix` (rerun with
  `error.detail.expected`), `InvalidInput` `from_path` (no workspace has it),
  `AmbiguousMatch` `from_repo` (several have it and not exactly one of their
  checkpoints changed it; rerun with one of `detail.expected`). To pick the
  repo yourself use `--from "${S}:REPO:PATH"` (e.g. `"${S}:admin:src/a.ts"`);
  the middle part is REPO only when it is one of the session's repos.
- `publish` replies only after checks, commit, push and the rebase of every
  session. `git push` waits for the remote's own hooks (bounded at 120 s,
  then `Unknown` `push_unknown`), the rebase is unbounded: a minute or two of
  silence after the remote branch moved is normal. Wait; do not interrupt or
  rerun.
- Release builds never run in a workspace (no remotes, detached `HEAD`, push
  guard). Publish (and push), then in the main checkout `git pull --ff-only`,
  check `HEAD` equals `data.commits.<repo>`, and run the build there; the
  build script is the human's step unless they asked you to run it. See
  README "Release builds from a session".
- Multi-repo `publish` commits repo by repo. When one repo hits `Conflict`,
  repos before it have already committed (and pushed, if push is on); the
  call does not roll them back. See "Publish conflict: fix and re-run" below.
- `diff` and `diff --between` preview current workspace content, checkpointed
  or not. Identical files are not listed; binary / >64 MiB entries carry
  `code: "Binary"` / `"TooLarge"` instead of a preview and the call stays ok.
- `grep` with no hits is `ok` with `"hits": []` (not an error). Skipped binary
  or oversized files are listed in `data.skipped`.
- `deps propose [--session S] [--repo R]` proposes a new dependency set after
  you change install inputs; `deps list` shows open proposals. Add `--switch`
  to move your own session onto the proposed set right away (no prompt; other
  sessions stay pinned). Check `data.switched.kind`: `skipped` with
  `code: OpenFiles` means a process holds files in your deps dir; stop it (only
  if you started it) and run again. Approving for other sessions is
  human-only (below). `data.pending_proposals: [{id, repo}]` in any
  envelope means a proposal waits for the human.

## Publish conflict: fix and re-run

This is the expected loop for agents; the human is not needed unless the
conflict is `assigned`.

1. `publish` returns `ok: false`, `error.code = "Conflict"`.
   - `error.detail.repo` is the repo that conflicted; nothing was committed
     there.
   - `error.detail.conflicts` lists its paths (with the hunks).
   - `data.commits` / `data.pushes` hold repos that already landed in this
     call (also in `error.detail.published_before`). They stay landed.
     The same holds for any publish error: a repo whose commit was written
     before its push timed out (`Unknown`) or was refused (`PushRefused`) is
     in `data.commits` and `data.pushes` too.
2. Fix the listed paths in that repo's workspace (merge in the upstream
   change; `git-strata diff` helps).
3. Optionally `git-strata checkpoint -m "resolve conflict" --path P ...`
   to keep a named point; the re-run snapshots the paths itself.
4. Re-run the exact same `publish --from ...` command. Repos that already
   landed are not committed again: they appear in
   `data.already_published` as `{repo, publish, commit, push}`, and only the
   rest is published (`data.commits`). The check is per `--from` path and by
   content: a path (or directory) whose content is exactly what landed is
   skipped; anything new or changed under it since (including new files in
   a directory you selected) is published as a new commit, and the unchanged
   paths of the same `--from` list do not cause `NoMatch`.
5. Repeat while another repo conflicts.

## Dev servers and `tauri dev`: per-session ports

Every session workspace gets its own block of 4 ports (a lease), so two
sessions can run `tauri dev` (or any dev server) at the same time. The main
checkout has no lease and keeps the project's default ports. Never change
ports by editing committed files (`tauri.conf.json`, `package.json`,
`next.config.*`, `.env`); start the server through git-strata instead:

```bash
cd <your workspace>                     # data.manifest.repos.<repo>.path
git-strata session ports                # read the lease: data.leases[0].ports / .env
git-strata session exec -- bun run dev  # any server that reads $PORT (Next.js does)
git-strata session exec -- sh -c 'vite --port "$PORT" --strictPort'
git-strata session exec --tauri -- bun run tauri dev
```

- `exec` runs the command in place of itself (its output, its exit code)
  in exactly one workspace (`--repo R` when the session has several) with:
  `PORT` = `STRATA_DEV_PORT` (web), `STRATA_CONNECTOR_PORT`,
  `STRATA_CONNECTOR_MCP_PORT`, `STRATA_AUX_PORT`, `STRATA_SESSION`,
  `STRATA_REPO`, `TAURI_CONNECTOR_PROJECT` (the Tauri app dir) and
  `STRATA_TAURI_DEV_CONFIG`. Child processes (e.g. Tauri's
  `beforeDevCommand`) inherit them.
- `--tauri` adds `--config $STRATA_TAURI_DEV_CONFIG` to `tauri dev`, which
  points `build.devUrl` at `http://localhost:$PORT`. The frontend dev server
  must then listen on `$PORT`. It understands `tauri dev`, `cargo tauri dev`,
  `npx|bunx|pnpx tauri dev`, `pnpm exec tauri dev`, and `bun|pnpm|yarn|npm
  run <script>` when that script is literally `tauri dev ...` (or the bare
  `tauri` CLI, with `dev` right after the script name: `bun run tauri dev`,
  `npm run tauri -- dev`). Always write `run`: `bun <script>` without it is
  refused, since the package manager may run its own command of that name.
  Options that pick another package (`--prefix`, `--filter`, `-w`,
  `--workspace`, `--cwd`, ...) and, for npm, any option before its `--` are
  refused. Anything else is `InvalidInput` `tauri_launcher`; use the form in
  `detail.fix`:
  `git-strata session exec -- sh -c 'tauri dev --config "$STRATA_TAURI_DEV_CONFIG"'`.
- tauri-connector does not read `STRATA_CONNECTOR_PORT` yet and picks its
  own port; find it in the `.connector.json` it writes under your
  workspace's `target/` (copies from the main checkout are removed when the
  session is created).
- `exec`'s variables reach only the command it starts and its children, not
  a later connector command or an MCP server that is already running. When
  you talk to the app from such a client, select it explicitly: `tauri-connector
  ... --project <tauri_app_root>` (`tauri_app_root`, else `path`, from
  `git-strata session ports`), or wrap the client:
  `git-strata session exec --no-check -- tauri-connector ...`. Never connect
  to the connector's default ports (9555/9556): they may be another
  session's app.
- Before starting, `exec` checks the lease's ports (`--no-check` skips it;
  `session ports --check` only reports):
  - `PortInUse` `already_running`: a process of this workspace already
    listens. Reuse it; stop it only if you started it.
  - `PortInUse` `foreign`: something else holds a port. Nothing moves on its
    own: run `detail.fix` (`git-strata session ports --reassign --session S
    --repo R`) for your own session, then `exec` again. Never kill a process
    you did not start.
  - `Busy` `port_unidentified` / `port_probe_error`: retry shortly.
- A workspace without a lease yet (e.g. one created before this feature)
  gets one from the daemon on first `ports` / `exec`; an existing lease works
  without the daemon. `Busy` `daemon_down` from `ports` / `exec`: report it
  to the human; do not start the daemon yourself. `Busy` `ports_exhausted`
  (`ports` shows the workspace with `"ports": null`): no free block; tell the
  human. `Busy` `lease_changed` (`--reassign`): another caller moved the
  lease since your check; run `session ports --check` again and decide
  afresh. `Unsupported` `daemon_too_old`: the running daemon predates leases;
  tell the human (install the matching git-strata and restart the daemon).

## Hook: checkpoint after every tool call (Claude Code)

`docs/modules/cli.md` §3.5 documents this for Claude Code only. The human adds
it to Claude Code's settings; git-strata does not write the file:

```json
{"hooks":{"PostToolUse":[{"matcher":"Edit|Write|Bash|MultiEdit","hooks":[{"type":"command","command":"git-strata checkpoint --tool-call"}]}],"SessionStart":[{"hooks":[{"type":"command","command":"git-strata daemon status"}]}]}}
```

- The session is resolved from the hook's cwd; do not add `--session`.
- When `--tool-call` is given and stdin is not a TTY, stdin is read as one JSON
  object (the hook input). Paths are taken from `tool_input.file_path`,
  `tool_input.file_paths[]` and `tool_input.edits[].file_path`; relative paths
  are joined to the object's `cwd`. A tool with no paths (e.g. `Bash`) scans
  every tracked repo of the session. Example input:
  `{"tool_use_id":"toolu_01","cwd":"/…/sessions/s-k3q7xm2a/tool","tool_name":"Edit","tool_input":{"file_path":"src/a.ts"}}`
- `--tool-call` without a value (or with an empty one) takes the id from the
  JSON's `tool_use_id`. If that is missing or empty too, or stdin is a TTY,
  the result is `InvalidInput` (`checkpoint_reason`). `--tool-call ID` with a
  value uses that ID.
- No other agent's hook format is documented; for other agents run
  `git-strata checkpoint -m "..."` yourself after edits.

## The JSON envelope

stdout carries exactly one compact JSON line; exit code 0 iff `ok` is true,
else 1. Human prompts and notices go to stderr only. One exception: a
`session exec` that starts its command prints no envelope; stdout, stderr
and the exit status are the command's (a failure before the start is an
envelope as usual).

```json
{"ok":true,"data":{...},"error":null}
{"ok":false,"data":{},"error":{"code":"Busy","message":"daemon is not running","detail":{"reason":"daemon_down"}}}
```

All three keys are always present; `data` is never null. Branch on
`error.code`, then `error.detail.reason` where present. Never parse `message`.
`--help` / `--version` print plain clap text (no envelope).

| `error.code` (`detail.reason`) | What an agent should do |
| --- | --- |
| `Busy` (`daemon_down`) | Run `git-strata daemon start`, then retry once. |
| `Busy` (`workspace_changed`) | A file changed while reading it; retry. |
| `Busy` (other) | Repo lock held by another operation; wait and retry later. |
| `NotTty` | Command needs a human at a terminal. Stop; give the exact command to the human. |
| `NotConfirmed` | Human confirmation failed / was cancelled / expired. Report it; the human reruns. Exception: `detail.reason` `plan_changed` (publish) means no confirmation was attempted and nothing was written: rerun the same command yourself first; if that rerun returns `NotTty`, hand the command to the human. |
| `AmbiguousMatch` | Ticket matches several sessions; retry with an id from `detail`. |
| `NoSuchSession` | Wrong selector, or cwd is not inside a workspace; `cd` into one or pass `--session`. |
| `NoSuchRepo` | Repo name not in config; check `--repo` / argument. With `detail.reason` `not_in_session`: the session has no workspace of that repo; run `detail.fix` (`git-strata add --session S --repo R`), then retry. |
| `InvalidInput` | Fix arguments per `detail.reason` (`checkpoint_reason`, `diff_mode`, `both_selectors`, `no_batch`, `unknown_command` with `detail.commands`, ...). `chain_diverged` from publish on a repo that needs no confirmation: run `git-strata push <REPO>` (it fetches, replays the unpushed chain onto origin, reruns checks, pushes a fast-forward), then rerun the publish; `replay_deps_unsealed` from push: open a session on the repo, push again. `chain_rewound`, or diverged on a gated repo: hand to the human (`chain`). |
| `Conflict` | Same hunk changed on both sides (another session or upstream); nothing committed in `detail.repo`, earlier repos of the call may have landed (`data.commits`). Fix, checkpoint, re-run the same publish ("Publish conflict: fix and re-run"); between sessions, who resolves can be set by `publish --assign`. |
| `Conflict` (`assigned`) | The path is assigned to another session; only that session may publish it. Do not retry from yours. |
| `NoCommit` | Nothing new to commit or push. Not a failure to fix. |
| `Conflict` `replay_conflict` (from `push`) | Origin moved and `detail.commit` of the unpushed chain does not apply onto `detail.origin`. Nothing was written or pushed. Fix the conflicting change in a session and publish, or hand `git-strata chain --repo R ...` to the human. Do not retry the push unchanged. |
| `NoMatch` | A `--from` path is not in the session's checkpoint; checkpoint first. |
| `Dirty` | `rm` without `--discard` on unpublished work (`detail.repos`; edited untracked siblings in `detail.untracked_repos`). Publish it, or ask the human about `rm --discard`. |
| `InvalidInput` (`url-rewritten`, `url-is-remote-name`, `empty-url`, `no-such-remote`) | git-strata's push refused before sending: origin's push URL is rewritten or ambiguous in the main checkout's git config. Tell the human; do not edit git config. |
| `OpenFiles` | A process holds files in the directory (`detail` has pids). Stop it (only if you started it) and retry. |
| `CheckFailed` | The repo's check failed in staging; fix the code, checkpoint, publish again. |
| `CheckMutates` | Configured check writes files; config problem, tell the human. |
| `HeadMoved` | Another publish or main moved first. Re-read state; do not loop. |
| `PushRefused` | Remote refused (detail has its words). Report to the human. |
| `Unknown` | Outcome uncertain (e.g. push timeout, `socket_closed`). Do not retry or clean up; run `status` and tell the human. Exception: `detail.reason` `poisoned` means nothing was written; the daemon restarts itself to reopen the store, so retry once after `git-strata daemon status` answers again. |
| `Binary` / `TooLarge` | Per-entry markers in `diff` / `grep` results; not a call failure. |
| `Corrupt` | Store damage. Stop and tell the human. |

## Hard rules

1. Never bypass a human gate. There is no `--yes` / `--force` / env var, and
   faking a TTY (`script`, `expect`, a pty) is bypassing. These belong to the
   human; print the exact command and stop:
   - `git-strata rm --session S --discard`
   - `git-strata deps approve <ID> [--sessions S,S]`
   - `git-strata push <REPO>` when the repo needs confirmation (its pinned
     `publish.confirm` is true; `deploy_on_push` alone does not gate, so e.g.
     staff-api with `confirm = false` publishes and pushes headless)
   - `git-strata publish` when it pushes to a repo that needs confirmation
   A repo needs confirmation when `publish.confirm` is true (the pinned
   flag in `pins.json` ORed with `config.toml`); `deploy_on_push` is
   information only. A publish into
   a repo with `publish.push = false` is never gated; only its later `push`
   is. Loosening the config needs a daemon restart plus the human's
   `config relax` (until then `daemon status` alerts `ConfigRelaxPending`).
   Full rule: README.md "Confirmation gate (Touch ID / TTY)".
   - `git-strata config relax <REPO>`
   - `git-strata resolve --session S --repo R --abandon|--accept`
   - `git-strata chain --repo R --accept-origin replay|drop` or `--restore`
2. Never edit `.strata/`, base snapshots, dependency-set caches, or anything
   under the git-strata root other than your own session workspace.
3. Never run `git push` (or `git commit` / ref updates on the main checkout).
   Commits come from `publish`; pushes from `git-strata push` / publish. A
   workspace's pre-push hook refuses `git push` anyway ("push refused in a
   strata workspace; publish with git-strata publish").
4. Do not push unless the human asked for it.
5. After `Unknown`, do not replay, `gc` or `rm`; leave the scene for the human.
   A standalone `push` that ends `Unknown` keeps its staging directory
   (`error.detail.scene`) and `daemon status` lists it as `UnknownPush`,
   also after a restart, until a later definite push of the chain settles it.
6. Only work in your own session; use `--session` / `--all` to read others.
7. Seed files (`.env`, `.env.local`, ...) are cloned from the main checkout
   and kept current by the daemon while you leave them untouched. Never
   print, `cat`, log, `git add` or commit them; they are gitignored and never
   checkpointed or published. An edited copy is not refreshed (`status`
   warns `SeedStale`). README.md "Seed files (.env)".

## Troubleshooting

- `Busy` `daemon_down` right after `daemon start`: check `daemon status`; the
  LaunchAgent may still be starting. If `no_plist`, the human must install it.
- `NoSuchSession` from a hook: the hook's cwd is outside every workspace;
  `cd` into `data.manifest.repos.<repo>.path` before working.
- `publish` says `no_batch` or `NoMatch`: you have not checkpointed the paths.
- A path you edited is missing from a checkpoint: it was outside the session's
  workspaces, or you passed `--path` and left it out.
- `status` shows health `Unknown` with `choices: ["abandon","accept"]`: a move
  failed midway; this is the human's `git-strata resolve`.
- Unexpected command error: `git-strata <cmd> --help` shows the real flags.
- Disk space questions: `git-strata du` (read-only, cached, each figure has
  `as_of`); `du --refresh` re-sizes in the background. `store.trash.eligible`
  is what `git-strata gc` could reclaim; run `gc` only when the human asks.
  `gc --store` also deletes checkpoints of sessions removed over 30 days
  ago: run `gc --store --dry-run` first and only run `--store` when asked.
