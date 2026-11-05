# git-strata command reference

Describes the R2 CLI and `docs/modules/cli.md` §3. When they
disagree with your installed version, `git-strata <command> --help` wins.
"Human" = needs a TTY (and, where marked, a LocalAuthentication prompt);
an agent must hand these to the human. "No daemon" = works when the daemon
is not running.

`git-strata help [TOPIC|COMMAND…]`, when available in the installed binary,
prints text without environment setup. Topics: gate, push-guards, errors,
errors-publish, output, headless,
sync-ack. Each command/topic is at most 4 KiB; the index is at most 1.5 KiB.
Unknown topics return an `InvalidInput` / `help_topic` envelope.

Run moving sync outside the workspace: `cd ~ && git-strata sync --session S`,
then cd back. `--allow-cwd` accepts your own stale cwd; `--allow-cwd-holders`
accepts others' (also on deps approve/propose --switch). They do not bypass
file or mapping protections. `status`/`sync` accept `--timeout SECS` (0 unlimited)
and `--progress`; the flag beats GIT_STRATA_TIMEOUT. Defaults: status 30 s,
status --all 120 s, sync 600 s. Hooks retain their own five-second budget.

`new --repo R --base REV` parks a session at a local commit. It is still a
checkpoint/publish session, not a git branch. Publishing is refused until every
base-only commit is reachable from the publish baseline B or main; B may be
the leading publish-chain tip. No per-session publish lines are introduced.

## Paths

The config is `~/Library/Application Support/git-strata/config.toml`, the
daemon socket `~/Library/Application Support/git-strata/daemon.sock`, the
pinned safety flags `~/Library/Application Support/git-strata/pins.json` (no
env var or flag changes them; only `$HOME` does). Workspaces live under the
data root (`[paths] root`, default `~/.strata`) as
`<root>/sessions/<session-id>/<repo>/`.

## Commands

