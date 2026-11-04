# Keeping up with main (main-sync)

Sessions follow `origin/<default_branch>`: the daemon fetches periodically and
moves idle sessions onto the new main tree (no open files, a quiet period, a
conflict-free projection; otherwise deferred, never forced). Per repo,
`[repo.<name>.sync]` in config.toml sets `follow_main`, `deps = "apply" |
"confirm"` and `fetch_secs`; those are the human's settings.

## Before you finish

```sh
git-strata sync --check        # local refs and store only; never fetches or moves
```

It returns `{session, upstream}` and exits 0 even when unclean. When
`upstream.clean` is false, look at each repo:

- `following: true`: run `cd ~ && git-strata sync --session S --repo R`, resolve any reported
  conflicts (below), re-run your checks, checkpoint, publish.
- `following: false`: ignore `behind`; only acknowledge named workspace
  conflicts (below). No sync fix is emitted, and a plain sync returns
  `result: not_following` without moving anything.

Read `data.upstream` in publish and rm replies too (publish is keyed by session
id) and apply the same rule. For a `Dirty` rm, the order for following repos is
sync, checks, checkpoint, publish, rm.

Upstream is informational: it never by itself refuses publish or rm. Behind
counts cap at 1000 (`capped` means at least that many). `ls` and
`status --brief` are cache-only; null/unchecked means not yet checked.
`sync --check` uses local refs; `as_of` describes fetch freshness.
`Unsupported daemon_too_old` on sync means the human must install matching
CLI and daemon versions.

`sync --check --hook` is the optional Stop-hook form (cache only, no
envelope); see [hooks.md](hooks.md).

## Cwd and timeout rules

Run every moving sync from outside the workspace: `cd ~ && git-strata sync
--session S --repo R`, then cd back to the workspace path. Even an up-to-date,
non-following or acknowledgement-only non-check request is refused from inside
a selected workspace. With multiple selected repos, `--keep` / `--resolved`
paths must identify exactly one repo through its saved report or workspace
conflicts; otherwise use `--repo` to resolve `ambiguous_resolution`. A cwd in
another repo does not block that resolution, including with an older daemon.
`--check` and `--check --hook` never move and can run inside.
`--allow-cwd` accepts your own shell's stale cwd; it exempts only your PID chain.
After `cd ~`, an agent's parent process can still hold a cwd inside the
workspace. The PID-chain exemption requires `--allow-cwd`; moving the child
shell out does not enable it. Exit or cd that parent out, or explicitly use
`cd ~ && git-strata sync --session S --repo R --allow-cwd` when keeping
that parent is intentional (preserve any other request flags); then re-enter
the live workspace.
Other cwd holders are refused unless `--allow-cwd-holders` is given. The same
holder override exists for `deps approve` and `deps propose --switch`; GUI
approvals skip held sessions. Fd-only holders outside dependency directories
may move with `held_old_tree`; overridden/racing cwd holders produce `cwd_stale`.
Neither flag bypasses dependency-file or mapped-file protections.

Status defaults to 30 seconds (120 with `--all`); sync defaults to 600 seconds,
a client policy that may expire during a normal cold install. `--timeout SECS`
overrides `GIT_STRATA_TIMEOUT`; zero disables the deadline. Hooks keep their
five-second process budget and ignore both. Waiting progress is stderr only,
every 15 seconds on a TTY or with `--progress`.
`Busy client_timeout` means a read or an incomplete request: back off before
retrying. `Unknown client_timeout` means the complete write may still run:
check status; do not retry, gc, or rm. Timeout does not cancel daemon work.

## Commands

| Command | Purpose |
| --- | --- |
| `git-strata sync --session S [--check] [--repo R]` | Inspect upstream, or fetch and merge it into this session. |
| `cd ~ && git-strata sync --session S --repo R --resolved PATH --target HEAD --generation N` | Finish the reported conflict after editing it. Repeat `--resolved` for each path. |
| `cd ~ && git-strata sync --session S --repo R --keep PATH --target HEAD --generation N` | Explicitly acknowledge that your current side of the reported conflict is correct. |

Main's source follows automatically in eligible idle workspaces; dependency
replacement uses the repo's effective `sync.deps` mode (`apply` or `confirm`).
To merge now, run `cd ~ && git-strata sync --session S --repo R`. When upstream says `following: false`,
ignore `behind` and only acknowledge named conflicts as below: no sync fix is
emitted, and a plain sync returns `result: not_following` without moving anything.

## Resolving a sync conflict

