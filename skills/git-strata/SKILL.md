---
name: git-strata
description: Use when working in a repo managed by git-strata (macOS), when the cwd is inside a git-strata session workspace (`<root>/sessions/<id>/<repo>`, default root ~/.strata), or when the user asks to start, checkpoint, diff, restore, sync with main, publish, push or remove a git-strata session, or to run a dev server on per-session ports. Gives each agent session its own isolated APFS-cloned workspace over a shared base snapshot. Covers the CLI loop (new, checkpoint, sync --check, publish, which closes a fully landed session), the JSON envelope and error codes, main-sync conflict resolution, and the commands an agent must hand to a human. For any agent that runs shell commands (Claude Code, Codex, Cursor and others).
license: MIT OR Apache-2.0
compatibility: macOS 13+ on APFS. Needs matching git-strata CLI and daemon builds; R2 features require R2 or later. The 0.1.2 CLI cannot read a v6 or v7 store. Only a human starts the daemon.
---

# git-strata

git-strata gives every coding session its own workspace: an APFS clone of a
shared, sealed base snapshot of each repo (plus its dependency set). One
session's edits never change another session's files, but they are readable
(`status --all`, `grep`/`files --all`, `diff --session S`). Edits are recorded
as checkpoints, committed by `publish`, and sent to the remote only by
`git-strata push` or a publish whose repo is configured to push. macOS only.
`git strata ...` and `git-strata ...` are the same binary.

A session is not a git branch. Do not `git commit` or switch branches in a
workspace. Record edits as checkpoints (`git-strata checkpoint`); each
`git-strata publish` creates one commit per participating repo on its
`publish.branch`. A session can publish repeatedly, and one publish can include
several sessions. Do not check out another branch in the workspace.

This file is the short path. Read the linked files when you hit their topic:

| File | Read it when |
| --- | --- |
| [reference.md](reference.md) | you need a command's flags, its `data` shape, or the full `detail.reason` list |
| [sync.md](sync.md) | `sync --check` is not clean, or you get `sync_conflict`, `HeadMoved`, `waiting`, `base_diverged`, `move_conflict` |
| [publish.md](publish.md) | you publish, hit a publish `Conflict`, change dependencies, or `rm` says `Dirty` |
| [dev-ports.md](dev-ports.md) | you start a dev server or `tauri dev`, or get `PortInUse` |
| [errors.md](errors.md) | any other `ok: false`, and troubleshooting |
| [hooks.md](hooks.md) | the human wants checkpoints or the upstream check run automatically by agent hooks |

When this skill and the binary disagree, the binary wins:
`git-strata <command> --help` describes the installed version.

## Before you start

- `git-strata daemon status` must show `data.running: true`. If it does not,
  keep working and tell the human the daemon is down; they run
  `git-strata daemon start`. **Never start, stop or restart the daemon
  yourself**, never run `launchctl` or `brew services`, and never write its
  plist, not even with a temp `HOME`. Without the daemon only `status`,
  `ls`, `grep`, `files`, `diff`, `deps list`, `daemon status`, a repeated
  `checkpoint --tool-call ID`, and `session ports`/`session exec` of a
  workspace that already holds its port lease work; the rest return `Busy` `daemon_down`, and
  edits made meanwhile are not checkpointed (tell the human which).
- Config is `~/Library/Application Support/git-strata/config.toml`; the daemon
  supports hot reload when `features` contains `config.reload`. After editing it,
  run `git-strata config reload`, check the name is in `data.repos`, and read
  `pending_relax` before using a new repo. Rejected restart-only changes and old
  daemons need an owner restart; installing/upgrading remains the human's job.
- The session records which agent created it. Claude Code, Codex and Cursor
  are detected; any other agent runs `export STRATA_AGENT=<name>` (1-64 of
  `[A-Za-z0-9._-]`, starting with a letter or digit) or passes `--agent NAME`
  to `new`.

## The loop

