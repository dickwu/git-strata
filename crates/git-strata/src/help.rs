//! Long `--help` text (`-h` stays the one-line `about`). Written for agents
//! that must act without asking a human. Every statement here describes the
//! current code; keep it in step with README.md.
//!
//! The confirmation gate lives in ONE place, `gate_text!`, used by the
//! top-level help and by `push`, `publish`, `rm`, `deps approve`,
//! `config relax`. Change the rule there (and in README.md "Confirmation
//! gate") when the gate changes.

/// The confirmation gate as the code implements it today.
macro_rules! gate_text {
    () => {
        "CONFIRMATION GATE (Touch ID / TTY)
  A repo needs confirmation when publish.confirm = true; that is the only gate.
  deploy_on_push is information (a push deploys) and gates nothing by itself.
  The flag used is the pinned confirm (pins.json) ORed with config.toml; a
  missing or unreadable pins.json, or a repo missing from it (new or renamed),
  counts as needs confirmation. The daemon pins such a repo with confirm on;
  a looser config.toml applies only after `git-strata config relax REPO`.

  Gated commands need a terminal on stdin; most then show a LocalAuthentication
  prompt (Touch ID, or the login password) from the daemon:
    push REPO              when REPO needs confirmation
    publish --from ...     only for batch repos with publish.push = true that need
                           confirmation and still have something to commit
    rm --discard           always (TTY + prompt)
    deps approve           always (TTY + prompt)
    config relax           always (TTY + prompt)
    resolve, chain         always need a TTY (chain --restore may also prompt)
  push REPO of a repo that needs no confirmation runs headless and, when origin
  moved under the unpushed chain (diverged), replays it itself (see push --help).
  publish into a repo with publish.push = false never asks, even when the repo
  needs confirmation; only the later `push REPO` does.

  Without a terminal: NotTty, detail {reason:\"not_tty\", command}; nothing was
  sent to the daemon. A cancelled, failed or expired prompt is NotConfirmed.
  No flag, env var or config skips the gate; faking a TTY (script, expect, a
  pty) is a bypass. Agent action on NotTty / NotConfirmed: stop and give the
  exact command to the human.

  Loosening config.toml (e.g. confirm true -> false) does not take effect by
  itself: after `daemon stop` + `daemon start` the daemon keeps the old pin and
  reports ConfigRelaxPending in `daemon status` alerts; the human then runs
  `git-strata config relax REPO`. Tightening counts on the next call (CLI) and
  is pinned at the next daemon start.
"
    };
}

/// Push guards (workspace hook and git-strata's own pinned push).
macro_rules! push_guard_text {
    () => {
        "PUSH GUARDS
  A plain `git push` in a session workspace is refused by its pre-push hook:
    git-strata: push refused in a strata workspace; publish with git-strata publish
  Do not use git push. Pushes come only from `git-strata push` or a pushing publish.

  git-strata's own push reads origin's push URL from the main checkout and pins
  it (url.<url>.insteadOf and pushInsteadOf via --config-env; GIT_CONFIG_COUNT=1
  core.hooksPath=<guard hooks>). Its guard pre-push checks the real destination,
  then runs the repo's own pre-push hook. Refusals, nothing sent:
    InvalidInput detail {remote, url, reason}, reason one of:
      url-rewritten       a url.<x>.insteadOf / pushInsteadOf rule rewrites the URL
                          (also at push time: \"would go to a URL other than the one checked\")
      url-is-remote-name  the URL is also a remote name (remote.<url>.url, or a
                          legacy <git dir>/remotes/<url> or branches/<url> file)
      empty-url, no-such-remote
  Agent action: report to the human (the fix is in the main checkout's git
  config); do not edit git config yourself.
"
    };
}

pub const TOP: &str = concat!(
    "OUTPUT
  stdout carries exactly one compact JSON line per call:
    {\"ok\":true,\"data\":{...},\"error\":null}
    {\"ok\":false,\"data\":{...},\"error\":{\"code\":\"Busy\",\"message\":\"...\",\"detail\":{\"reason\":\"daemon_down\"}}}
  All three keys are always present; data is never null (on errors it may hold
  partial results, e.g. repos that already published).
  Exit status: 0 iff ok is true, else 1. Branch on error.code, then
  error.detail.reason; never parse message. stderr has human notices only.
  --help / --version print plain text on stdout, exit 0, no JSON.
  session exec, once its command starts, prints no envelope: stdout, stderr
  and the exit status are the command's (failures before that: an envelope).
  Bad arguments: InvalidInput, detail.reason \"unknown_command\" (with
  detail.commands, and detail.group inside deps/daemon/config/session) or \"args\".

PATHS
  config     ~/Library/Application Support/git-strata/config.toml
  pins       ~/Library/Application Support/git-strata/pins.json
  socket     ~/Library/Application Support/git-strata/daemon.sock
  data root  [paths] root in config.toml (default /Volumes/GWD/inter/.strata);
             workspaces are <root>/sessions/<session-id>/<repo>/
  Only $HOME moves the first three; there is no flag or env var.

DAEMON
  Write commands go to the daemon (LaunchAgent com.gwd.git-stratad).
  Busy with detail.reason daemon_down: run `git-strata daemon start`, retry once.
  Without the daemon only these work: status, grep, files, diff, deps list,
  daemon status, session ports/exec of a workspace that already has its
  ports (and repeating a --tool-call checkpoint that already exists).

HEADLESS FLOW (no human needed)
  git-strata daemon status                       # data.running
  git-strata new PROJ-1 --repo tool --repo staff-api  # data.manifest.id, .repos.<repo>.path
  cd <data.manifest.repos.tool.path>             # edit files there
  git-strata add --repo front                    # only if another repo is needed later
  git-strata checkpoint -m \"what changed\"
  git-strata diff
  git-strata publish --from <id>:src/a.ts --from <id>:src/b.ts
  git-strata rm --session <id>

ERRORS (code -> what an agent does)
  Busy daemon_down   daemon start, retry once      Busy other       wait, retry later
  NotTty             hand the command to a human   NotConfirmed     report; human reruns
                                                   (plan_changed: rerun it yourself once)
  Conflict           fix detail.conflicts in detail.repo, rerun the same publish
  Conflict assigned  another session owns the path; do not retry
  NoMatch / no_batch checkpoint the paths, then publish
  NoSuchSession      cd into a workspace or pass --session
  NoSuchRepo not_in_session  run detail.fix (git-strata add --session S --repo R)
  InvalidInput siblings_missing  publish checks need sibling repos: run detail.fix
  AmbiguousMatch     use a session id from detail
  Dirty              rm on work that is not published: publish it, or ask the human (rm --discard)
  CheckFailed        fix the code, checkpoint, publish again
  OpenFiles          stop the process you started (detail has pids), retry
  NoCommit           nothing to do; not a failure
  HeadMoved          re-read status; do not loop
  PushRefused        report the remote's words to the human
  Unknown            outcome uncertain: do not retry, gc or rm; tell the human
  Corrupt            stop; tell the human
  PortInUse foreign  run detail.fix (session ports --reassign), then rerun
  PortInUse already_running  this workspace's server is up: reuse it

",
    gate_text!(),
    "
",
    push_guard_text!(),
    "
Per-command detail: git-strata <command> --help. Full reference: README.md.
"
);

pub const NEW: &str = "Create a session of workspaces (APFS clones of base snapshots).

--repo R (repeatable, or R1,R2): the session gets workspaces for these repos
only, all tracked. This is the fast form; agents should use it. Repos that link
to a sibling by relative path (../other) need that sibling too: pass it, or add
it later with `git-strata add --repo other`.

--repos A,B (no --repo): every repo in config.toml gets a workspace, because
sibling repos link to each other by relative path; A,B are the tracked ones.
Only tracked repos are watched, checkpointed and published. Untracked siblings
are pinned at their base; do not edit them (an edit there is never checkpointed,
and makes `rm` return Dirty with detail.untracked_repos).

The session records which AI agent created it (data.manifest.agent; status and
the GUI show it): --agent NAME, else STRATA_AGENT, else detected from the shell
(Claude Code -> claude-code, Codex -> codex, Cursor -> cursor-agent). An agent
that is none of these should `export STRATA_AGENT=<name>` (1-64 of
[A-Za-z0-9._-], starting with a letter or digit) before running `new`.";

pub const NEW_AFTER: &str = "EXAMPLES
  git-strata new PROJ-123 --repo staff-api            # one workspace
  git-strata new PROJ-123 --repo tool --repo staff-api
  git-strata new --ticket PROJ-123 --repos tool       # every repo, tool tracked
  STRATA_AGENT=aider git-strata new PROJ-123 --repo tool

DATA
  {manifest:{id, ticket?, agent?, root, repos:{<repo>:{base, head, path, tracked}}}, warnings:[]}
  warnings may hold {kind:AgentNotRecorded, agent, reason:daemon_too_old}: the
  running daemon predates agent recording; the session was created without it.
  Use data.manifest.id as SESSION and data.manifest.repos.<repo>.path as the cwd.

ERRORS
  InvalidInput repos       neither --repo nor --repos given, or an empty name
  InvalidInput repo_flags  --repo and --repos both given
  InvalidInput ticket      ticket given both positionally and with --ticket
  InvalidInput agent       --agent or STRATA_AGENT is not a valid agent name (nothing created)
  NoSuchRepo               a repo name is not in config.toml
  NoSuchBranch             a repo has no origin/<default_branch> tip (the session's repos are fetched)
  Busy daemon_down         run `git-strata daemon start`, retry once
";

pub const ADD_AFTER: &str = "Gives an existing (active) session workspaces for more repos, tracked, built
as `new` builds them (base snapshot, worktree, pin, caches, seed files).

EXAMPLES
  git-strata add --repo front                     # the session of the cwd
  git-strata add --session PROJ-123 --repo front --repo admin

A command that names a repo the session lacks fails NoSuchRepo with
detail.reason not_in_session and detail.fix: this command.

DATA {session, added:{<repo>:{base, head, path, tracked}}, already:{...}, warnings:[]}
  A repo already in the session is listed in already and left as it is.

ERRORS
  InvalidInput repos       no --repo given
  InvalidInput lifecycle   the session is not active
  NoSuchRepo               a repo is not in config.toml
  NoSuchSession            no such session (or the cwd is not in one: pass --session)
  Exists add_leftover      a leftover from an earlier failed add is in the way;
                           detail.path names it (`git-strata rm` of the session clears it)
  Any other error while building: nothing was recorded and the session is as it
  was; detail.op is \"add\" and detail.kept lists the partial directories left
  under the session root (`git-strata rm` of the session retires them).
";

pub const STATUS_AFTER: &str = "EXAMPLES
  git-strata status                  # the session of the cwd
  git-strata status --session PROJ-123
  git-strata status --all            # {sessions:[...]}

A workspace with health Unknown carries choices [\"abandon\",\"accept\"]: a move
stopped midway; that is the human's `git-strata resolve`. Works without the daemon.
--session or --repo with --all is InvalidInput both_selectors.
";

pub const CHECKPOINT_AFTER: &str = "EXAMPLES
  git-strata checkpoint -m \"parse config first\"
  git-strata checkpoint -m \"only these\" --path src/a.ts --path ../staff-api/x.php
  git-strata checkpoint --tool-call       # hook mode: hook JSON on stdin

Each path goes to the session repo whose workspace holds it; paths outside the
session are skipped (stderr). No paths: every tracked repo is scanned.
-m with nothing changed returns the latest checkpoint. --tool-call ID is
idempotent per (session, repo, ID).
DATA {checkpoints:[...]}, one per repo recorded.
ERRORS InvalidInput checkpoint_reason (give exactly one of --tool-call / -m),
NoSuchSession (cwd is not in a workspace: cd there or pass --session).
";

pub const RESTORE_AFTER: &str = "EXAMPLES
  git-strata restore 42                # whole workspace back to checkpoint 42
  git-strata restore 42 src/a.ts       # only these paths

DATA {checkpoint, session}. ERRORS NoSuchCheckpoint, NoSuchSession.
";

pub const GREP_AFTER: &str = "EXAMPLES
  git-strata grep 'TODO'                       # this session
  git-strata grep 'fn main' --all              # every live session

No hits is ok with \"hits\": []. Binary or oversized files are listed in
data.skipped. Works without the daemon. There is no separate search command:
grep searches content, files lists paths.
";

pub const FILES_AFTER: &str = "EXAMPLES
  git-strata files                     # every file of this session
  git-strata files 'src/**/*.ts' --session PROJ-123

DATA {paths:[{session, repo, path}]}. Works without the daemon.
";

pub const DIFF_AFTER: &str = "EXAMPLES
  git-strata diff                                   # this session vs its base (= --base)
  git-strata diff --session PROJ-123
  git-strata diff --between s-aaaa1111 s-bbbb2222 --repo tool

Shows current workspace content, checkpointed or not. Binary / >64 MiB entries
carry code Binary / TooLarge instead of a preview; the call stays ok.
With --session S and no --repo outside a workspace, the one repo of S with
changes or checkpoints is picked (data.repo; null when none). Several such
repos is InvalidInput reason repo with detail.candidates; pass --repo.
Works without the daemon. Two modes together is InvalidInput diff_mode.
";

pub const DEPS_AFTER: &str = "EXAMPLES
  git-strata deps propose --switch     # after changing install inputs; moves only this session
  git-strata deps list
  git-strata deps approve <ID>         # HUMAN ONLY (TTY + prompt)

propose --switch: check data.switched.kind; skipped with code OpenFiles means a
process holds files in the deps dir: stop it (only if you started it), rerun.
";

pub const PUBLISH: &str = "Publish checkpointed session content as commits on each repo's publish branch.

Before publishing, a fresh BeforePublish checkpoint of each --from batch is
taken, so the paths' current content is what gets committed. Repos commit one
by one; a repo that landed stays landed when a later repo fails.";

pub const PUBLISH_AFTER: &str = concat!(
    "EXAMPLES
  git-strata publish --from s-k3q7xm2a:src/a.ts --from s-k3q7xm2a:src
  git-strata publish --assign src/a.ts s-k3q7xm2a   # same as `assign`

Agents always pass --from SESSION:PATH (repo path, or a directory). Without
--from it is interactive on a TTY and InvalidInput no_batch otherwise.
In zsh write \"${S}:path\", not \"$S:path\" ($S:t, $S:h, ... are modifiers).

--FROM PATHS
  PATH is relative to the repo root, with no repo prefix:
  staff-api/app/x.php in session S is --from S:app/x.php. The repo is the
  workspace that has PATH (on disk or in its base). When several do, the one
  whose checkpoints in S changed PATH is taken. To name the repo outright:
    --from S:REPO:PATH      e.g. --from S:admin:src/a.ts
  The text between the first and second ':' is REPO only when it is one of
  S's repos; otherwise all after the first ':' is PATH (so a PATH that
  starts with <repo>: is written S:<repo>:<repo>:...).
  Path errors come before any TTY check and send nothing:
  InvalidInput from_syntax       no ':', empty session, not a repo path
  InvalidInput from_repo_prefix  PATH starts with a repo name; rerun with
                                 error.detail.expected
  InvalidInput from_path         no workspace has PATH (detail.repos), or
                                 S:REPO:PATH and REPO's workspace lacks it
  AmbiguousMatch from_repo       several have it and not exactly one
                                 changed it (detail.candidates, changed_in);
                                 rerun with one of detail.expected
                                 (S:REPO:PATH)

DURATION
  The reply comes after checks, commit, push and the rebase of every session.
  git push waits for the remote to close (its hooks included; bounded at
  120 s, then Unknown push_unknown); the rebase is not bounded. Silence for a
  minute or two after the remote branch moved is normal: wait for the JSON
  line, do not interrupt or rerun.

RELEASE BUILDS
  Not from a workspace (no remotes, detached HEAD, push guard). Publish and
  push, then in the main checkout: git pull --ff-only, check HEAD equals
  data.commits.<repo>, run the build there. See README.md.

DATA
  {commits:{<repo>:oid}, checks_ran, waiting:[], pushes:[{repo, outcome}],
   already_published:[{repo, publish, commit, push}]}

CONFLICT LOOP (no human needed unless reason is assigned)
  1. ok false, error.code Conflict: error.detail.repo conflicted (nothing
     committed there), error.detail.conflicts lists paths and hunks;
     data.commits / data.pushes hold repos that already landed.
  2. Fix those paths in that repo's workspace.
  3. Rerun the SAME publish command. Landed paths show up in
     data.already_published and are not committed again.
  4. Repeat while another repo conflicts.

OTHER ERRORS
  NoMatch            a --from path is not checkpointed: checkpoint, rerun
  CheckFailed        the repo's check failed in staging: fix, rerun
  PushRefused        the commit landed (data.commits) but the remote refused
  Unknown push_unknown  push outcome unknown: do not retry; tell the human
  Busy publish_unknown  an earlier publish of the repo is Unknown. It clears
                     by itself once origin has that commit (checked on each
                     publish); the human can `git-strata push REPO` first
  InvalidInput chain_rewound / chain_diverged  origin moved under an unpushed
                     chain. diverged on a repo without confirmation: run
                     `git-strata push REPO` (it replays), then rerun publish.
                     Otherwise the human runs `git-strata chain`
  NotConfirmed plan_changed  nothing written: rerun once yourself
  InvalidInput siblings_missing  the repo's checks need sibling repos
                     (check_siblings) that could not be supplied; nothing
                     written. Run detail.fix (git-strata add --session S
                     --repo R ...) or follow detail.missing[].next, then rerun
  Busy / Unknown with detail.sibling  preparing that sibling (fetch, anchor)
                     was busy or unknown; Busy: retry later; Unknown: tell
                     the human

",
    gate_text!(),
    "
",
    push_guard_text!()
);

pub const PUSH: &str = "Push a repo's publish chain to origin.

Pushes refs/strata/publish/<branch> to origin's <branch> (publish.branch).
Only run it when the human asked for a push.";

pub const PUSH_AFTER: &str = concat!(
    "EXAMPLE
  git-strata push tool

RESULT
  Pushed  -> ok, data {kind:\"Pushed\", commit, replayed, rebases, warnings}
  Refused -> PushRefused (message has the remote's words); report to the human
  unknown -> Unknown (push_unknown; a kept staging dir is in detail.scene when
             given); do not retry, gc or rm; `daemon status` lists it as UnknownPush
  nothing new -> NoCommit (not a failure)
  unknown repo -> NoSuchRepo

ORIGIN MOVED (repos that need no confirmation)
  push fetches origin first. When origin gained commits the chain does not
  have (chain_diverged), it replays the unpushed commits onto origin's tip
  (like `chain --accept-origin replay`), reruns the repo's checks on the new
  tip, moves chain and anchor, rebases sessions that sat on a replayed commit,
  and pushes once as a fast-forward. data.replayed = {from, to, onto, commits,
  checks_ran} (null when nothing was replayed). Never forced, never retried.
  Conflict replay_conflict -> nothing written, nothing pushed; detail.commit
             is the commit that did not apply onto detail.origin. Fix it in a
             session (or hand `git-strata chain` to the human)
  CheckFailed -> checks failed on the replayed tip; nothing written or pushed
  InvalidInput replay_deps_unsealed -> the deps set for the replayed tree is
             not built; open a session (`new`) on the repo, then push again
  InvalidInput chain_rewound -> origin dropped pushed commits: human only
             (`git-strata chain`); push never replays a rewound chain
  InvalidInput siblings_missing -> the replay's checks need sibling repos
             (check_siblings) that could not be supplied; nothing written or
             pushed. Follow detail.missing[].next, then push again
  A repo that needs confirmation is never replayed by push: its push sends
  only a chain that leads origin; diverged is NoCommit, the human runs chain.

",
    gate_text!(),
    "
",
    push_guard_text!()
);

pub const ASSIGN_AFTER: &str = "EXAMPLE
  git-strata assign --file src/a.ts --session s-k3q7xm2a

Decides which session resolves a conflicted path. Other sessions then get
Conflict with reason assigned for that path and must not retry.
";

pub const RM_AFTER: &str = concat!(
    "EXAMPLES
  git-strata rm --session s-k3q7xm2a              # after its work is published
  git-strata rm --session s-k3q7xm2a --discard    # HUMAN ONLY (TTY + prompt)

Dirty: the session has changes that are not published. A file whose exact
bytes and mode are in a publish of this session that origin already has does
not count, even while the session waits on a deps proposal. detail.repos lists
the rest per repo, detail.landed counts what was skipped, detail.untracked_repos
names edited untracked siblings, detail.unresolved lists interrupted scenes.
detail.reason = changed_during_remove: files changed while rm ran; retry.
Publish the work, or hand `rm --discard` to the human. Never rm after an
Unknown outcome.

",
    gate_text!()
);

pub const GC_AFTER: &str = "Removes old bases and leftovers of removed sessions, and empties
<root>/store/trash of entries older than 1 h that no session, workspace,
deps staging, move intent, alternates file or worktree link names (refused
entries stay). Skipped / refused / stuck items are printed on stderr; data is
{}. Do not run it after an Unknown outcome.

--store also runs the store-wide gc (opt-in; never automatic): rows of
sessions removed more than 30 days ago (their checkpoints can no longer be
restored), decided deps proposals, closed deps attempts, old gc records,
blobs nothing kept references, store/tmp leftovers older than 1 h, the same
trash rule as above, and a budgeted (2 GiB) blob integrity scan. Sessions a
running or Unknown publish names are kept. data is {store: report}; corrupt
blobs are listed, never deleted.
--store --dry-run changes nothing: data is {dry_run: true, preview:
{keep_secs, floor_secs, sessions[], proposals, attempts, gc_runs, blobs,
blob_bytes, trash_eligible, trash_young, trash_refused[]}}, an upper bound
of what --store would reclaim now. Run it first.
";

pub const DU_AFTER: &str = "Read-only. Answers from the daemon's size cache in about a second; every
figure has as_of (when it was measured) and stale/pending counts. Units not
fresh (missing, inode/mtime changed, or older than 10 min for tmp, sessions
and dot-dirs, 24 h for bases/deps snapshots, blob shards and trash entries)
are re-sized in the background (4 threads, utility I/O). store.db* is sized
every call. --refresh re-sizes everything in the background; poll `du` until
scan.running is false. Needs the daemon (Busy daemon_down otherwise).

FIGURES (bytes)
  logical    sum of file lengths (what `ls -l` adds up)
  allocated  blocks each file has, shared clone extents counted once per file
  private    blocks only that file owns (APFS private size, clone-aware);
             the physical cost of a unit is between private and allocated
  shared     allocated - private
  exact      false when the volume gives no private size (private = allocated)

DATA
  {root, now, total, store{all, db, blobs, tmp, other, trash{..., entries,
   eligible{entries, reclaim_min, reclaim_max}, young, floor_secs, gc}},
   bases{all, repos}, deps{all, repos}, sessions{all, list[{session, savings}]},
   savings{live, cumulative}, scan{running, started, done, queued, full,
   failed, last_full}, took_ms}
  savings.live: logical - private of live sessions. savings.cumulative: from
  sessions created/removed since `since`; earlier sessions are unknown.
  trash.eligible is what `git-strata gc` would try to delete (it may still
  refuse referenced entries).

EXAMPLES
  git-strata du             # cached figures, queue what is stale
  git-strata du --refresh   # full background recompute
";

pub const DAEMON_AFTER: &str = "INSTALL
  The per-user LaunchAgent is ~/Library/LaunchAgents/com.gwd.git-stratad.plist
  (Label com.gwd.git-stratad, ProgramArguments the git-stratad binary, RunAtLoad,
  KeepAlive, LimitLoadToSessionType Aqua, PATH ~/.bun/bin:/opt/homebrew/bin:
  /usr/local/bin:/usr/bin:/bin, StandardErrorPath ~/Library/Application
  Support/git-strata/daemon.err.log). The CLI does not write it.

EXAMPLES
  git-strata daemon status   # {running, socket, session?, holders, unknown_publishes, alerts}
  git-strata daemon start    # launchctl bootstrap gui/<uid> <plist>; {already:true} if loaded
  git-strata daemon stop     # launchctl bootout; waits up to 10 s until unloaded

The daemon reads config.toml only at start: after editing it, run
`daemon stop` then `daemon start`. start/stop use launchd, not the socket.
ERRORS InvalidInput no_plist (LaunchAgent not installed: tell the human; do
not write the plist yourself), Unknown launchctl (launchctl failed or timed out).
";

pub const RESOLVE_AFTER: &str = "HUMAN ONLY. Exactly one of --abandon / --accept (none: InvalidInput
no_choice; both: both_choices). Needs a TTY; on a non-TTY it is NotTty.
Agents: when status shows health Unknown, hand
  git-strata resolve --session S --repo R --abandon|--accept
to the human.
";

pub const CHAIN_AFTER: &str = "HUMAN ONLY. Exactly one of --accept-origin replay|drop / --restore.
Needs a TTY; --restore pushes and may also prompt. publish errors
InvalidInput chain_rewound / chain_diverged mean the human must run this,
except chain_diverged on a repo that needs no confirmation: `git-strata push
REPO` replays it headless (push --help, ORIGIN MOVED).
";

pub const CONFIG_AFTER: &str = concat!(
    "EXAMPLE (human only)
  git-strata config relax staff-api

Accepts a loosened config.toml into pins.json after the daemon reported
ConfigRelaxPending. InvalidInput not_relaxed: config is not looser than the
pin (nothing to do). NoSuchRepo: unknown repo.

",
    gate_text!()
);

pub const SESSION_AFTER: &str = "Every session workspace (not the main checkout) leases a block of 4 TCP ports
(web, connector, connector_mcp, aux) when it is created, so concurrent dev
servers and tauri-connector instances of different sessions never collide.
Committed config stays untouched: the ports reach the processes as env vars.

EXAMPLES
  git-strata session ports                      # this workspace's ports and env
  git-strata session exec -- bun run dev        # dev server on $PORT
  git-strata session exec --tauri -- bun run tauri:dev
  git-strata session ports --reassign --session S --repo R   # after PortInUse foreign
";

pub const SESSION_PORTS: &str = "A workspace's leased dev ports and the env that hands them out.

Read-only for a workspace that already has a lease (no daemon needed). A
workspace without one (created before leases) gets one from the daemon
(ports_ensure). --all lists every live workspace and never writes; unleased
ones show \"ports\":null,\"reason\":\"unleased\".";

pub const SESSION_PORTS_AFTER: &str = "OUTPUT
  data.leases[]: {session, repo, path, ports{web,connector,connector_mcp,aux},
  env{PORT, STRATA_DEV_PORT, STRATA_CONNECTOR_PORT, STRATA_CONNECTOR_MCP_PORT,
  STRATA_AUX_PORT, STRATA_SESSION, STRATA_REPO, TAURI_CONNECTOR_PROJECT,
  STRATA_TAURI_DEV_CONFIG}, tauri_app_root, check?}
  check (with --check): per role {state: free|ours|foreign|unidentified|
  probe_error, listeners[{pid, command, in_workspace}]}. Advisory only.

--reassign
  Needs exactly one session and repo. Refused with PortInUse already_running
  while a process of this workspace listens on its ports; otherwise the lease
  moves to the next free block (the old ports are not reused right away).
  The daemon moves it only if it is still the lease checked here.

ERRORS
  Busy ports_exhausted   no free block in [dev_ports] range: tell the human
                         (without --reassign: an entry \"ports\":null,
                         \"reason\":\"ports_exhausted\", not an error)
  Busy lease_changed     --reassign: another caller moved the lease since the
                         check; check again and decide afresh
  Unsupported daemon_too_old  the daemon predates leases: tell the human
  Unsupported no_policy  the daemon has no port-allocation policy installed
                         (not about [dev_ports], which is optional): tell
                         the human

EXAMPLES
  git-strata session ports --check
  git-strata session ports --all
";

pub const SESSION_EXEC: &str = "Run a command in a workspace with its dev-port env (exec: the command
replaces git-strata; its exit status is the command's).

The env (see `session ports --help`) overrides inherited values. Run from
inside the workspace the command keeps the cwd; from outside every workspace
it runs at the workspace root; from another workspace: InvalidInput
cwd_mismatch. Before running, the 4 ports are checked (--no-check skips).";

pub const SESSION_EXEC_AFTER: &str = "--tauri
  Adds --config '{\"build\":{\"devUrl\":\"http://localhost:<web>\"}}' to tauri dev,
  so the webview loads this workspace's dev server. Accepted launchers:
    tauri dev | cargo tauri dev | npx/bunx/pnpx tauri dev | pnpm exec tauri dev
      (inserted before the first bare --)
    bun run S | pnpm run S | yarn run S | npm run S
      where package.json script S is one plain `[NAME=v ...] [bunx|npx] tauri dev
      [opts]` (no quotes, $, &&, or bare --), or the bare `[bunx|npx] tauri`
      with `dev` right after S (bun run tauri dev, npm run tauri -- dev); no
      bare -- in the caller args (npm: at most one). `run` is required (without
      it the manager may run its own command S); S must not start with -.
      Refused: options
      that select another package (--prefix, --filter, -w, --workspace, --cwd,
      ...) and, for npm, any option before its --.
  Anything else: InvalidInput tauri_launcher; run without --tauri and pass
  --config \"$STRATA_TAURI_DEV_CONFIG\" to tauri dev yourself.

ERRORS (from the port check)
  PortInUse already_running  this workspace already serves on its ports
  PortInUse foreign          run detail.fix (session ports --reassign ...)
  Busy port_unidentified     retry shortly
  Busy port_probe_error      retry
  NoSuchPath                 the command was not found

EXAMPLES
  git-strata session exec -- bun run dev
  git-strata session exec --tauri -- bun run tauri:dev
  git-strata session exec --session PROJ-1 --repo front -- npm run dev
";
