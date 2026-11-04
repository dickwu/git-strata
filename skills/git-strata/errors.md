# Errors and troubleshooting

## The JSON envelope

stdout carries exactly one compact JSON line; exit code 0 iff `ok` is true,
else 1. Human prompts and notices go to stderr only. Exceptions: `help`, `--help`/`--version`;
`ls --table` prints a text table; `sync --check --hook` prints the hook's
own JSON (see [hooks.md](hooks.md)); and a
`session exec` that starts its command prints no envelope; stdout, stderr
and the exit status are the command's (a failure before the start is an
envelope as usual).

```json
{"ok":true,"data":{...},"error":null}
{"ok":false,"data":{},"error":{"code":"Busy","message":"the daemon is not running; ask the human to run `git-strata daemon start`","detail":{"reason":"daemon_down"}}}
```

All three keys are always present; `data` is never null. Branch on
`error.code`, then `error.detail.reason` where present. Never parse `message`.

| `error.code` (`detail.reason`) | What an agent should do |
| --- | --- |
| `Busy` (`daemon_down`) | Keep working; tell the human the daemon is down (they run `git-strata daemon start`). Retry once `daemon status` shows `running`. Never start it yourself. |
| `Busy` (`workspace_changed`) | A file changed while reading it; retry. |
| `Busy` (`publish_unknown`) | An earlier publish of this repo is `Unknown`. It clears by itself once origin has that commit (checked on each publish). Do not loop: tell the human; they may run `git-strata push <REPO>` first. |
| `Busy` / `Unknown` with `detail.sibling` | Preparing that sibling repo (fetch, anchor) was busy or unknown. `Busy`: retry later. `Unknown`: stop and tell the human. |
| `Busy client_timeout` | Read deadline or final request newline unsent; nothing changed by this request. Back off before retrying. `phase` is connect/send/reply. |
| `Unknown client_timeout` | The complete write may still run. Inspect status; do not retry, gc, or rm. Timeout did not cancel it. |
| `NoSuchSession stale_cwd` | Cwd is in a staging/old tree; cd to `detail.workspace`. |
| `HeadMoved baseline_moved` | The checkpoint baseline tree no longer equals the workspace head's tree. Rerun the exact `publish --from …` command in `detail.fix`; do not treat this as a sync conflict. |
| `OpenFiles caller_cwd` | Nothing moved; cd outside and run `git-strata sync --session S`, then cd back to the workspace path. |
| `OpenFiles cwd_holders` | Other shells would be left in the old tree; ask their owners to cd out. `--allow-cwd` only exempts the caller's PID chain; `--allow-cwd-holders` explicitly accepts others. |
| `Busy` (other) | Repo lock held by another operation; wait and retry later. |
| `NotTty` | Command needs a human at a terminal. Stop; give the exact command to the human. |
| `NotConfirmed` | Human confirmation failed / was cancelled / expired. Report it; the human reruns. Exception: `detail.reason` `plan_changed` (publish) means no confirmation was attempted and nothing was written: rerun the same command yourself first; if that rerun returns `NotTty`, hand the command to the human. |
| `AmbiguousMatch` | Ticket matches several sessions; retry with an id from `detail`. |
| `NoSuchSession` | Wrong selector, or cwd is not inside a workspace; `cd` into one or pass `--session`. |
| `NoSuchRepo` | Repo name not in config; check `--repo` / argument. With `detail.reason` `not_in_session`: the session has no workspace of that repo; run `detail.fix` (`git-strata add --session S --repo R`), then retry. |
| `InvalidInput` | Fix arguments per `detail.reason` (`checkpoint_reason`, `diff_mode`, `both_selectors`, `no_batch`, `unknown_command` with `detail.commands`, ...). `siblings_missing`: the repo's checks need sibling repos; run `detail.fix` (`git-strata add --session S --repo R ...`) or follow `detail.missing[].next`, then rerun. `chain_diverged` from publish on a repo that needs no confirmation: this is a push (the repo usually has `publish.push = false`), so only when the human asked you to push this repo run `git-strata push <REPO>` (it fetches, replays the unpushed chain onto origin, reruns checks, pushes a fast-forward), then rerun the publish; otherwise stop and tell the human the chain diverged and needs `git-strata push <REPO>`. `replay_deps_unsealed` from push: open a session on the repo, push again. `chain_rewound`, or diverged on a gated repo: hand to the human (`chain`). |
| `Conflict` (publish) | Same hunk changed on both sides (another session or upstream); nothing committed in `detail.repo`, earlier repos of the call may have landed (`data.commits`). Fix, checkpoint, re-run the same publish ([publish.md](publish.md)); between sessions, who resolves can be set by `publish --assign`. |
| `Conflict` (`sync_conflict`) | Edit the listed hunks, then sync with `--resolved`, the reported target and generation. Earlier successful repos appear in `detail.done`. |
| `Conflict` (`base_diverged`) | Tell the human; chain recovery must precede accepting the dropped base. |
| `Conflict` (`move_conflict`) | Publish is blocked until the listed workspace conflicts are acknowledged through sync. |
| `InvalidInput` (`resolved_unchanged`, `resolved_unrelated`, `resolved_overlap_changed`) | Use the saved report: edit the conflict, select a reported path, or undo edits that expanded its overlap. `--keep` explicitly accepts unchanged bytes. |
| `Conflict` (`assigned`) | The path is assigned to another session; only that session may publish it. Do not retry from yours. |
| `NoCommit` | Nothing new to commit or push. Not a failure to fix. |
| `Conflict` `replay_conflict` (from `push`) | Origin moved and `detail.commit` of the unpushed chain does not apply onto `detail.origin`. Nothing was written or pushed. Fix the conflicting change in a session and publish, or hand `git-strata chain --repo R ...` to the human. Do not retry the push unchanged. |
| `NoMatch` | A `--from` path is not in the session's checkpoint; checkpoint first. |
| `Dirty` | `rm` without `--discard` on unpublished work (`detail.repos`; edited untracked siblings in `detail.untracked_repos`). Publish it, or ask the human about `rm --discard`. |
| `InvalidInput` (`url-rewritten`, `url-is-remote-name`, `empty-url`, `no-such-remote`) | git-strata's push refused before sending: origin's push URL is rewritten or ambiguous in the main checkout's git config. Tell the human; do not edit git config. |
| `OpenFiles` | A process holds files in the directory (`detail` has pids). Stop it (only if you started it) and retry. |
| `CheckFailed` | The repo's check failed in staging; fix the code, checkpoint, publish again. |
| `CheckMutates` | Configured check writes files; config problem, tell the human. |
| `HeadMoved` | Another publish or main moved first. For sync, use the fresh report and re-resolve; already-resolved unchanged bytes need an explicit `--keep`. Bound retries as in [sync.md](sync.md). |
| `PushRefused` | Remote refused (detail has its words). Report to the human. |
| `Unknown` | Outcome uncertain (e.g. push timeout, `socket_closed`). Do not retry, gc, or rm; run `status` and tell the human. Exception: `detail.reason` `poisoned` means nothing was written; the daemon restarts itself to reopen the store, so retry once after `git-strata daemon status` answers again. |
| `Binary` / `TooLarge` | Per-entry markers in `diff` / `grep` results; not a call failure. |
| `Corrupt` | Store damage. Stop and tell the human. |

