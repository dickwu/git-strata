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
| `checkpoint` | exactly one of `--tool-call [ID]` (no ID: hook JSON `tool_use_id`) / `-m, --message <MSG>`; `--session <S>`; `--path <P>` (repeatable) | no | repeat of an existing `--tool-call` only | `{checkpoints:[Checkpoint,...]}` |
| `restore <CHECKPOINT> [PATHS]...` | `--session <S>` | no | no | `{checkpoint,session}` |
| `grep <PATTERN>` | regex; `--session <S>` or `--all` | no | yes | `{hits:[...],skipped?:[{session,repo,path,code}]}` |
| `files [GLOB]` | `--session <S>` or `--all` | no | yes | `{paths:[{session,repo,path}]}` |
| `diff` | one of `--base` (default) / `--session <S>` / `--between <A> <B>`; `--repo <REPO>` (required with `--between`) | no | yes | `{only_a:[],only_b:[],both:[{path,kind,preview?,code?}]}` |
| `deps propose` | `--session <S>`, `--repo <R>` (default: cwd's), `--switch` (move this session only) | no | no | `Proposal`; with `--switch`: `{ proposal, switched: { session, kind: moved\|skipped, code?, detail? } }` |
| `deps approve <ID>` | `--sessions <S,S>` (default: all sessions of the repo) | yes + confirm | no | `{results:[{session,kind:"moved"\|"skipped",code?,detail?}]}` |
| `deps list` | `--repo <R>` | no | yes | `{proposals:[...]}` |
| `publish` | `--from <S:PATH>` or `--from <S:REPO:PATH>` (repeatable; none = interactive on a TTY, else `InvalidInput` `no_batch`); `--assign <FILE> <SESSION>` | when a repo it will still commit pushes and needs confirmation. A guarded repo is exempt only when the content check proves its selected paths already landed; a session still on its old base (its rebase is `Waiting` on a deps proposal) still counts, so it can need a TTY and confirmation even though nothing new would be committed. If the daemon's own replan needs a guarded repo outside the set the CLI's TTY check covered (history changed between the plan and the publish), publish returns `NotConfirmed` with `detail.reason` `plan_changed` before prompting or writing anything (`data.already_published` still lists repos that landed earlier): rerun the same command | no | `{commits:{<repo>:oid},checks_ran,waiting:[],pushes:[{repo,outcome}],already_published:[{repo,publish,commit,push}]}`; on `Conflict`, `error.detail.repo` is the stopped repo and `data` keeps the repos that landed |
| `push <REPO>` | — | when the repo needs confirmation | no | `PushOutcome` `{kind,message}`; for a repo without confirmation also `commit`, `replayed` (`{from,to,onto,commits,checks_ran}` or null), `rebases`, `warnings`: it fetches and replays a diverged chain onto origin first (`Conflict` `replay_conflict` / `CheckFailed` / `replay_deps_unsealed`: nothing written or pushed) |
| `assign` | `--file <PATH> --session <S>` (both required) | no | no | `{}` |
| `rm` | `--session <S>` (required), `--discard` | `--discard`: yes + confirm | no | `RemoveReport` |
| `gc` | — (also empties `store/trash` entries older than 1 h that nothing names; refused/stuck on stderr) | no | no | `{}` |
| `du` | `--refresh` (re-size everything in the background; returns at once) | no | no | `{root,now,total,store{all,db,blobs,tmp,other,trash{...,entries,eligible{entries,reclaim_min,reclaim_max},young,floor_secs,gc}},bases{all,repos},deps{all,repos},sessions{all,list[{session,savings,...}]},savings{live,cumulative},scan{running,started,done,queued,full,failed,last_full},took_ms}`; every figure `{logical,allocated,private,shared,files,exact,errors,units,stale,pending,complete,as_of}` |
| `daemon start` / `daemon stop` | — | no | yes (uses launchd, not the socket) | `{}` or `{already:true}` |
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
