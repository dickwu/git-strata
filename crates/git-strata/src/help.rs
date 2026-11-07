//! Long `--help` text (`-h` stays the one-line `about`). Written for agents
//! that must act without asking a human. Every statement here describes the
//! current code; keep it in step with README.md.
//!
//! The confirmation gate lives in ONE place, `gate_text!`, used by the
//! `help gate` topic. Command help carries a summary and topic link. Change
//! the rule there (and in README.md "Confirmation gate") when it changes.

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
  rm.regenerable is pinned the same way: a longer or edited list is looser
  (relax); deleting positive patterns or appending `!` negations is narrower.

  Gated commands need a terminal on stdin; most then show a LocalAuthentication
  prompt (Touch ID, or the login password) from the daemon:
    push REPO              when REPO needs confirmation
    publish --from ...     only for batch repos with publish.push = true that need
                           confirmation and still have something to commit
    rm --discard           always (TTY + prompt)
    deps approve           always (TTY + prompt)
    config relax           always (TTY + prompt)
    resolve, chain         always need a TTY (chain --restore may also prompt)
  mark done / mark active ask nothing; a done session can be auto-closed later (git-strata mark --help).
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
  itself: run `git-strata config reload`; the daemon keeps the old pin and
  reports ConfigRelaxPending in `daemon status` alerts; the human then runs
  `git-strata config relax REPO`. Tightening is pinned during reload and
  fences older operations before their next lock take.
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