When a reply has a `fix`, it is the command to run next (or, when it starts
with "ask the human", the text to relay). Never run a human-only command from
a `fix` yourself (SKILL.md, hard rule 1).

## Daemon down

**Agents never start, stop or restart the daemon** (`git-strata daemon start`
/ `stop` are human-only; never run them, or `launchctl`, yourself, not even
with a temp `HOME`: the LaunchAgent domain is per user, not per HOME). Check
it with `git-strata daemon status` (`data.running`). If it is down, keep
working in the workspace and tell the human it is down so they can run
`git-strata daemon start`. While it is down `checkpoint` (by hand or from a
hook) answers `Busy` `daemon_down` and that checkpoint is not recorded: tell the human which edits
were not checkpointed. Do not write a plist yourself.

Without the daemon, only these work (read-only, straight from the store):
`status`, `ls`, `grep`, `files`, `diff`, `deps list`, `daemon status`, a
repeated `checkpoint --tool-call ID` whose checkpoint already exists, and
`session ports`/`session exec` of a workspace that already holds its lease. Every
other command returns `Busy` with `detail.reason = "daemon_down"`.

Installing, upgrading and rolling back git-strata (including the 0.1.2 store
migration and `[repo.<name>.sync]` tables) are the human's steps; point them at
the [0.1.2 release notes](https://github.com/dickwu/git-strata/blob/main/docs/release-notes/v0.1.2.md).

## Troubleshooting

- `Busy` `daemon_down`: check `daemon status`; if it is down, tell the human.
  Right after the human started it, the LaunchAgent may still be starting.
- `InvalidInput` `foreign_home` / `foreign_agent` / `no_plist` from `daemon
  start|stop`: you should not have run it; tell the human and do not retry.
- `NoSuchSession` from a hook: the hook's cwd is outside every workspace;
  `cd` into `data.manifest.repos.<repo>.path` before working.
- `publish` says `InvalidInput no_batch`: you gave no `--from`; an agent must
  name the batch with `--from SESSION:PATH`. `NoMatch`: a `--from` path is not
  checkpointed; checkpoint it and rerun.
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
