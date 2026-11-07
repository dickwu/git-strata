# Publishing, dependencies and removing a session

## Publish

A publish makes one commit per participating repo on its configured
`publish.branch`, possibly combining multiple sessions. Workspaces stay in the
checkpoint → publish workflow. Run publishing commands from outside using
explicit `--from` session ids. B is the staging baseline (the leading publish
chain when applicable, not necessarily origin). A parked `new --base` session
cannot publish until its base-only commits are on B or main. If a workspace
moves between checkpoint and publish, `HeadMoved baseline_moved` asks you to
rerun the literal `publish --from …` command; equal baseline trees are allowed
even when their commit ids differ.


- `publish` publishes the selected paths' current content: before sending
  the publish it records a fresh `BeforePublish` checkpoint of each
  `--from` batch, so edits made to a selected file up to that moment are
  included. There is no option to publish an older checkpoint; to keep an
  edit out, revert it (or `restore` the path) before publishing.
  Without `--from` it is interactive on a TTY and `InvalidInput`
  (`no_batch`) otherwise, so agents always pass `--from SESSION:PATH`.
  PATH is relative to the repo root with no repo prefix (`billing-api/app/x.php`
  is `--from "${S}:app/x.php"`). Write `"${S}:path"`, never `"$S:path"`: zsh
  reads `$S:t`, `$S:h`, ... as modifiers. Path mistakes come back before any
  TTY check: `InvalidInput` `from_repo_prefix` (rerun with
  `error.detail.expected`), `InvalidInput` `from_path` (no workspace has it),
  `AmbiguousMatch` `from_repo` (several have it and not exactly one of their
  checkpoints changed it; rerun with one of `detail.expected`). To pick the
  repo yourself use `--from "${S}:REPO:PATH"` (e.g. `"${S}:admin:src/a.ts"`);
  the middle part is REPO only when it is one of the session's repos.
- `publish` replies only after checks, commit, push and the rebase of every
  session. `git push` waits for the remote's own hooks (bounded at 120 s,
  then `Unknown` `push_unknown`), the rebase is unbounded: a minute or two of
  silence after the remote branch moved is normal. Wait; do not interrupt or
  rerun.
- Checks run with `TMPDIR`, `TMP` and `TEMP` set to a fresh, private
  `<layout root>/+tmp` that is removed with the staging layout. A tool cache
  that defaults to `$TMPDIR` (phpstan without `tmpDir`) is therefore cold on
  every publish and never shared with other sessions; do not point a check at
  a shared temp dir to make it warm. `XDG_CACHE_HOME` and the rest of the
  environment are unchanged, apart from the `GIT_*` variables checks never
  get. The path is long (about 80 bytes under `~/.strata`), so a check that
  binds a Unix socket in `$TMPDIR` has little room (macOS limit 104 bytes).
- Multi-repo `publish` commits repo by repo. When one repo hits `Conflict`,
  repos before it have already committed (and pushed, if push is on); the
  call does not roll them back. See "Publish conflict: fix and re-run" below.

## Publish conflict: fix and re-run

This is the expected loop for agents; the human is not needed unless the
conflict is `assigned`.

1. `publish` returns `ok: false`, `error.code = "Conflict"`.
   - `error.detail.repo` is the repo that conflicted; nothing was committed
     there.
   - `error.detail.conflicts` lists its paths (with the hunks).
   - `data.commits` / `data.pushes` hold repos that already landed in this
     call (also in `error.detail.published_before`). They stay landed.
     The same holds for any publish error: a repo whose commit was written
     before its push timed out (`Unknown`) or was refused (`PushRefused`) is
     in `data.commits` and `data.pushes` too.
2. Fix the listed paths in that repo's workspace (merge in the upstream
   change; `git-strata diff` helps).
3. Optionally `git-strata checkpoint -m "resolve conflict" --path P ...`
   to keep a named point; the re-run snapshots the paths itself.
4. Re-run the exact same `publish --from ...` command. Repos that already
   landed are not committed again: they appear in
   `data.already_published` as `{repo, publish, commit, push}`, and only the
   rest is published (`data.commits`). The check is per `--from` path and by
   content: a path (or directory) whose content is exactly what landed is
   skipped; anything new or changed under it since (including new files in
   a directory you selected) is published as a new commit, and the unchanged
   paths of the same `--from` list do not cause `NoMatch`.
   A landed repo whose push was refused (`push.kind` `Refused`) makes the
   re-run `ok: false` `PushRefused` with `detail.fix` `git-strata push REPO`:
   hand that to the human; do not loop on `publish`. If the commit has been
   pushed since, the entry reads `Pushed` with `landed_later: true`. A re-run
   reply carries `data.closed` and `data.auto_close` like a first run's (see
   "Removing a session").
5. Repeat while another repo conflicts.

Between sessions, who resolves a conflicted path can be decided with
`git-strata assign --file PATH --session S` (or `publish --assign FILE
SESSION`); other sessions then get `Conflict` `assigned` for it and must not
retry.

## Confirmation gate

