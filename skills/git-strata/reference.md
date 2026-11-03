# git-strata command reference

Taken from the built binary's `--help` and `docs/modules/cli.md` §3.
"Human" = needs a TTY (and, where marked, a LocalAuthentication prompt);
an agent must hand these to the human. "No daemon" = works when the daemon
is not running.

| Command | Arguments and flags | Human | No daemon | `data` on success |
| --- | --- | --- | --- | --- |
| `new [TICKET]` | `--ticket <T>` (same as positional), `--repo <R>` (repeatable or comma separated; only these repos get a workspace, all tracked; preferred) or `--repos <A,B>` (the tracked repos; every configured repo still gets a workspace); one of the two, not both (`repo_flags`) | no | no | `{manifest:{id,ticket?,root,repos:{<repo>:{base,head,path,tracked}}},warnings:[]}` |
| `add` | `--repo <R>` (at least one; repeatable or comma separated), `--session <S>` (default: cwd's session) | no | no | `{session,added:{<repo>:WorkspaceState},already:{<repo>:WorkspaceState},warnings:[]}` |
| `status` | `--session <S>` or `--all` (default: cwd's session) | no | yes | `SessionStatus`; `--all`: `{sessions:[...]}` |
| `sync` | `--session S`, repeatable `--repo R`, `--check`, `--hook` (requires `--check`), `--no-fetch`, repeatable `--resolved PATH` / `--keep PATH`, `--target HEAD --generation N`, `--accept-diverged` | no; accepting divergence requires the human's prior chain recovery | no | `--check`: `{session,upstream}` (exit 0 even when unclean); move: `{session,fetched,repos,next,upstream?}` with per-repo `moved`, `source_only`, `up_to_date`, `waiting`, `not_following`, or `conflicts_cleared`; `--check --hook`: optional `{decision:"block",reason}` without an envelope, otherwise silent, always exit 0 |
| `checkpoint` | exactly one of `--tool-call [ID]` (no ID: hook JSON `tool_use_id`) / `-m, --message <MSG>`; `--session <S>`; `--path <P>` (repeatable) | no | repeat of an existing `--tool-call` only | `{checkpoints:[Checkpoint,...]}` |
| `restore <CHECKPOINT> [PATHS]...` | `--session <S>` | no | no | `{checkpoint,session}` |
| `grep <PATTERN>` | regex; `--session <S>` or `--all` | no | yes | `{hits:[...],skipped?:[{session,repo,path,code}]}` |
| `files [GLOB]` | `--session <S>` or `--all` | no | yes | `{paths:[{session,repo,path}]}` |
| `diff` | one of `--base` (default) / `--session <S>` / `--between <A> <B>`; `--repo <REPO>` (required with `--between`) | no | yes | `{only_a:[],only_b:[],both:[{path,kind,preview?,code?}]}` |
| `deps propose` | `--session <S>`, `--repo <R>` (default: cwd's), `--switch` (move this session only) | no | no | `Proposal`; with `--switch`: `{ proposal, switched: { session, kind: moved\|skipped, code?, detail? } }` |
| `deps approve <ID>` | `--sessions <S,S>` (default: all sessions of the repo) | yes + confirm | no | `{results:[{session,kind:"moved"\|"skipped",code?,detail?}]}` |
| `deps list` | `--repo <R>` | no | yes | `{proposals:[...]}`; daemon rows carry effective pinned `mode: "apply" \| "confirm"` |
| `publish` | `--from <S:PATH>` or `--from <S:REPO:PATH>` (repeatable; none = interactive on a TTY, else `InvalidInput` `no_batch`); `--assign <FILE> <SESSION>` | when a repo it will still commit pushes and needs confirmation. A guarded repo is exempt only when the content check proves its selected paths already landed; a session still on its old base (its rebase is `Waiting` on a deps proposal) still counts, so it can need a TTY and confirmation even though nothing new would be committed. If the daemon's own replan needs a guarded repo outside the set the CLI's TTY check covered (history changed between the plan and the publish), publish returns `NotConfirmed` with `detail.reason` `plan_changed` before prompting or writing anything (`data.already_published` still lists repos that landed earlier): rerun the same command | no | `{commits:{<repo>:oid},checks_ran,waiting:[],pushes:[{repo,outcome}],already_published:[{repo,publish,commit,push}]}`; on `Conflict`, `error.detail.repo` is the stopped repo and `data` keeps the repos that landed |
| `push <REPO>` | — | when the repo needs confirmation | no | `PushOutcome` `{kind,message}`; for a repo without confirmation also `commit`, `replayed` (`{from,to,onto,commits,checks_ran}` or null), `rebases`, `warnings`: it fetches and replays a diverged chain onto origin first (`Conflict` `replay_conflict` / `CheckFailed` / `replay_deps_unsealed`: nothing written or pushed) |
| `assign` | `--file <PATH> --session <S>` (both required) | no | no | `{}` |
| `rm` | `--session <S>` (required), `--discard` | `--discard`: yes + confirm | no | `RemoveReport` |
| `gc` | — (also empties `store/trash` entries older than 1 h that nothing names; refused/stuck on stderr) | no | no | `{}` |
| `du` | `--refresh` (re-size everything in the background; returns at once) | no | no | `{root,now,total,store{all,db,blobs,tmp,other,trash{...,entries,eligible{entries,reclaim_min,reclaim_max},young,floor_secs,gc}},bases{all,repos},deps{all,repos},sessions{all,list[{session,savings,...}]},savings{live,cumulative},scan{running,started,done,queued,full,failed,last_full},took_ms}`; every figure `{logical,allocated,private,shared,files,exact,errors,units,stale,pending,complete,as_of}` |
| `daemon start` / `daemon stop` (human only; login HOME only, D38) | — | no | yes (uses launchd, not the socket) | `{}` or `{already:true}` |
| `daemon status` | — | no | yes | `{running,socket,session?,holders:[],unknown_publishes:[],alerts:[]}` |
| `resolve` | `--session <S> --repo <R>` + exactly one of `--abandon` / `--accept` | yes | no | `ResolutionReport` |
| `chain` | `--repo <R>` + exactly one of `--accept-origin <replay\|drop>` / `--restore` | yes (`--restore` may also confirm) | no | `{repo,choice,outcome?\|refs_moved?}` |
| `config relax <REPO>` | — | yes + confirm | no | — |
| `session ports` | `--session <S>`, `--repo <R>` (defaults: cwd's session; cwd's repo, else every repo), or `--all` (read-only, every live workspace; unleased ones show `ports: null`, `reason: "unleased"`); `--check` (who listens on each port); `--reassign` (move exactly one workspace's lease to another block) | no | yes when the lease exists | `{leases:[{session,repo,path,tauri_app_root,ports:{web,connector,connector_mcp,aux},env:{...},check?}]}` |
| `session exec -- <CMD>...` | `--session <S>`, `--repo <R>` (exactly one workspace), `--tauri` (add `--config $STRATA_TAURI_DEV_CONFIG` to `tauri dev`), `--no-check` | no | yes when the lease exists | none: the process becomes `<CMD>` (its output and exit code) with the lease env set |

`checkpoint -m` with nothing changed returns the latest checkpoint instead of
a new one; every new `--tool-call` ID records a row. `publish` records a fresh
`BeforePublish` checkpoint of each `--from` batch first, so it publishes the
selected paths' current content (no flag selects an older checkpoint).

Selectors: `--session` accepts a session id (`s-` + 8 base32 chars) or a
ticket. Without it, the session comes from the cwd.

Fixed `detail.reason` values produced by the CLI: `daemon_down`,
`socket_closed`, `both_selectors`, `checkpoint_reason`, `diff_mode`,
`no_batch`, `no_choice`, `both_choices`, `not_ancestor`, `no_plist`,
`foreign_home`, `foreign_agent`,
`unknown_command` (with `detail.commands`), `args`, `repos`, `ticket`,
`assign_with_from`, `pick`, `stdin`, `not_tty` (code `NotTty`, with
`detail.command`), `push_unknown`, `launchctl`, `rpc_id`, `unknown_method`,
`rpc_failed`, `readonly_client`. From `session ports` / `session exec`: `reassign_one`, `ambiguous_repo` (with `detail.repos`), `cwd_mismatch`, `tauri_launcher` (with `detail.fix`), `exec`, `unleased` (a `--all` entry, not an error), `already_running` / `foreign` (code `PortInUse`), `port_unidentified` / `port_probe_error` (code `Busy`), `ports_exhausted` (`Busy`; in `session ports` an entry with `"ports": null`, not an error), `lease_changed` (`Busy`, `--reassign`: check again), `not_active` (`NoSuchSession`), `daemon_too_old` (`Unsupported`). From publish: `assigned`, `chain_rewound`,
`chain_diverged`, `workspace_changed`, `plan_changed`; from push: `replay_conflict`,
`replay_deps_unsealed`, `replay_too_long`, `chain_rewound`; from deps:
`main_moved`; from `config relax`: `not_relaxed`; from the push guard:
`url-rewritten`, `url-is-remote-name`, `empty-url`, `no-such-remote`.

The closed set of `error.code` values (types.md): Conflict, HeadMoved, Dirty,
Busy, NoSuchPath, NoSuchBranch, NoSuchCommit, NoCommit, NoMergeBase,
IsDirectory, NotDirectory, NotFile, Binary, NotText, NoMatch, AmbiguousMatch,
Exists, NotEmpty, NameTaken, ReservedName, ReadOnly, TooLarge, InvalidInput,
NoSuchSession, NoSuchRepo, NoSuchCheckpoint, NoSuchProposal, NoSuchView,
NotConfirmed, NotTty, OpenFiles, CrossVolume, Drift, CheckFailed,
CheckMutates, PushRefused, Unknown, Unsupported, Corrupt, PortInUse.

## Sync replies and resolution

`sync --check` reads local refs/store without fetching or writing. It returns
`{session, upstream}`; inspect per-repo `following` before acting on `behind`.
For `following: false`, ignore behind and only acknowledge named conflicts:
there is no sync fix and sync never moves that repo. `--check --hook` reads
Claude Code Stop-hook JSON from stdin, uses cached data, and has a five-second
total deadline covering stdin and the socket. It is silent on errors/outside a
workspace and when `stop_hook_active` is true, and always exits zero.

For `Conflict sync_conflict`, name every reported path in one call, using repeated
`--resolved` or `--keep` (one choice per path), `--repo R`, and both
`--target <detail.target.head>` and `--generation <detail.generation>`.
`--resolved` requires edited bytes; `--keep` deliberately accepts the current side.
Non-overlapping upstream hunks still merge. Partial submissions keep accepted
resolutions for the same target/generation and report the remaining conflicts.
HeadMoved invalidates the old choices and supplies a fresh report; review it,
re-resolve, or explicitly keep already-edited bytes with its new binding.

The binding exceptions are named-conflict acknowledgements: O7 legacy conflicts
with no saved report when up to date, legacy-only named paths with no saved
report even when behind/diverged, and named conflicts in non-following repos.
They clear only those markers under the queue, repo lock and G0, returning
`conflicts_cleared` without moving or accepting divergence. Other resolutions
require binding, even if no report currently exists. For a following repo, finish
a saved report before separately acknowledging unrelated legacy paths.

Per-repo `moved` / `source_only` replies contain `from`/`to` heads and deps,
`resolved`, `kept`, and `whole_file` (non-text resolutions take the current file
or absence whole). Optional/diagnostic fields:

| Field | Meaning / agent action |
| --- | --- |
| `lww` | Paths resolved by configured last-writer-wins rules; review and rerun checks. |
| `regenerable_kept` | Conflicting paths retained under pinned `rm.regenerable`; review or regenerate, then rerun checks. |
| `accepted_diverged` | Accepted base-only commits (`head`, `subject`) after the human's chain recovery; report them, never initiate acceptance yourself. |
| `old_tree` | Kept previous workspace path or null (open handles, late writes or uncarried content). Re-enter the live workspace by path, restart only your processes, preserve/report remaining old-tree work. |
| `deps` | State plus proposal/fix when applicable; `source_only` moved source but kept the old set. |

After a move, re-enter the workspace, rerun checks, checkpoint and publish.
`waiting_human`, `own_change` and `drift` do not clear by retrying: pass on the
reply's `deps.fix` (e.g. `ask the human: git-strata deps approve P`), proposal and
reason, then stop. This also applies to dependency state on `source_only` and
on an already-current source tree. If no fix is supplied, report the state;
do not invent a proposal id. Only transient installing/publishing waits get
30 seconds and at most five retries. The same bound applies to a reviewed
HeadMoved series or transient `auto_deferred` writes; then publish what is
allowed and report the remaining state. `install_failed` / `previous_attempt`
needs human staging cleanup/approval, not an install retry loop.

Earlier successful repos remain in `error.detail.done`. Sync reasons and related
outcomes (including legacy/internal diagnostics) are:

| Code / reason or result | Agent action |
| --- | --- |
| `Conflict sync_conflict` | Resolve every reported path with the supplied target/generation using the current report. |
| `HeadMoved sync_conflict` | Review the fresh report, re-resolve or deliberately keep with its binding; at most five retries. |
| `InvalidInput resolution_binding_required` | Run plain `sync --repo R` to obtain a report, then supply both `--target` and `--generation`; do not guess either. |
| `InvalidInput ambiguous_resolution` | Select exactly one repo with `--repo R` and use its report. |
| `InvalidInput resolved_and_keep` | Choose one flag per path; remove the duplicate choice. |
| `InvalidInput resolved_unchanged` | Edit the conflict, or deliberately use `--keep` after comparing with upstream. |
| `InvalidInput resolved_unrelated` | Select a current reported path, or a recorded workspace conflict for an acknowledgement; handle unrelated legacy paths separately when no report is saved. |
| `InvalidInput resolved_overlap_changed` | Recheck the supplied hunks and revise edits that expanded into previously clean upstream regions. |
| `NoSuchCommit main_missing` | No usable main tip exists; report the repo/ref state to the human and stop. |
| `InvalidInput` / `NoSuchRepo not_tracked` | Select a tracked workspace; if needed, create a session explicitly with `new --repo R`. Do not edit untracked siblings. |
| `NoSuchRepo not_in_session` | Run the reply's `fix` (`git-strata add --session S --repo R`). |
| `NoSuchSession no_such_session` | Use an existing session id or enter its workspace; do not retry a removed session. |
| `result: not_following` | Informational, not an error: ignore behind, only acknowledge named conflicts, then stop syncing this repo. |
| `Conflict base_diverged` | Stop; report base-only commits for the human's chain recovery. |
| `InvalidInput diverged_still_on_chain` | Stop; the old head is still reachable. Do not force or repeat acceptance. |
| `Busy auto_deferred` | Inspect `defer.reason`: `state` requires status/recovery (stop for unhealthy/inactive state); `recent_writes`, `changed_since_scan`, `changed_during_move` permit only the bounded retry described above. `defer.detail.reason: main_or_workspace_moving` is transient too. |
| `Busy sync_retry` | Preparation raced with main/workspace/dependency ownership. The daemon retries internally; if surfaced, use the same bounded retry. |
| `waiting` / `publishing`, `installing` | Transient; wait 30 seconds, at most five retries. |
| `waiting` / `waiting_human`, `own_change`, `drift` | Pass on `deps.fix`, proposal and reason to the human; stop retrying. |
| `Corrupt manifest_missing` / `sync_report` | Missing base manifest or invalid saved report; preserve the workspace and stop for human recovery. |
| `Unsupported daemon_too_old` / `sync_check_unavailable` | Ask the human to install matching CLI/daemon versions. `sync_check_unavailable` is a legacy internal diagnostic; current check dispatch should never return it. |

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