On `Conflict` with `detail.reason = "sync_conflict"`, read `detail.conflicts`.
Edit each listed path to contain both your changes and upstream's `theirs`.
Edit only inside the reported hunks: adjacent edits can fuse a region with a
previously clean upstream change and are refused as `resolved_overlap_changed`.
Make unrelated edits after syncing. Name **every reported conflict path in one
call**, mixing repeated `--resolved` and `--keep` as needed:

```sh
cd ~ && git-strata sync --session S --repo R --resolved PATH_A --resolved PATH_B --keep PATH_C --target HEAD --generation N
```

Use HEAD and N from `detail.target.head` and `detail.generation`; both flags
are required outside the acknowledgement cases below. Upstream's non-overlapping
hunks merge automatically. Inside a conflicting hunk your text wins, so compare
it with `theirs`; restoring your old bytes is not a resolution (`resolved_unchanged`).
Use `--keep` only after deciding explicitly that your existing side is correct.
For non-text conflicts (binary, deletion, mode/type, oversized files), your current
file or absence is taken whole and reported under `whole_file`: compare with
`theirs` first. Paths absent from the saved report are `resolved_unrelated`.
A partial submission makes no move. Its `detail.conflicts` lists only remaining
conflicts, and `detail.resolved` / `detail.kept` list choices accepted in that
request. The daemon retains their original report rows, but does **not** persist
choices for automatic reuse. In the next call, repeat all accepted names with
the same `--resolved` / `--keep` flags and unchanged accepted bytes, together with
newly resolved names and the same `--target` / `--generation`. Repeating those
choices is idempotent; omitting their names does not replay them.

### HeadMoved

`HeadMoved` supplies a fresh report and invalidates the old resolution choices.
Re-check it against the latest upstream. That report captures your already-edited
bytes: another unchanged `--resolved` is refused. Re-resolve, or use `--keep`
with the new binding after checking that your resolution remains correct. A move
to the originally reported target before the target changes also completes that
resolution.

## After a move

After `moved` or `source_only`, `cd` back to the workspace by its path and restart
processes you started there; existing shells can remain in the old tree. Re-run
checks, checkpoint, then publish. Read these per-repo reply fields:

| Field | Meaning and action |
| --- | --- |
| `lww` | Paths resolved by the configured last-writer-wins rules; review the resulting files and rerun checks. This is separate from hunk-bound `--resolved`. |
| `regenerable_kept` | Conflicting paths kept under the pinned `rm.regenerable` policy; review or regenerate them and rerun checks. |
| `accepted_diverged` | Base-only commits (`head`, `subject`) accepted after the human's chain recovery; report them to the human. Never initiate that recovery or acceptance yourself. |
| `old_tree` | Path of a retained previous workspace, or null. It can contain open handles, late writes or uncarried content. Re-enter the live workspace and restart only your processes; preserve the old tree and report any remaining work. |

## Waiting states

For `result: waiting`, inspect `deps.state` or `reason` before retrying:

- `waiting_human`, `own_change` and `drift` do **not** clear by retrying. Both
  `waiting` and `source_only` carry `deps.state` and the matching pending
  `deps.proposal` when present. Only `waiting_human` with that proposal gets
  `deps.fix: "ask the human: git-strata deps approve P"`; `own_change` and `drift`
  have no approval fix. Relay the state, proposal and supplied fix, then stop the
  sync loop. Do not invent an approval id. For `source_only`, the source move
  succeeded but the dependency action remains.
- Only transient waits (`deps.state: installing` or `reason: publishing`) get
  a 30-second wait and at most five retries. Use the same bound for repeated
  `HeadMoved` (review each fresh report) and `Busy auto_deferred` with
  `recent_writes`, `changed_since_scan` or `changed_during_move`. Agent sync uses a
  fresh `walk_activity` snapshot, not the Auto policy's quiet window. After the
  bound, publish what is allowed and report the remaining state to the human.

## Divergence

On `base_diverged`, stop and tell the human about chain recovery. Never pass
`--accept-diverged` on your own. It is accepted only after the old workspace
head is reachable from neither the publish chain nor the tracking ref.

## Legacy workspace conflicts (`move_conflict`)

Publish refuses paths still in `ws.conflicts` with `move_conflict`. If **all**
named paths are in `ws.conflicts` and **none** is in the saved sync report,
`cd ~ && git-strata sync --session S --repo R --resolved PATH` (or `--keep PATH`) acknowledges only
those paths without target/generation. This works when up to date **and when
behind or diverged**, even with a separate saved report; that report is preserved.
The per-repo reply is
`{"result":"conflicts_cleared","following":true,"resolved":[...],"kept":[...]}`.
It clears conflict markers before fetch or move preparation, without changing
base/head/generation or accepting divergence. Named conflicts in a repo with
`following: false` can likewise be acknowledged under the session queue, repo
lock and G0; that reply has `following:false`. Read and resolve each file before
naming it. Mixing legacy-only paths with report paths in a bound resolution
returns `resolved_unrelated`: use two calls, a legacy-only acknowledgement and
then a report resolution with its binding. Unnamed legacy conflicts survive
automatic and sync moves and can be acknowledged separately.

