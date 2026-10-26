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
  The flag used is the pinned confirm (pins.json) ORed with config.toml; an
  unreadable pins.json, or a repo missing from it, counts as needs confirmation.

  Gated commands need a terminal on stdin; most then show a LocalAuthentication
  prompt (Touch ID, or the login password) from the daemon:
    push REPO              when REPO needs confirmation
    publish --from ...     only for batch repos with publish.push = true that need
                           confirmation and still have something to commit
    rm --discard           always (TTY + prompt)
    deps approve           always (TTY + prompt)
    config relax           always (TTY + prompt)
    resolve, chain         always need a TTY (chain --restore may also prompt)
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
  Bad arguments: InvalidInput, detail.reason \"unknown_command\" (with
  detail.commands, and detail.group inside deps/daemon/config) or \"args\".

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
  daemon status (and repeating a --tool-call checkpoint that already exists).

HEADLESS FLOW (no human needed)
  git-strata daemon status                       # data.running
  git-strata new PROJ-1 --repos tool,staff-api   # data.manifest.id, .repos.<repo>.path
  cd <data.manifest.repos.tool.path>             # edit files there
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
  AmbiguousMatch     use a session id from detail
  Dirty              rm on unpublished work: publish it, or ask the human (rm --discard)
  CheckFailed        fix the code, checkpoint, publish again
  OpenFiles          stop the process you started (detail has pids), retry
  NoCommit           nothing to do; not a failure
  HeadMoved          re-read status; do not loop
  PushRefused        report the remote's words to the human
  Unknown            outcome uncertain: do not retry, gc or rm; tell the human
  Corrupt            stop; tell the human

",
    gate_text!(),
    "
",
    push_guard_text!(),
    "
Per-command detail: git-strata <command> --help. Full reference: README.md.
"
);

pub const NEW: &str = "Create a session: one workspace per configured repo.

Every repo in config.toml gets a workspace (an APFS clone of its base snapshot),
because sibling repos link to each other by relative path. --repos chooses which
repos are tracked: only tracked repos are watched, checkpointed and published.
Untracked siblings are pinned at their base; do not edit them (an edit there is
never checkpointed, and makes `rm` return Dirty with detail.untracked_repos).";

pub const NEW_AFTER: &str = "EXAMPLES
  git-strata new PROJ-123 --repos tool,staff-api
  git-strata new --ticket PROJ-123 --repos tool

DATA
  {manifest:{id, ticket?, root, repos:{<repo>:{base, head, path, tracked}}}, warnings:[]}
  Use data.manifest.id as SESSION and data.manifest.repos.<repo>.path as the cwd.

ERRORS
  InvalidInput repos   --repos missing or empty (it is required)
  InvalidInput ticket  ticket given both positionally and with --ticket
  NoSuchRepo           a --repos name is not in config.toml
  NoSuchBranch         a repo has no origin/<default_branch> tip (every repo is fetched)
  Busy daemon_down     run `git-strata daemon start`, retry once
";

pub const STATUS_AFTER: &str = "EXAMPLES
  git-strata status                  # the session of the cwd
  git-strata status --session PROJ-123
  git-strata status --all            # {sessions:[...]}

A workspace with health Unknown carries choices [\"abandon\",\"accept\"]: a move
stopped midway; that is the human's `git-strata resolve`. Works without the daemon.
--session with --all is InvalidInput both_selectors.
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
  workspace that has PATH (on disk or in its base). Path errors come before
  any TTY check and send nothing:
  InvalidInput from_syntax       no ':', empty session, not a repo path
  InvalidInput from_repo_prefix  PATH starts with a repo name; rerun with
                                 error.detail.expected
  InvalidInput from_path         no workspace has PATH (detail.repos)
  AmbiguousMatch from_repo       several do (detail.candidates)

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
  InvalidInput chain_rewound / chain_diverged  human runs `git-strata chain`
  NotConfirmed plan_changed  nothing written: rerun once yourself

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
  Pushed  -> ok, data {kind:\"Pushed\", message}
  Refused -> PushRefused (message has the remote's words); report to the human
  unknown -> Unknown (push_unknown; a kept staging dir is in detail.scene when
             given); do not retry, gc or rm; `daemon status` lists it as UnknownPush
  nothing new -> NoCommit (not a failure)
  unknown repo -> NoSuchRepo

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

Dirty: the session still has changes against its base. detail.repos lists
paths per repo, detail.untracked_repos names edited untracked siblings,
detail.unresolved lists interrupted scenes. Publish the work, or hand
`rm --discard` to the human. Never rm after an Unknown outcome.

",
    gate_text!()
);

pub const GC_AFTER: &str = "Removes old bases and leftovers of removed sessions. Skipped items are
printed on stderr; data is {}. Do not run it after an Unknown outcome.
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
InvalidInput chain_rewound / chain_diverged mean the human must run this.
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