```sh
# 1. Create a session. Name every repo you will touch with --repo (repeatable).
git-strata new PROJ-123 --repo api --repo web
#    data.manifest.id                -> SESSION, e.g. s-k3q7xm2a
#    data.manifest.repos.<repo>.path -> that repo's workspace: work only there
#    A repo that links a sibling by relative path (../x) needs that sibling too.

# 2. Work inside the workspace; commands without --session use the cwd.
cd "<data.manifest.repos.api.path>"
git-strata add --repo front          # later, from inside it: another repo
                                     # (or anywhere: add --session <SESSION> --repo front)

# 3. Checkpoint after each meaningful edit (no hook? do it yourself).
git-strata checkpoint -m "parse config before connect"
git-strata checkpoint -m "only these" --path src/a.ts --path ../web/x.ts

# 4. Inspect / undo
git-strata status                    # this session (--all: every session)
git-strata diff                      # this session vs its base
git-strata grep 'TODO'               # files 'src/**/*.ts' lists paths
git-strata restore 42                # back to checkpoint 42 (every path)
git-strata restore 42 src/a.ts       # ... or only the listed paths

# 5. Before you finish: is main ahead of you?
git-strata sync --check              # data.upstream.clean; see sync.md if false
cd ~ && git-strata sync --session <SESSION> --repo api  # following repos only
cd "<data.manifest.repos.api.path>"   # re-enter the current tree

# 6. Publish checkpointed paths (PATH is relative to the repo root)
S=s-k3q7xm2a
cd ~
git-strata publish --from "${S}:src/a.ts" --from "${S}:src/b.ts"
#    several repos have the path? --from "${S}:REPO:PATH"

# 7. Push only if the human asked (a gated repo needs the human at a TTY)
git-strata push api

# 8. Read the publish reply (step 6, run from `cd ~`). A fully landed session is
#    closed by the publish itself: data.auto_close.sessions["$S"].closed is true,
#    the session is gone, there is nothing to mark or remove. Otherwise
#    .reasons[0] says why it stayed open; the first bullet below says what to do.
```

