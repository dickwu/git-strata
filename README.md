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
- [Headless command sequences](#headless-command-sequences)
- [Publish conflicts: fix and rerun](#publish-conflicts-fix-and-rerun)
- [Confirmation gate (Touch ID / TTY)](#confirmation-gate-touch-id--tty)
- [Push guards](#push-guards)
- [Error codes and what to do](#error-codes-and-what-to-do)
- [Commands](#commands)

## Install and daemon

Two binaries: `git-strata` (the CLI) and `git-stratad` (the per-user daemon).
Write commands go through the daemon over a unix socket.

The daemon runs as the LaunchAgent `com.gwd.git-stratad`, from
`~/Library/LaunchAgents/com.gwd.git-stratad.plist`. The CLI never writes this
plist; installing it is a human step. Its content (from
`strata_daemon::plist::plist_text`):

- `Label` `com.gwd.git-stratad`; `ProgramArguments` the absolute path of `git-stratad`
- `RunAtLoad` true, `KeepAlive` true, `LimitLoadToSessionType` `Aqua`, `ProcessType` `Interactive`
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
- Branch on `error.code`, then `error.detail.reason` where present. Never parse `message`.
- stderr carries only human notices: prompts, alerts, `skipped (not in session ...)`,
  `pending proposal <id> for <repo>`.
- `--help` / `--version` print plain clap text on stdout and exit 0, with no envelope.
- Bad arguments give `InvalidInput` with `detail.reason` `unknown_command`
  (plus `detail.commands`, and `detail.group` inside `deps` / `daemon` / `config`)
  or `args`.

## Session lifecycle

1. **`new`** creates a session. Every repo in `config.toml` is fetched
   (`git fetch origin <default_branch>`) and gets a workspace, because sibling
   repos link to each other by relative path. `--repos A,B` is required and
   chooses which repos are **tracked**: only tracked repos are watched,
   checkpointed and published. Untracked siblings stay pinned at their base;
   do not edit them. An edit there is never checkpointed and makes `rm`
   return `Dirty` (`detail.untracked_repos`).
   Result: `data.manifest.id` (the session id, `s-` + 8 base32 chars) and
   `data.manifest.repos.<repo>.{path, tracked, base, head}`.
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

## Headless command sequences

Everything below needs no human, as long as no repo in the publish is gated
(see [Confirmation gate](#confirmation-gate-touch-id--tty)).

```sh
git-strata daemon status                     # data.running must be true
git-strata daemon start                      # only if not running; then retry once

git-strata new PROJ-123 --repos tool,staff-api
#  -> S = data.manifest.id, W = data.manifest.repos.tool.path
cd "$W"                                      # edit files here
git-strata checkpoint -m "parse config before connect"
git-strata diff                              # vs the base
git-strata publish --from "$S:src/a.ts" --from "$S:src/lib"
#  -> data.commits.<repo> = commit oid; data.pushes = pushes done by publish
git-strata rm --session "$S"
```

Hook mode (Claude Code `PostToolUse`): `git-strata checkpoint --tool-call`
reads the hook JSON on stdin; see [SKILL.md](skills/git-strata/SKILL.md#hook-checkpoint-after-every-tool-call-claude-code).

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

## Confirmation gate (Touch ID / TTY)

> Keep this section in step with `gate_text!` in
> `crates/git-strata/src/help.rs`, which is the same text in `--help`.
> It describes the current code and is expected to change.

**Which repos are gated.** A repo needs confirmation when
`publish.confirm = true` **or** `deploy_on_push = true`
(`RepoConfig::needs_human_confirm`). The CLI uses the pinned flags in
`pins.json` ORed with `config.toml`; an unreadable `pins.json`, or a repo
missing from it, counts as needs confirmation. `config.toml` may not set
`deploy_on_push = true` with `publish.confirm = false` (validation fails:
"deploy_on_push requires publish.confirm").

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
`{"repos":{"<name>":{"deploy_on_push":bool,"confirm":bool,"push":bool}}}`.

## Push guards

**Workspace guard.** Each session workspace has a `pre-push` hook that refuses
a plain `git push`:

```
git-strata: push refused in a strata workspace; publish with git-strata publish
```

Never run `git push` (or commit on the main checkout). Commits come from
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
| `Busy` (other) | repo lock held; wait, retry later |
| `NotTty` | hand the exact command to the human |
| `NotConfirmed` | report; the human reruns (`plan_changed`: rerun once yourself) |
| `AmbiguousMatch` | ticket matches several sessions; use an id from `detail` |
| `NoSuchSession` | cd into a workspace or pass `--session` |
| `NoSuchRepo` | name not in config |
| `NoSuchBranch` | (`new`) a repo has no `origin/<default_branch>`; tell the human |
| `InvalidInput` | fix arguments per `detail.reason`; `chain_rewound` / `chain_diverged`: human runs `chain`; push-guard reasons above: tell the human |
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

The full closed set of codes: Conflict, HeadMoved, Dirty, Busy, NoSuchPath,
NoSuchBranch, NoSuchCommit, NoCommit, NoMergeBase, IsDirectory, NotDirectory,
NotFile, Binary, NotText, NoMatch, AmbiguousMatch, Exists, NotEmpty,
NameTaken, ReservedName, ReadOnly, TooLarge, InvalidInput, NoSuchSession,
NoSuchRepo, NoSuchCheckpoint, NoSuchProposal, NoSuchView, NotConfirmed,
NotTty, OpenFiles, CrossVolume, Drift, CheckFailed, CheckMutates,
PushRefused, Unknown, Unsupported, Corrupt.

## Commands

Flags, defaults and `data` shapes:
[skills/git-strata/reference.md](skills/git-strata/reference.md). There is no
`--yes`, `--force` or `--human` anywhere.

| Command | Human needed |
| --- | --- |
| `new [TICKET] [--ticket T] --repos A,B` | no |
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
| `daemon start \| stop \| status` | no |
| `resolve --session S --repo R (--abandon \| --accept)` | yes |
| `chain --repo R (--accept-origin replay\|drop \| --restore)` | yes |
| `config relax REPO` | yes |