| Command | Arguments and flags | Human | No daemon | `data` on success |
| --- | --- | --- | --- | --- |
| `new [TICKET]` | `--ticket <T>` (same as positional), `--repo <R>` (repeatable or comma separated; only these repos get a workspace, all tracked; preferred) or `--repos <A,B>` (the tracked repos; every configured repo still gets a workspace); one of the two, not both (`repo_flags`); `--agent <NAME>` (default: `STRATA_AGENT`, else detected: `claude-code`, `codex`, `cursor-agent`; invalid: `InvalidInput` `agent`); `--base REV` with one `--repo`, or repeat `--base R=REV` for selected repos (requires `new.base`; incompatible with `--repos`) | no | no | `{manifest:{id,ticket?,agent?,root,repos:{<repo>:{base,head,path,tracked}}},warnings:[]}`; warning `AgentNotRecorded` (`daemon_too_old`): the daemon predates agent recording, the session is fine |
| `add` | `--repo <R>` (at least one; repeatable or comma separated), `--session <S>` (default: cwd's session) | no | no | `{session,added:{<repo>:WorkspaceState},already:{<repo>:WorkspaceState},warnings:[]}`; `Exists` `add_leftover`: an earlier failed add left `detail.path` (an `rm` of the session clears it) |
| `ls` | `--table` (a table for people; errors stay envelopes) | no | yes (`data.source: "local"`) | `{sessions:[{id,ticket,state,created,repos,phase?}]}`; cache-only, never runs git |
| `status` | `--session <S>` or `--all` (default: cwd's session); `--brief` (store only: no drift, seeds, mirrors, old trees), `--timeout SECS`, `--progress` | no | yes | `SessionStatus` (per repo `upstream`: behind/ahead, following, state, fix); `--all`: `{sessions:[...]}`; health `Unknown` with `choices` = the human's `resolve` |
| `sync` | `--session S`, repeatable `--repo R`, `--check`, `--hook` (requires `--check`; see [hooks.md](hooks.md)), `--no-fetch`, repeatable `--resolved PATH` / `--keep PATH`, `--target HEAD --generation N`, `--accept-diverged`, `--allow-cwd`, `--allow-cwd-holders`, `--timeout SECS`, `--progress` | no; accepting divergence requires the human's prior chain recovery | no | `--check`: `{session,upstream,base_warnings}` (exit 0 even when unclean); move: `{session,fetched,repos,next,upstream?}` with per-repo `moved`, `source_only`, `up_to_date`, `waiting`, `not_following`, or `conflicts_cleared`; `--check --hook`: optional `{decision:"block",reason}` without an envelope, otherwise silent, always exit 0 |
| `checkpoint` | exactly one of `--tool-call [ID]` (no ID: hook JSON `tool_use_id`) / `-m, --message <MSG>`; `--session <S>`; `--path <P>` (repeatable) | no | repeat of an existing `--tool-call` only | `{checkpoints:[Checkpoint,...]}` |
| `restore <CHECKPOINT> [PATHS]...` | `--session <S>` | no | no | `{checkpoint,session}` |
| `grep <PATTERN>` | regex; `--session <S>` or `--all` | no | yes | `{hits:[...],skipped?:[{session,repo,path,code}]}` |
| `files [GLOB]` | `--session <S>` or `--all` | no | yes | `{paths:[{session,repo,path}]}` |
| `diff` | mode `--base` (default, this or `--session <S>`'s session vs its base) or `--between <A> <B>` (exclusive with `--base`/`--session`); `--repo <REPO>` (required with `--between`) | no | yes | `{repo,only_a:[{path,kind}],only_b:[],both:[]}` (`repo` null when nothing matched); `--between`: `both:[{path,kind,preview?,code?}]` |
| `deps propose` | `--session <S>`, `--repo <R>` (default: cwd's), `--switch` (move this session only), `--allow-cwd`, `--allow-cwd-holders` | no | no | `Proposal`; with `--switch`: `{ proposal, switched: { session, kind: moved\|skipped, code?, detail? } }` |
| `deps approve <ID>` | `--sessions <S,S>` (default: all sessions of the repo), `--allow-cwd`, `--allow-cwd-holders` | yes + confirm | no | `{results:[{session,kind:"moved"\|"skipped",code?,detail?}]}` |
| `deps list` | `--repo <R>` | no | yes | `{proposals:[...]}`; daemon rows carry effective pinned `mode: "apply" \| "confirm"` |
| `publish` | `--from <S:PATH>` or `--from <S:REPO:PATH>` (repeatable; none = interactive on a TTY, else `InvalidInput` `no_batch`); `--assign <FILE> <SESSION>`; `-m TEXT` and `--message-for REPO=TEXT` repeat as paragraphs | when a repo it will still commit pushes and needs confirmation. A guarded repo is exempt only when the content check proves its selected paths already landed; a session still on its old base (its rebase is `Waiting` on a deps proposal) still counts, so it can need a TTY and confirmation even though nothing new would be committed. If the daemon's own replan needs a guarded repo outside the set the CLI's TTY check covered (history changed between the plan and the publish), publish returns `NotConfirmed` with `detail.reason` `plan_changed` before prompting or writing anything (`data.already_published` still lists repos that landed earlier): rerun the same command | no | `{commits:{<repo>:oid},message_source:{<repo>:source},checks_ran,waiting:[],pushes:[{repo,outcome}],already_published:[{repo,publish,commit,push}]}`; on `Conflict`, `error.detail.repo` is the stopped repo and `data` keeps the repos that landed |
| `push <REPO>` | — | when the repo needs confirmation | no | `PushOutcome` `{kind,message}`; for a repo without confirmation also `commit`, `replayed` (`{from,to,onto,commits,checks_ran}` or null), `rebases`, `warnings`: it fetches and replays a diverged chain onto origin first (`Conflict` `replay_conflict` / `CheckFailed` / `replay_deps_unsealed`: nothing written or pushed) |
| `assign` | `--file <PATH> --session <S>` (both required) | no | no | `{}` |
| `mark` | `--session <S>` (required), `done [--note TEXT]` or `active`, `[--agent NAME]` | no | no | `{session, mark}`; default agent finish step; requires `session.mark`; note 1–2000 UTF-8 bytes, tab/newline allowed; active clears metadata |
| `rm` | `--session <S>` (required), `--discard` | `--discard`: yes + confirm | no | `RemoveReport` (`data.regenerable`: paths kept under the pinned `rm.regenerable`); Outside the default agent loop (finish with `mark done`). `Dirty`: mark done and report unpublished work; nothing removed, `detail.repos` / `untracked_repos` / `unresolved`; `Dirty` `changed_during_remove`: retry |
| `gc` | `--store` (also the store-wide gc: sessions removed over 30 days ago, their checkpoints and blobs, decided proposals, store/tmp, blob scrub), `--dry-run` (with `--store`: report only). Plain `gc` also empties `store/trash` entries older than 1 h that nothing names; refused/stuck on stderr. Agents run it only when the human asks, never after `Unknown` | no | no | `{}`; `--store`: `{store: report}`; `--store --dry-run`: `{dry_run:true,preview:{...}}` |
| `du` | `--refresh` (re-size everything in the background; returns at once) | no | no | `{root,now,total,store{all,db,blobs,tmp,other,trash{...,entries,eligible{entries,reclaim_min,reclaim_max},young,floor_secs,gc}},bases{all,repos},deps{all,repos},sessions{all,list[{session,savings,...}]},savings{live,cumulative},scan{running,started,done,queued,full,failed,last_full},took_ms}`; every figure `{logical,allocated,private,shared,files,exact,errors,units,stale,pending,complete,as_of}` |
| `daemon start` / `daemon stop` (human only; login HOME only, D38) | — | no | yes (uses launchd, not the socket) | `{}` or `{already:true}` |
| `daemon status` | — | no | yes | `{running,socket,session?,holders:[],unknown_publishes:[],alerts:[],alerts_at?:[],pins_meta?:{},config?:{generation,hash,loaded_at,last_attempt,restart_required}}` |
| `resolve` | `--session <S> --repo <R>` + exactly one of `--abandon` / `--accept` | yes | no | `ResolutionReport` |
| `chain` | `--repo <R>` + exactly one of `--accept-origin <replay\|drop>` / `--restore` | yes (`--restore` may also confirm) | no | `{repo,choice,outcome?\|refs_moved?}` |
| `config reload` | — | no | no | ReloadReport in data; exits 0/1/2/3/4 |
| `config relax <REPO>` | — | yes + confirm | no | — |
| `session ports` | `--session <S>`, `--repo <R>` (defaults: cwd's session; cwd's repo, else every repo), or `--all` (read-only, every live workspace; unleased ones show `ports: null`, `reason: "unleased"`); `--check` (who listens on each port); `--reassign` (move exactly one workspace's lease to another block) | no | yes when the lease exists | `{leases:[{session,repo,path,tauri_app_root,ports:{web,connector,connector_mcp,aux},env:{...},check?}]}` |
| `session exec -- <CMD>...` | `--session <S>`, `--repo <R>` (exactly one workspace), `--tauri` (add `--config $STRATA_TAURI_DEV_CONFIG` to `tauri dev`), `--no-check` | no | yes when the lease exists | none: the process becomes `<CMD>` (its output and exit code) with the lease env set |

`daemon status` copies `config` when the daemon returns it; the field is absent
when down or omitted by an older daemon. `generation`, `hash` and `loaded_at`
describe the active catalog. `last_attempt` is null before any reload attempt,
then the report plus `at` and a rejection `reason` when present;
`restart_required` lists restart-only fields from that attempt.

`checkpoint -m` with nothing changed returns the latest checkpoint instead of
a new one; every new `--tool-call` ID records a row. `publish` records a fresh
`BeforePublish` checkpoint of each `--from` batch first, so it publishes the
selected paths' current content (no flag selects an older checkpoint).

Selectors: `--session` accepts a session id (`s-` + 8 base32 chars) or a
ticket. Without it, the session comes from the cwd.

Fixed `detail.reason` values produced by the CLI: `daemon_down`,
`client_timeout` (`Busy` for reads/incomplete requests, `Unknown` for complete writes),
`timeout` (invalid timeout), `help_topic` (unknown help topic), `stale_cwd`,
`caller_cwd`, `cwd_holders`, `socket_closed`, `both_selectors`, `checkpoint_reason`, `diff_mode`,
`no_batch`, `no_choice`, `both_choices`, `not_ancestor`, `no_plist`,
`foreign_home`, `foreign_agent`,
`unknown_command` (with `detail.commands`), `args`, `repos`, `ticket`,
`assign_with_from`, `pick`, `stdin`, `not_tty` (code `NotTty`, with
`detail.command`), `push_unknown`, `launchctl`, `rpc_id`, `unknown_method`,
`rpc_failed`, `readonly_client`. From `new`: `repo_flags`, `agent`. From `add`: `add_leftover` (code `Exists`). From `rm`: `changed_during_remove` (code `Dirty`). From `session ports` / `session exec`: `reassign_one`, `ambiguous_repo` (with `detail.repos`), `cwd_mismatch`, `tauri_launcher` (with `detail.fix`), `exec`, `unleased` (a `--all` entry, not an error), `already_running` / `foreign` (code `PortInUse`), `port_unidentified` / `port_probe_error` (code `Busy`), `ports_exhausted` (`Busy`; in `session ports` an entry with `"ports": null`, not an error), `lease_changed` (`Busy`, `--reassign`: check again), `not_active` (`NoSuchSession`), `daemon_too_old` / `no_policy` (`Unsupported`: tell the human); a command `exec` cannot find is `NoSuchPath`. From publish: `assigned`, `chain_rewound`,
`chain_diverged`, `workspace_changed`, `plan_changed`; from push: `replay_conflict`,
`replay_deps_unsealed`, `replay_too_long`, `chain_rewound`; from deps:
`main_moved`; from `config reload`: the daemon rejection reason (e.g. `parse`,
`invalid_config`, `restart_required`, `stale_rows`, `spawn_failed`, `recovery_failed`
for a new repo's pending-rebase reconstruction failure before publication),
`config_rejected` when unspecified, `reload_pending`, `daemon_too_old`, or
`invalid_reload_report`; from `config relax`: `not_relaxed`, `shown_required`,
`config_not_applied`, `action_changed`; from the push guard:
`url-rewritten`, `url-is-remote-name`, `empty-url`, `no-such-remote`.

The client also preserves the daemon's `Busy too_many_open_files` reason when it sheds a
connection because it is out of file handles, including the null-id shed reply. Tell the
human; do not treat it as `rpc_id` or retry in a loop (see [errors.md](errors.md)).

The closed set of `error.code` values (types.md): Conflict, HeadMoved, Dirty,
Busy, NoSuchPath, NoSuchBranch, NoSuchCommit, NoCommit, NoMergeBase,
IsDirectory, NotDirectory, NotFile, Binary, NotText, NoMatch, AmbiguousMatch,
Exists, NotEmpty, NameTaken, ReservedName, ReadOnly, TooLarge, InvalidInput,
NoSuchSession, NoSuchRepo, NoSuchCheckpoint, NoSuchProposal, NoSuchView,
NotConfirmed, NotTty, OpenFiles, CrossVolume, Drift, CheckFailed,
CheckMutates, PushRefused, Unknown, Unsupported, Corrupt, PortInUse.


## Behaviour notes

- `checkpoint` may record more than one repo of the session: every path is
  mapped to the session repo whose workspace contains it; paths outside the
  session are skipped. With no paths, every tracked repo of the session is
  scanned. `data.checkpoints` has one row per repo recorded.
- `-m` records a new checkpoint only when something changed since the
  latest one; with no change it returns that latest checkpoint (its id and
  message, not yours). `--tool-call ID` is idempotent per (session, repo, ID):
  every new ID records a row (even with no change), and repeating it returns
  the same rows.
- `diff` lists the changed paths and their kinds against the base, from
  current workspace content, checkpointed or not. `diff --between A B`
  also previews paths both sides changed (`both[].preview`, `Clean` or
  `Conflict` hunks). Identical files are not listed; binary / >64 MiB entries
  carry `code: "Binary"` / `"TooLarge"` instead of a preview and the call
  stays ok.
- `grep` with no hits is `ok` with `"hits": []` (not an error). Skipped binary
  or oversized files are listed in `data.skipped`.

Sync replies, resolution flags and every sync reason: [sync.md](sync.md).

## `du` figures

Read-only, from the daemon's cache (about 1 s). `as_of` is when a figure was
measured; `stale` / `pending` count units being re-sized in the background.
`logical` = file lengths; `allocated` = blocks per file (a shared clone extent
counts once per file); `private` = blocks only that file owns (APFS private
size); `shared` = allocated - private. A unit's real cost lies between
`private` and `allocated`; freeing it may free less if a base or another
session shares its blocks, and Time Machine snapshots can hold freed blocks.
`savings.live` = logical - private of live sessions; `savings.cumulative`
counts only sessions recorded since `since` (earlier: unknown).
`store.trash.eligible` is what `git-strata gc` would try to delete; agents do
not run `gc` unless asked.

R2 additions: `new T --repo R --base LOCAL_REV` parks at a local commit at or
ahead of main. For several repos, repeat `--base R=REV`; repos without it use
main_tip. It cannot be combined with `--repos`. `publish -m TEXT` repeats as
paragraphs; `--message-for R=TEXT` overrides a repo. New fields require
`new.base` / `publish.message`; old daemons refuse unsupported requested
features before creating/publishing anything. Status adds `ahead` and base_only
for commits on neither the publish target B nor main_tip.

## Desktop menu-bar states

The desktop's read-only tray distinguishes Down (red slash), Alert (red triangle), Checking
(dim ellipsis), Waiting (amber dot), Conflict (violet diamond), Syncing (teal ring), and Normal.
Checking means stale/missing input or unavailable session upstream detail; it is not proof
that the daemon stopped. Conflicts need the session agent and do not count as human waiting.
A non-conflict auto-deferral older than 24 hours appears as one Deferred item per repo,
excluding sessions already covered by stuck apply proposals. Config relax items open Daemon.
Error-class warnings may persist until the cause is dealt with and the human restarts the
daemon. The tray never acknowledges, snoozes, approves or performs daemon writes.

### Config reload

After editing config.toml, run `git-strata config reload`. Its ordinary envelope
`data` contains `outcome` (applied/unchanged/rejected/pending), `generation`, `repos`,
`added`, `changed`, `restart_required`, `pending_relax`, and `reasons`. Check the
repo name in `repos`, even on unchanged (the poll may already have applied it).
Exit 0: applied/unchanged; 2: rejected, old catalog kept; 3: pending, rerun; 4: old
daemon, ask the owner to restart. Other failures use exit 1. No TTY is required.
Relaxations still require the human's `config relax`. Busy/pins_tightened ends the
current operation; rerun from the command boundary. Landed publish rebases can
report Deferred/pins_tightened and are retried by catch-up. Do not restart the
daemon, use launchctl, or add a repo via an unimplemented add-repo command.