macro_rules! gate_brief {
    ($when:literal) => {
        concat!(
            "CONFIRMATION: ",
            $when,
            "\n",
            "  Without a TTY: NotTty; stop and give the human the exact command.\n",
            "  No flag, env or config skips the gate; faking a TTY is a bypass.\n",
            "  Full rules: git-strata help gate\n"
        )
    };
}
const OUTPUT: &str = "OUTPUT
  stdout carries exactly one compact JSON line per call:
    {\"ok\":true,\"data\":{...},\"error\":null}
    {\"ok\":false,\"data\":{...},\"error\":{\"code\":\"Busy\",\"message\":\"...\",\"detail\":{\"reason\":\"daemon_down\"}}}
  All three keys are always present; data is never null (on errors it may hold
  partial results, e.g. repos that already published).
  Exit status: 0 on success, 1 for ordinary/argument/transport/output errors.
  config reload: 2 rejected, 3 pending, 4 daemon too old. Branch on error.code, then
  error.detail.reason; never parse message. stderr has human notices only.
  Known help topics/commands, --help and --version print text, exit 0, no JSON.
  Unknown help topics return InvalidInput with detail.reason help_topic.
  session exec, once its command starts, prints no envelope: stdout, stderr
  and the exit status are the command's (failures before that: an envelope).
  Bad arguments: InvalidInput, detail.reason \"unknown_command\" (with
  detail.commands, and detail.group inside deps/daemon/config/session) or \"args\".
";
const ERRORS: &str = "ERRORS (code -> what an agent does)
  Busy daemon_down   tell the human, retry later   Busy other       wait, retry later
  Busy pins_tightened rerun with the newly applied config
  Busy reload_pending rerun config reload; its current attempt continues
  Unsupported daemon_too_old  ask the owner to upgrade/restart
  InvalidInput config_rejected  fallback if the rejected report has no reason;
                               otherwise the daemon reason is forwarded.
                               Inspect data.reasons and data.restart_required
  InvalidInput recovery_failed  new-repo pending-rebase recovery failed; inspect
                               data.reasons, resolve the failure, rerun config reload
  InvalidInput shown_required  use a CLI that sends the displayed flags
  InvalidInput config_not_applied  run config reload before config relax
  NotTty             hand the command to a human   NotConfirmed     report; human reruns
                                                   (plan_changed: rerun it yourself once)
  Conflict (publish) fix detail.conflicts in detail.repo, rerun the same publish
  Conflict sync_conflict  cd ~ && git-strata sync --session S --repo R --resolved PATH
                          (or --keep PATH) for every reported path,
                          using detail.target.head and detail.generation
                     partial: detail.conflicts has only remaining paths;
                     repeat detail.resolved/kept with the same flags and bytes,
                     plus new resolutions and the same target/generation
  Conflict assigned  another session owns the path; do not retry
  NoMatch / no_batch checkpoint the paths, then publish
  NoSuchSession      cd into a workspace or pass --session
  NoSuchRepo not_in_session  run detail.fix (git-strata add --session S --repo R)
  InvalidInput siblings_missing  publish checks need sibling repos: run detail.fix
  AmbiguousMatch     use a session id from detail
  Dirty              mark done and report unpublished work; never run rm --discard
  CheckFailed        fix the code, checkpoint, publish again
  OpenFiles caller_cwd  run detail.fix outside the workspace; it preserves request flags
  OpenFiles cwd_holders  cd holders out, or explicitly use --allow-cwd-holders
  OpenFiles (other)   stop the process you started (detail has pids), retry
  NoSuchSession stale_cwd  cd back to the live workspace path in detail.fix
  Busy client_timeout  request not dispatched or read-only; back off before retrying
  Unknown client_timeout  request sent; inspect status, never replay, gc or rm
  NoCommit           nothing to do; not a failure
  HeadMoved (sync)    read the fresh report; re-resolve with its target/generation
                     (or --keep after review); at most five retries
  HeadMoved (other)   re-read status; do not loop
  sync waiting       retry only installing/publishing waits (30 s, at most five)
                     dependency waits/source_only include deps.state and a matching
                     pending proposal when present; only waiting_human with
                     that proposal gets deps.fix. Relay the state/proposal/fix
                     and stop for waiting_human, own_change or drift
  sync not_following following:false, no upstream fix; ignore behind and only
                     acknowledge named workspace conflicts
  PushRefused        report the remote's words to the human
  Unknown            outcome uncertain: do not retry, gc or rm; tell the human
  Corrupt            stop; tell the human
  PortInUse foreign  run detail.fix (session ports --reassign), then rerun
  PortInUse already_running  this workspace's server is up: reuse it
";
const HEADLESS: &str = "HEADLESS FLOW (no human needed)
  git-strata daemon status                       # data.running
  git-strata new PROJ-1 --repo web --repo api    # data.manifest.id, .repos.<repo>.path
  cd <data.manifest.repos.web.path>              # edit files there
  git-strata add --repo front                    # only if another repo is needed later
  git-strata checkpoint -m \"what changed\"
  git-strata diff
  cd ~
  git-strata publish --from <id>:src/a.ts --from <id>:src/b.ts
  # a full publish closes the session; otherwise read data.auto_close
";
const SYNC_ACK: &str = "SYNC ACKNOWLEDGEMENT
  Legacy-only: every named path is in ws.conflicts, none in the saved report.
  cd ~ && git-strata sync --session S --repo R --resolved PATH (or --keep PATH)
  clears only those markers,
  even behind/diverged or with a separate report. No binding or move; the
  separate report remains. Non-following repos also allow named acknowledgement.
  Reply: {result:conflicts_cleared, following:true|false, resolved:[...], kept:[...]}
  Mixing legacy-only and report paths in a bound call is resolved_unrelated;
  use two calls. Partial conflict choices are not saved for automatic reuse.
";

pub const TOP: &str = "OUTPUT
  One compact JSON envelope on stdout; human notices on stderr. Exit 0 on
  success, 1 on error. Full contract: git-strata help output

PATHS
  Config/pins/socket: ~/Library/Application Support/git-strata/
  Only $HOME moves them; no flag or other env var.
  Data root: [paths] root in config.toml (default ~/.strata).
  Workspaces: <root>/sessions/<session-id>/<repo>/

DAEMON
  Busy daemon_down: tell the human; retry after they start the daemon.
  Agents never start/stop it. Without it: status, ls, grep, files, diff,
  deps list, daemon status; repeated checkpoint --tool-call ID; leased session ports/exec.

GATES
  push/pushing publish, rm --discard, deps approve, config relax, resolve/chain:
  TTY/confirmation rules: git-strata help gate. No flag, env or config skips
  the gate; faking a TTY is a bypass.
  Without a TTY: NotTty; stop and give the human the exact command.

HEADLESS FLOW
  new --repo R; edit; checkpoint -m TEXT; diff; publish --from S:PATH.
  A full publish closes the session; otherwise read data.auto_close, which says what is left. Full example: git-strata help headless
  Sync from outside: cd ~ && git-strata sync --session S; then cd back.
  Do not use git push. Full rules: git-strata help push-guards

ERRORS
  Busy: back off. NotTty/NotConfirmed: give the command to the human.
  Conflict: resolve reported paths. Dirty: mark done and report unpublished work.
  OpenFiles caller_cwd/cwd_holders: cd out or follow detail.fix.
  NoSuchSession stale_cwd: cd back to the live workspace path.
  client_timeout: Busy means read/unsent; Unknown means sent, inspect status.
  Unknown: do not retry, gc, or rm; inspect status and tell the human.
  Corrupt: stop and tell the human. Full table: git-strata help errors

TOPICS
  git-strata help gate | push-guards | errors | errors-publish | output | headless
  git-strata help sync-ack; git-strata help <command...>
";

// Base safety lives in the publish error topic to keep command help bounded.
macro_rules! publish_base_text {
    () => { "BASE SAFETY
  A batch's workspace base must be in the publish target B or on main_tip.
  base_not_on_main: ask the human to push/recover the base commits first;
  follow detail.fix. Dependency approval never drops parked base commits.
  HeadMoved baseline_moved: run the literal publish command in detail.fix
  to take fresh checkpoints after a baseline move.

" };
}

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
  git-strata new PROJ-123 --repo api                  # one workspace
  git-strata new PROJ-123 --repo web --repo api
  git-strata new --ticket PROJ-123 --repos tool       # every repo, tool tracked
  STRATA_AGENT=aider git-strata new PROJ-123 --repo web

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
  Busy daemon_down         tell the human (they run `git-strata daemon start`)
";

pub const ADD_AFTER: &str =
    "Gives an existing (active) session workspaces for more repos, tracked, built
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

pub const SYNC_AFTER: &str = "Run moving sync outside the workspace: cd ~ && git-strata sync --session S.
Then cd back to the workspace path. --check and --hook may run inside.
Caller cwd is refused even when up to date, non-following or acknowledging
conflicts. --allow-cwd accepts your own stale cwd; --allow-cwd-holders accepts
others. Neither bypasses dependency-file or mapped-file protections.

Default total deadline: 600 s (client policy; a cold install can take longer).
--timeout SECS overrides GIT_STRATA_TIMEOUT; 0 means unlimited. --hook keeps
its five-second budget and ignores both. --progress writes stderr notes.
Unknown client_timeout: the write may still run; inspect status and do not
retry, gc, or rm. Busy client_timeout: nothing changed; back off before retry.
";

pub const STATUS_AFTER: &str = "Default total deadline: 30 s, or 120 s with --all. --timeout SECS overrides
GIT_STRATA_TIMEOUT; 0 means unlimited. Busy client_timeout: back off before
retrying; it never triggers a local fallback. --progress writes stderr notes.

EXAMPLES
  git-strata status                  # the session of the cwd
  git-strata status --session PROJ-123
  git-strata status --all            # {sessions:[...]}
  git-strata status --brief          # store only: no drift/seeds/mirrors/old trees

A workspace with health Unknown carries choices [\"abandon\",\"accept\"]: a move
stopped midway; that is the human's `git-strata resolve`. Works without the daemon.
--session or --repo with --all is InvalidInput both_selectors.
";

pub const LS_AFTER: &str = "EXAMPLES
  git-strata ls                      # {sessions:[{id,ticket,state,created,repos,phase?}]}
  git-strata ls --table              # ID TICKET STATE MARK REPOS CREATED

From the store alone, sorted by id; never stats a workspace or runs git.
Without the daemon (or with one that predates it) the store is read
directly and data.source is \"local\". With --table a success prints only
the table; an error is still the JSON envelope.
";

pub const CHECKPOINT_AFTER: &str = "EXAMPLES
  git-strata checkpoint -m \"parse config first\"
  git-strata checkpoint -m \"only these\" --path src/a.ts --path ../api/x.php
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
  git-strata diff --between s-aaaa1111 s-bbbb2222 --repo web

Shows current workspace content, checkpointed or not. Binary / >64 MiB entries
carry code Binary / TooLarge instead of a preview; the call stays ok.
With --session S and no --repo outside a workspace, the one repo of S with
changes or checkpoints is picked (data.repo; null when none). Several such
repos is InvalidInput reason repo with detail.candidates; pass --repo.
Works without the daemon. Two modes together is InvalidInput diff_mode.
";

pub const DEPS_AFTER: &str = concat!(
    "EXAMPLES
  cd ~ && git-strata deps propose --session S --repo R --switch
  git-strata deps list
  git-strata deps approve <ID>         # HUMAN ONLY (TTY + prompt)

propose --switch moves only this session after changing install inputs. Run
outside its workspace, then cd back to the workspace path after moving. From
inside, --allow-cwd accepts your own PID chain's stale cwd; cd back afterward.
Check data.switched.kind. For skipped with code OpenFiles, inspect detail.reason:
  cwd_holders: your own shell/parent needs --allow-cwd; other holders must cd
    out, or --allow-cwd-holders accepts their stale cwd too.
  Other OpenFiles: a process holds deps files; stop it only if you started it,
    then rerun. Neither cwd flag bypasses file or mapping protections.\n",
    gate_brief!("deps approve always needs a TTY + Touch ID; propose does not.")
);

pub const PUBLISH: &str =
    "Publish checkpointed session content as commits on each repo's publish branch.

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
  api/app/x.php in session S is --from S:app/x.php. The repo is the
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
  {commits:{<repo>:oid}, message_source:{<repo>:flag|checkpoints|legacy}, checks_ran, waiting:[], pushes:[{repo, outcome}],
   already_published:[{repo, publish, commit, push}], closed, auto_close:{closed, reasons[], sessions}}

CONFLICT LOOP (no human needed unless reason is assigned)
  1. ok false, error.code Conflict: error.detail.repo conflicted (nothing
     committed there), error.detail.conflicts lists paths and hunks;
     data.commits / data.pushes hold repos that already landed.
  2. Fix those paths in that repo's workspace.
  3. Rerun the SAME publish command. Landed paths show up in
     data.already_published and are not committed again.
  4. Repeat while another repo conflicts.

Other errors: git-strata help errors-publish

",
    gate_brief!(
        "batch repos with publish.push = true that need confirmation and have work to commit (TTY + Touch ID)."
    ),
    "\nDo not use git push. Full rules: git-strata help push-guards\n"
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
    gate_brief!("when this repo needs confirmation (TTY + Touch ID)."),
    "\nDo not use git push. Full rules: git-strata help push-guards\n"
);

pub const ASSIGN_AFTER: &str = "EXAMPLE
  git-strata assign --file src/a.ts --session s-k3q7xm2a

Decides which session resolves a conflicted path. Other sessions then get
Conflict with reason assigned for that path and must not retry.
";

pub const MARK_AFTER: &str = "EXAMPLES
  git-strata mark --session s-k3q7xm2a done --note \"what landed and what is left\"
  git-strata mark --session s-k3q7xm2a active

A full publish closes its session by itself. When it stays open, the reply says
why. A done session, marked by anyone, is closed by the daemon on a later publish
or push of its repos once everything has landed, unless one of its repos has
publish.auto_close = false: its workspace goes to the trash (recoverable until
trash gc) and its checkpoints stay. To keep a session, mark it active: that clears
the mark, note and attribution. The mark command itself changes no files,
checkpoints or lifecycle. Only Active/Unknown sessions may be marked. No TTY or confirmation.
Requires the daemon feature session.mark; an older daemon returns Unsupported
with reason daemon_too_old. Notes are 1–2000 UTF-8 bytes; tab and newline are
allowed, other characters below 0x20 are refused. Active takes no note.
--agent uses the same detection as new; P1 stores setter kind only.
";

pub const RM_AFTER: &str = concat!(
    "EXAMPLES
  git-strata mark --session s-k3q7xm2a done      # the daemon's fallback after a publish
  git-strata rm --session s-k3q7xm2a              # human removal / existing automation
  git-strata rm --session s-k3q7xm2a --discard    # HUMAN ONLY (TTY + prompt)

Agents finish with publish: a fully landed session closes itself. Publish from `cd ~`.
Read `data.auto_close.sessions[<your session id>]`
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
reasons are in this reply only: act on them now. Never `rm --discard`.
Resume work with mark active. A Dirty refusal means mark done and report what
remains; never run rm --discard or seek a bypass.

Dirty: rm refused and nothing was removed; the session is still Active and
listed by `git-strata ls` (detail.removed = false, detail.lifecycle). The
message names up to 5 unpublished paths per repo. A file whose exact bytes and
mode are in a publish of this session that origin already has does not count,
even while the session waits on a deps proposal. detail.repos lists the rest
per repo, detail.landed counts what was skipped, detail.untracked_repos names
edited untracked siblings, detail.unresolved lists interrupted scenes.
detail.reason = changed_during_remove: files changed while rm ran; retry.
Paths matching the repo's pinned rm.regenerable (Modified/Added/Untracked, not
install inputs, tracked healthy repos only) do not block: they are
checkpointed and listed in data.regenerable (detail.regenerable on Dirty).
Publish the work, or hand `rm --discard` to the human. Never rm after an
Unknown outcome. `git-strata status <sid>` shows the last rm outcome.

Ctrl-C before the request is sent: nothing happened. After it is sent: the
daemon finishes the rm on its own (it may still be refused); check
`git-strata status <sid>`.

",
    gate_brief!("rm --discard always needs a TTY + Touch ID.")
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

pub const DU_AFTER: &str =
    "Read-only. Answers from the daemon's size cache in about a second; every
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

pub const DAEMON_AFTER: &str =
    "start/stop: HUMAN ONLY. Agents never run them (or launchctl); on daemon_down
they tell the human. Both act only when HOME is the login user's home (D38).

INSTALL
  The per-user LaunchAgent is ~/Library/LaunchAgents/com.gwd.git-stratad.plist
  (Label com.gwd.git-stratad, ProgramArguments the git-stratad binary, RunAtLoad,
  KeepAlive, LimitLoadToSessionType Aqua, PATH ~/.bun/bin:/opt/homebrew/bin:
  /usr/local/bin:/usr/bin:/bin, StandardErrorPath ~/Library/Application
  Support/git-strata/daemon.err.log). The CLI does not write it; the human
  can print it with `git-stratad --print-plist [ABSOLUTE_BIN]`.

EXAMPLES
  git-strata daemon status   # {running, socket, session?, holders, unknown_publishes,
                            #  alerts, alerts_at, pins_meta, config?:{generation,hash,loaded_at,last_attempt,
                            #                   restart_required}}
  git-strata daemon start    # launchctl bootstrap gui/<uid> <plist>; {already:true} if loaded
  git-strata daemon stop     # launchctl bootout; waits up to 10 s until unloaded

config is copied when the daemon returns it; absent when down or omitted by an
older daemon. generation/hash/loaded_at describe the active catalog. last_attempt
is null before any reload attempt, then the report plus at and a rejection reason
when present; restart_required lists restart-only fields from that attempt.

After editing config.toml, run `git-strata config reload` and check data.repos.
Restart-only changes and old daemons require an owner restart. start/stop use
launchd, not the socket.
ERRORS InvalidInput no_plist (LaunchAgent not installed: tell the human; do
not write the plist yourself), InvalidInput foreign_home (HOME is not the login
home; nothing done), InvalidInput foreign_agent (the loaded agent comes from
another plist, e.g. one bootstrapped from a temp HOME; nothing done, the
message names the `launchctl bootout` the human can run), Unknown launchctl
(launchctl failed or timed out).
";

pub const RESOLVE_AFTER: &str = concat!(
    "HUMAN ONLY. Exactly one of --abandon / --accept (none: InvalidInput
no_choice; both: both_choices). Needs a TTY; on a non-TTY it is NotTty.
Agents: when status shows health Unknown, hand
  git-strata resolve --session S --repo R --abandon|--accept
to the human.\n",
    gate_brief!("resolve always needs a TTY.")
);

pub const CHAIN_AFTER: &str = concat!(
    "HUMAN ONLY. Exactly one of --accept-origin replay|drop / --restore.
Needs a TTY; --restore pushes and may also prompt. publish errors
InvalidInput chain_rewound / chain_diverged mean the human must run this,
except chain_diverged on a repo that needs no confirmation: `git-strata push
REPO` replays it headless (push --help, ORIGIN MOVED).\n",
    gate_brief!("chain always needs a TTY; restore may also prompt.")
);

pub const CONFIG_AFTER: &str = concat!(
    "EXAMPLE (human only)
  git-strata config relax api

Accepts a loosened config.toml into pins.json after the daemon reported
ConfigRelaxPending. InvalidInput not_relaxed: config is not looser than the
pin (nothing to do). NoSuchRepo: unknown repo.

",
    gate_brief!("config relax always needs a TTY + Touch ID.")
);

pub const SESSION_AFTER: &str =
    "Every session workspace (not the main checkout) leases a block of 4 TCP ports
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

pub const SESSION_EXEC: &str =
    "Run a command in a workspace with its dev-port env (exec: the command
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

const ERRORS_PUBLISH: &str = concat!(publish_base_text!(), "OTHER ERRORS
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
");
const TOPICS: &[(&str, &str)] = &[
    ("gate", gate_text!()),
    ("push-guards", push_guard_text!()),
    ("errors", ERRORS),
    ("errors-publish", ERRORS_PUBLISH),
    ("output", OUTPUT),
    ("headless", HEADLESS),
    ("sync-ack", SYNC_ACK),
];

pub fn render(words: &[String]) -> crate::Result<String> {
    use clap::CommandFactory;
    let mut command = crate::args::Cli::command();
    command.build();
    if words.is_empty() {
        return Ok(format!(
            "Usage: git-strata help [TOPIC | COMMAND ...]\n\nTopics: {}\nCommands: {}\n\n<command> --help describes the installed version.\n",
            TOPICS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", "),
            crate::args::top_commands().join(", ")
        ));
    }
    if words.len() == 1
        && let Some((_, text)) = TOPICS.iter().find(|(name, _)| *name == words[0])
    {
        return Ok((*text).to_string());
    }
    for word in words {
        let Some(next) = command.find_subcommand(word).cloned() else {
            return Err(strata_types::StrataError::new(
                strata_types::Code::InvalidInput,
                format!("unknown help topic or command: {}", words.join(" ")),
            )
            .with_detail(
                serde_json::json!({"reason":"help_topic", "topics":TOPICS.iter().map(|(n,_)|n).collect::<Vec<_>>(),
                    "commands":command.get_subcommands().map(|c|c.get_name()).collect::<Vec<_>>() }),
            ));
        };
        command = next;
    }
    // Clap chooses short layout for commands with no long-only text. Use
    // its --help selection as well as its built command path for byte parity.
    let mut argv = vec!["git-strata".to_string()];
    argv.extend_from_slice(words);
    argv.push("--help".to_string());
    match crate::args::Cli::command().try_get_matches_from(argv) {
        Err(e) => Ok(e.to_string()),
        Ok(_) => Ok(command.render_long_help().to_string()),
    }
}
pub const CONFIG_RELOAD_AFTER: &str = "After editing config.toml, run git-strata config reload. The ordinary JSON envelope's data contains outcome, generation, repos, added, changed, restart_required, pending_relax and reasons. Check membership in repos before using a new repo. No TTY or confirmation is needed. Exit 0: applied/unchanged; 1: argument/transport/output or invalid-report failure; 2: rejected (the daemon reason, or config_rejected); 3: reload_pending (rerun); 4: daemon_too_old. Parse and validation failures preserve data.reasons; unknown outcomes preserve data with invalid_reload_report. Tightenings fence older work; relaxations still require config relax. Repo removal, paths.root, status.parallel, and existing repo identity/exclusion changes require an owner restart.";