A repo needs confirmation when `publish.confirm` is true (the pinned flag in
`pins.json` ORed with `config.toml`); `deploy_on_push` is information only (a
repo with `confirm = false` publishes and pushes headless even when a push
deploys). A gated `push`, or a `publish` that pushes to a gated repo, needs
the human at a TTY: print the exact command and stop. A publish into a repo
with `publish.push = false` is never gated; only its later `push` is.
After loosening the config, run `git-strata config reload`, then give the human
`git-strata config relax REPO` (`daemon status` alerts `ConfigRelaxPending`
until confirmation). Supported changes need no restart. Full
rule: the README's [Confirmation gate](https://github.com/dickwu/git-strata/blob/main/README.md#confirmation-gate-touch-id--tty).

## Dependencies

- `deps propose [--session S] [--repo R]` proposes a new dependency set after
  you change install inputs; `deps list` shows open proposals. Add `--switch`
  to move your own session onto the proposed set right away (no prompt; other
  sessions stay pinned). Run `cd ~ && git-strata deps propose --session S --repo R --switch`,
  then cd back to the workspace path after moving. From inside, add `--allow-cwd`
  to accept your own PID chain's stale cwd, then cd back afterward.
  Check `data.switched.kind`: for `skipped` with `code: OpenFiles`, inspect
  `data.switched.detail.reason`. `cwd_holders` means your own shell or parent
  needs `--allow-cwd`; other holders must cd out, or `--allow-cwd-holders`
  explicitly accepts their stale cwd too. Other `OpenFiles` means a process
  holds deps files: stop it only if you started it, then rerun. Neither cwd
  flag bypasses file or mapping protections. Approving for other sessions is
  human-only (SKILL.md, hard rule 1). `data.pending_proposals: [{id, repo}]` in any
  envelope identifies an open proposal. Inspect its effective mode/state:
  `apply` can proceed automatically; human-action waits follow the stop rule in
  [sync.md](sync.md#waiting-states).
- If you change install inputs (package.json deps, bun.lock, patches,
  pubspec.yaml / pubspec.lock, ...) and publish without switching, publish
  still runs the checks with deps that match the staged inputs: it uses a
  sealed set (warning `DepsSubstituted`) or installs one first (slower;
  warning `DepsReinstalled`, and the history shows an extra row stopped with
  `Drift`). Plain drift does not refuse the publish; it can still stop with
  `Drift` when the inputs move again during that install (`deps_drift`) or
  when the deps dirs overlap files you publish (`deps_select_failed`,
  `tracked_overlap`: follow its `fix`/`hint`). Still run the warning's `fix`
  (the outside-workspace command above) so your workspace matches. `status`
  shows the same `drift.fix`. If that install fails (`detail.stage: "deps_install"`), hand it
  to the human with `detail.log_tail`; do not retry in a loop (a retry
  returns `previous_attempt`).

## Release builds

- Release builds never run in a workspace (no remotes, detached `HEAD`, push
  guard). Publish (and push), then in the main checkout `git pull --ff-only`,
  check `HEAD` equals `data.commits.<repo>`, and run the build there; the
  build script is the human's step unless they asked you to run it. See the
  README's [Release builds from a session](https://github.com/dickwu/git-strata/blob/main/README.md#release-builds-from-a-session).

## Removing a session

- A full publish closes the session; you need no `rm`. The workspace goes to
  the trash (recoverable until trash `gc`) and the checkpoints stay. Publish
  from `cd ~`, then read `data.auto_close.sessions[<your session id>]`
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
- A done session, marked by anyone, is closed by the daemon on a later publish
  or push of its repos once everything has landed, unless one of its repos has
  `publish.auto_close = false`. To keep a session, or its workspace, run
  `git-strata mark --session S active`: it clears the mark, note and
  attribution. A successful publish reply without `data.auto_close` comes from
  a daemon that lacks the `publish.auto_close` feature: mark the session done
  and leave removal to the human, as before.
- `Dirty` from an `rm` you ran: nothing was removed. Mark done and
  tell the human what is unpublished. Never run `rm --discard` or seek a bypass.
  `detail.repos` lists unpublished paths, `detail.untracked_repos` edited siblings,
  and `detail.unresolved` interrupted scenes.
  `detail.reason: changed_during_remove` means files changed while rm ran:
  retry.
- Paths matching the repo's pinned `rm.regenerable` do not block; they are
  checkpointed and listed in `data.regenerable`.
- Never `rm` after an `Unknown` outcome.

Publish messages default to distinct latest manual checkpoint subjects in the
unpublished window, with a bounded body listing messages. `-m` and
`--message-for REPO=TEXT` override them. Empty/control/reserved-trailer messages
are refused; reserved-key errors name the line and its one-based number in
`detail.line` / `detail.line_number`. Messages already consumed by a committed
publish stay consumed after crash recovery, including in the confirmation preview.
Generated sessions/paths/Strata-Publish trailers identify the
commit; paths counts changed files, not selectors.

A parked or rewound base not in B or main_tip refuses base_not_on_main before
confirmation; give its fix to the human. If a workspace baseline moved after
the checkpoint, HeadMoved baseline_moved gives the literal publish command to
rerun. This takes fresh checkpoints and does not require accepting divergence.
If plan-time ref refresh returns `Busy lock_wait`, retry the same publish after
the other writer finishes; this error remains Busy even without a TTY.
A successful non-discard rm reports unpublished=false even if automatic rebase
has not moved the stale base yet; discard retains its pre-rm tracker value.