## Failed dependency install

When `upstream.deps.state` is `install_failed`, tell the human: inspect the kept
staging in `deps.error.detail.staging`, remove it, then run
`git-strata deps approve P`. Do not delete it yourself or retry the install loop.


## Reply fields

Per-repo `moved` / `source_only` replies contain `from`/`to` heads and deps,
`resolved`, `kept`, and `whole_file` (non-text resolutions take the current file
or absence whole). `deps`: both `waiting` and `source_only` carry the state and
the matching pending proposal when present; only `waiting_human` with that
proposal gets an approval fix; `source_only` moved the source but kept the old
dependency set. `install_failed` / `previous_attempt` needs the human's staging
cleanup and approval, not an install retry loop.

## Sync reasons and actions

Earlier successful repos remain in `error.detail.done`; inspect them even when
a later repo fails. The move handler's reasons and related outcomes are:

| Code / reason or result | Agent action |
| --- | --- |
| `NoSuchSession stale_cwd` | cd to `detail.workspace`, the current tree. |
| `OpenFiles caller_cwd` / `cwd_holders` | Follow the cwd rules above; no move occurred. |
| `Conflict sync_conflict` | Resolve every reported path with the supplied target/generation as above. |
| `HeadMoved sync_conflict` | Review the fresh report, re-resolve or deliberately keep with its binding; at most five retries. |
| `InvalidInput resolution_binding_required` | Run `cd ~ && git-strata sync --session S --repo R` to obtain a report, then supply both `--target` and `--generation`; do not guess either. |
| `InvalidInput ambiguous_resolution` | Select exactly one repo with `--repo R` and use its report. |
| `InvalidInput resolved_and_keep` | Choose one flag per path; remove the duplicate choice. |
| `InvalidInput resolved_unchanged` | Edit the conflict, or deliberately use `--keep` after comparing with upstream. |
| `InvalidInput resolved_unrelated` | Select a current reported path; acknowledge legacy-only workspace conflicts in a separate call, even when a saved report exists. Do not mix the two groups. |
| `InvalidInput resolved_overlap_changed` | Recheck the supplied hunks and revise edits that expanded into previously clean upstream regions. |
| `NoSuchCommit main_missing` | No usable main tip exists; report the repo/ref state to the human and stop. |
| `InvalidInput` / `NoSuchRepo not_tracked` | Select a tracked workspace; if needed, create a session explicitly with `new --repo R`. Do not edit untracked siblings. |
| `NoSuchRepo not_in_session` | Run the reply's `fix` (`git-strata add --session S --repo R`). |
| `NoSuchSession no_such_session` | Use an existing session id or enter its workspace; do not retry a removed session. |
| `result: not_following` | Informational, not an error: ignore behind, only acknowledge named conflicts, then stop syncing this repo. |
| `Conflict base_diverged` | Stop; report base-only commits for the human's chain recovery. |
| `InvalidInput diverged_still_on_chain` | Stop; the old head is still reachable. Do not force or repeat acceptance. |
| `Busy auto_deferred` | Inspect `defer.reason`: `state` requires status/recovery (stop for unhealthy/inactive state); `recent_writes`, `changed_since_scan`, `changed_during_move` permit only the bounded retry above. `defer.detail.reason: main_or_workspace_moving` is transient too. |
| `Busy sync_retry` | Preparation raced with main/workspace/dependency ownership. The daemon retries internally; if surfaced, use the same bounded retry. |
| `waiting` / `publishing`, `installing` | Transient; wait 30 seconds, at most five retries. |
| `waiting` / `waiting_human`, `own_change`, `drift` | Pass on `deps.fix`, proposal and reason to the human; stop retrying. |
| `Corrupt manifest_missing` / `sync_report` | Missing base manifest or invalid saved report; preserve the workspace and stop for human recovery. |
| `Unsupported daemon_too_old` | Ask the human to install matching CLI/daemon versions. |

An ahead local base stays parked: sync keeps result=up_to_date and adds ahead.
Once main contains it, normal follow-main resumes; a divergent main still
needs the existing human chain-recovery process. Stuck automatic rebases name
commands, PIDs, holder kind and paths. The Stop hook can ask the user for help
once per deferred row, including a clean workspace. It never authorizes
killing someone else's process. For following repos use the supplied
`cd ~ && git-strata sync --session S --repo R` command, then cd back.