- Step 8, in full. Read `data.auto_close.sessions[<your session id>]`
  (`data.closed` is the whole group's and never counts other sessions the push
  re-judged). If its `closed` is `true` the session is gone: there is nothing
  to mark or remove; stay out of its path. Otherwise `reasons[0]` says why it
  stayed open:
  - `disabled`: the daemon neither closes nor marks for this repo set; mark
    done and remove by hand as before (`mark done`, then
    `git-strata rm --session S` when finished).
  - `unpublished`: publish the listed paths (or `mark active` and keep
    working). If `detail.regenerable` lists some of them, those are build
    output by the repo's rule and your own `git-strata rm --session S` lets
    them go. If `detail.landing.<repo>.historical` lists some, they hold an
    older publish's content, not main's: publish them if the revert is meant.
    If `detail.landing.<repo>.why_none` is `no_built_base`, the paths are
    published but the judge cannot prove it: run `git-strata rm --session S`
    yourself (it refuses `Dirty`, so it is safe). If it is `no_landed_publish`
    with `published: true`, they landed as a replayed copy: run
    `git-strata sync --session S`, then `git-strata rm --session S`.
  - `open_files`: close the holders, then `git-strata rm --session S` yourself.
  - `changed_during_remove`: wait until your tools are idle, then
    `git-strata rm --session S`.
  - `not_pushed`, `push_refused`: `detail.repo` may be another repo than the
    one you just published, and an earlier publish of yours may be what is
    waiting (`detail.outstanding` lists them all). If `detail.on_chain` is
    `true`, hand the push to the human; the next push of that repo closes the
    session, and `git-strata rm --session S` is the fallback if it stays
    listed. If it is `false`, no push lands that commit: tell the human if it
    was meant to land and run `git-strata rm --session S` when done.
  - `not_merged`: the repo publishes to another branch than its default, in
    this call or an earlier one: the daemon never closes such a session; run
    `git-strata rm --session S` yourself when done and tell the human the
    merge is theirs.
  - `kept_trees` with every `changed` false: run `git-strata rm --session S`
    when you are done.
  - `embedded_repo`: `detail.embedded_repos` names a nested git repository no
    publish covers: move out what you need, then `git-strata rm --session S`,
    which takes it to the trash.
  - `assigned`: `detail.assigned` lists every path, by repo: publish them, or
    tell the human to assign them to another session (for a repo that is not
    one of this session's, only the human can).
  - `deps_pending` with `detail.own` true and `detail.state`
    `awaiting_publish`: your own deps draft never landed: publish its install
    inputs if you meant the change (a repo with `detail.tracked` false cannot
    be published); no command cancels a draft, so if you dropped the change,
    run `git-strata rm --session S` when you are done.
  - `conflicts` with `detail.report` true: your `sync` left a conflict:
    `cd ~ && git-strata sync --session S --repo <detail.repo>` reports it
    again; resolve it as that says, or run `git-strata rm --session S` if you
    no longer want the sync.
  - `busy`: another operation had the session, nothing was judged or marked;
    run `git-strata rm --session S` when it is done, or publish again.
  - `extended`: a repo was added to this session while this call ran: nothing
    was judged or marked and the session is yours, so publish the new work
    when it is ready.
  - anything else: tell the human.

  If `mark.written` is `false` and `mark.skipped` is not `unchanged`, these
  reasons are in this reply only: act on them now. Never `rm --discard`. A
  successful publish reply without `data.auto_close` comes from a daemon that
  lacks the `publish.auto_close` feature: mark the session done and leave
  removal to the human, as before.
- Marking requires the daemon feature `session.mark`; an older daemon returns
  `Unsupported daemon_too_old`. Done notes are 1–2000 UTF-8 bytes, with tab/newline
  allowed and other C0 controls refused. Active clears the mark and takes no note.
- `--session` takes a session id or a ticket (`AmbiguousMatch` when a ticket
  names several; use an id from `detail`). Write `"${S}:path"`, never
  `"$S:path"`: zsh reads `$S:t`, `$S:h` as modifiers.
- `checkpoint -m` with nothing changed returns the latest checkpoint and does
  not record the new message, so it cannot feed a later publish subject;
  `publish` snapshots the selected paths itself, so it always publishes their
  current content. To keep an edit out, revert or `restore` it first.
- `publish` replies only after checks, commit, push and the rebase of every
  session; a minute or two of silence is normal. Wait; never interrupt or rerun.
- Even after `cd ~`, an agent's parent process may still hold a cwd in the
  workspace. Exit or move that process out; if intentionally keeping it, use
  `cd ~ && git-strata sync --session S --allow-cwd` (preserving any other
  request flags), then re-enter the live workspace. This exempts only your
  PID chain; other holders still need to cd out or `--allow-cwd-holders`.
- After a sync or automatic move (`result: moved` / `source_only`), `cd` back
  into the workspace by its path and restart processes you started there.
- Dev servers: never edit ports in committed files; run them with
  `git-strata session exec -- <cmd>` ([dev-ports.md](dev-ports.md)).

## Reading replies

stdout is exactly one compact JSON line; exit code 0 iff `ok` is true.
Ordinary errors use 1; `config reload` uses 2 for rejection, 3 while pending,
and 4 for an old daemon (argument, transport and output failures use 1).
`note:` lines and waiting progress go to stderr. Pipe stdout to `jq`; never
combine streams with `2>&1 | jq`.

`git-strata help gate`, when available in the installed binary, contains the
full confirmation rules. On older installs use `git-strata <command> --help`
for that version's rules.

The envelope is:

```json
{"ok":true,"data":{...},"error":null}
{"ok":false,"data":{},"error":{"code":"Busy","message":"...","detail":{"reason":"daemon_down"}}}
```

Branch on `error.code`, then `error.detail.reason`; never parse `message`.
When a reply carries a `fix` (in `detail`, `upstream`, `deps` or a warning),
it is the command to run next, or the text to hand the human when it starts
with "ask the human". Never run a human-only command (hard rule 1) yourself.
Exception: an `upstream` with `as_of_rm: "before"` describes a session that
is already closed (read before teardown); ignore its `fix` and run nothing
against that session. Exceptions to the envelope: successful `help`, `--help`/`--version`, `ls --table`, a
`session exec` that started its command, and `sync --check --hook`.
Unknown help topics return an `InvalidInput` / `help_topic` envelope.

| You got | Do |
| --- | --- |
| `Busy daemon_down` | keep working; tell the human; retry after `daemon status` shows running |
| `Busy too_many_open_files` | the daemon is out of file handles; tell the human; do not retry in a loop or start another daemon ([errors.md](errors.md)) |
| `Busy publish_unknown` | do not loop; tell the human ([errors.md](errors.md)) |
| `Busy client_timeout` | back off before retrying; nothing was changed by that request |
| `Unknown client_timeout` | it may still run; inspect status; do not retry, gc, or rm |
| `HeadMoved baseline_moved` | rerun the exact `publish --from …` command in `detail.fix` to checkpoint against the new baseline |
| `OpenFiles caller_cwd` | run `detail.fix` from outside, preserving its repo/resolution flags; then cd back |
| `OpenFiles cwd_holders` | `--allow-cwd` accepts your own PID chain's stale cwd; other holders must cd out or be explicitly accepted with `--allow-cwd-holders` |
| `Busy pins_tightened` | rerun the command once on the new catalog; do not loop inside the old operation |
| `Busy` (other) | another operation holds the lock; wait and retry |
| `NotTty` / human-only command | stop; give the human the exact command |
| `NoSuchSession` | `cd` into your workspace or pass `--session` |
| `NoSuchRepo not_in_session` | run `detail.fix` (`git-strata add --session S --repo R`) |
| `InvalidInput no_batch` | pass the batch: `--from SESSION:PATH` (repeatable) |
| `NoMatch` | a `--from` path is not checkpointed: checkpoint it, rerun |
| `Conflict` (publish) | fix the listed paths, rerun the same publish ([publish.md](publish.md)) |
| `InvalidInput base_not_on_main` / `base_diverged_from_main` | hand `detail.fix` to the human; do not push from main or accept divergence yourself |
| `InvalidInput base_behind_main` | rerun the `new` command in `detail.fix` without `--base` |
| `BaseAheadOfMain` warning | local base commits are pinned; publish requires them in B or main_tip |
| `Conflict sync_conflict` / `HeadMoved` | resolve the report ([sync.md](sync.md)) |
| `Conflict assigned` | another session owns the path; do not retry |
| `CheckFailed` | fix the code, checkpoint, publish again |
| `Dirty` (rm) | nothing was removed: publish the listed paths (or `mark active` and keep working), or mark the session done and tell the human what is unpublished; never run `rm --discard` or seek a bypass |
| `PortInUse` | [dev-ports.md](dev-ports.md) |
| `Unknown` | do not retry, `rm` or `gc`; run `status` and tell the human |
| `Corrupt` | stop and tell the human |

Everything else: [errors.md](errors.md).

## Hard rules

1. A full publish closes the session; you need no `rm`. If it stays open,
   follow `data.auto_close` (step 8), which says whether
   `git-strata rm --session S`, never with `--discard`, is yours to run. A done
   mark, yours or the daemon's, lets the daemon close the session on a later
   publish or push of its repos once everything has landed: the workspace goes
   to the trash and the checkpoints stay. To keep working, or to keep the
   workspace, run `git-strata mark --session S active`, and do so before an
   `add` if you marked it done yourself.
   Never bypass a human gate. There is no `--yes`, `--force` or env var, and
   faking a TTY (`script`, `expect`, a pty) is bypassing. These are the
   human's; print the exact command and stop:
   - `git-strata rm --session S --discard`
   - `git-strata deps approve <ID> [--sessions S,S]`
   - `git-strata push <REPO>`, and a `publish` that pushes, when the repo
     needs confirmation (`publish.confirm` true in pins.json or config.toml;
     `deploy_on_push` alone gates nothing)
   - `git-strata config relax <REPO>`
   - `git-strata resolve --session S --repo R --abandon|--accept`
   - `git-strata chain --repo R --accept-origin replay|drop` or `--restore`
   - `git-strata daemon start|stop`

   Never pass `sync --accept-diverged` on your own: it is valid only after
   the human's chain recovery ([sync.md](sync.md)).
2. Work only in your own session's workspaces. Never edit `.strata/`, base
   snapshots, dependency caches, untracked sibling workspaces, or anything
   else under the git-strata root.
3. Never `git push`, `git commit` or move refs in the main checkout. Commits
   come from `publish`; pushes from `git-strata push` or publish.
4. Do not push unless the human asked.
5. After `Unknown`, do not replay, `gc` or `rm`; leave the scene for the human.
6. Seed files (`.env`, `.env.local`, ...) are cloned from the main checkout
   and refreshed when it changes, unless you edited the copy (then `status`
   warns `SeedStale`). They are never checkpointed or published. Never print,
   `cat`, log, `git add` or commit them. Details: README
   [Seed files (.env)](https://github.com/dickwu/git-strata/blob/main/README.md#seed-files-env).
7. Run `gc` only when the human asks (`gc --store --dry-run` first).
8. Release builds never run in a workspace: publish (and push), then build in
   the main checkout after `git pull --ff-only` ([publish.md](publish.md)).

`publish -m "subject"` overrides the commit message (`-m` repeats as paragraphs;
`--message-for REPO=TEXT` overrides one repo). Otherwise the latest manual
checkpoint subjects in the unpublished window are used; tool-call IDs are
excluded. A no-change `checkpoint -m` records no new message. `new T --repo R
--base LOCAL_REV` parks at a local commit at or ahead of main; it does not adopt
that commit into the publish chain. Remote publish lines are not supported.

A stuck auto-rebase can hand control back once through the Stop hook. Ask the
user about named holder processes; stop only processes you own, or cd out of
those shells. The returned command runs sync from outside the workspace.
