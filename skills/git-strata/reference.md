# git-strata command reference

Taken from the built binary's `--help` and `docs/modules/cli.md` §3.
"Human" = needs a TTY (and, where marked, a LocalAuthentication prompt);
an agent must hand these to the human. "No daemon" = works when the daemon
is not running.

| Command | Arguments and flags | Human | No daemon | `data` on success |
| --- | --- | --- | --- | --- |
| `new [TICKET]` | `--ticket <T>` (same as positional), `--repos <A,B>` (required; the tracked repos; every configured repo still gets a workspace) | no | no | `{manifest:{id,ticket?,root,repos:{<repo>:{base,head,path,tracked}}},warnings:[]}` |
| `status` | `--session <S>` or `--all` (default: cwd's session) | no | yes | `SessionStatus`; `--all`: `{sessions:[...]}` |
| `checkpoint` | exactly one of `--tool-call [ID]` (no ID: hook JSON `tool_use_id`) / `-m, --message <MSG>`; `--session <S>`; `--path <P>` (repeatable) | no | repeat of an existing `--tool-call` only | `{checkpoints:[Checkpoint,...]}` |
| `restore <CHECKPOINT> [PATHS]...` | `--session <S>` | no | no | `{checkpoint,session}` |
| `grep <PATTERN>` | regex; `--session <S>` or `--all` | no | yes | `{hits:[...],skipped?:[{session,repo,path,code}]}` |
| `files [GLOB]` | `--session <S>` or `--all` | no | yes | `{paths:[{session,repo,path}]}` |
| `diff` | one of `--base` (default) / `--session <S>` / `--between <A> <B>`; `--repo <REPO>` (required with `--between`) | no | yes | `{only_a:[],only_b:[],both:[{path,kind,preview?,code?}]}` |
| `deps propose` | `--session <S>`, `--repo <R>` (default: cwd's), `--switch` (move this session only) | no | no | `Proposal`; with `--switch`: `{ proposal, switched: { session, kind: moved\|skipped, code?, detail? } }` |
| `deps approve <ID>` | `--sessions <S,S>` (default: all sessions of the repo) | yes + confirm | no | `{results:[{session,kind:"moved"\|"skipped",code?,detail?}]}` |
| `deps list` | `--repo <R>` | no | yes | `{proposals:[...]}` |
| `publish` | `--from <S:PATH>` (repeatable; none = interactive on a TTY, else `InvalidInput` `no_batch`); `--assign <FILE> <SESSION>` | when a repo it will still commit pushes and needs confirmation. A guarded repo is exempt only when the content check proves its selected paths already landed; a session still on its old base (its rebase is `Waiting` on a deps proposal) still counts, so it can need a TTY and confirmation even though nothing new would be committed. If the daemon's own replan needs a guarded repo outside the set the CLI's TTY check covered (history changed between the plan and the publish), publish returns `NotConfirmed` with `detail.reason` `plan_changed` before prompting or writing anything (`data.already_published` still lists repos that landed earlier): rerun the same command | no | `{commits:{<repo>:oid},checks_ran,waiting:[],pushes:[{repo,outcome}],already_published:[{repo,publish,commit,push}]}`; on `Conflict`, `error.detail.repo` is the stopped repo and `data` keeps the repos that landed |
| `push <REPO>` | — | when the repo needs confirmation | no | `PushOutcome` `{kind,message}`; for a repo without confirmation also `commit`, `replayed` (`{from,to,onto,commits,checks_ran}` or null), `rebases`, `warnings`: it fetches and replays a diverged chain onto origin first (`Conflict` `replay_conflict` / `CheckFailed` / `replay_deps_unsealed`: nothing written or pushed) |
| `assign` | `--file <PATH> --session <S>` (both required) | no | no | `{}` |
| `rm` | `--session <S>` (required), `--discard` | `--discard`: yes + confirm | no | `RemoveReport` |
| `gc` | — | no | no | `{}` |
| `daemon start` / `daemon stop` | — | no | yes (uses launchd, not the socket) | `{}` or `{already:true}` |
| `daemon status` | — | no | yes | `{running,socket,session?,holders:[],unknown_publishes:[],alerts:[]}` |
| `resolve` | `--session <S> --repo <R>` + exactly one of `--abandon` / `--accept` | yes | no | `ResolutionReport` |
| `chain` | `--repo <R>` + exactly one of `--accept-origin <replay\|drop>` / `--restore` | yes (`--restore` may also confirm) | no | `{repo,choice,outcome?\|refs_moved?}` |
| `config relax <REPO>` | — | yes + confirm | no | — |

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
`rpc_failed`, `readonly_client`. From publish: `assigned`, `chain_rewound`,
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
CheckMutates, PushRefused, Unknown, Unsupported, Corrupt.
