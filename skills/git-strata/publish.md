# Publishing, dependencies and removing a session

## Publish

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
Loosening the config needs a daemon restart (by the human) plus the human's
`config relax` (until then `daemon status` alerts `ConfigRelaxPending`). Full
rule: the README's [Confirmation gate](https://github.com/dickwu/git-strata/blob/main/README.md#confirmation-gate-touch-id--tty).

## Dependencies

- `deps propose [--session S] [--repo R]` proposes a new dependency set after
  you change install inputs; `deps list` shows open proposals. Add `--switch`
  to move your own session onto the proposed set right away (no prompt; other
  sessions stay pinned). Check `data.switched.kind`: `skipped` with
  `code: OpenFiles` means a process holds files in your deps dir; stop it (only
  if you started it) and run again. Approving for other sessions is
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
  (`deps propose --switch`) so your workspace matches. `status` shows the same
  `drift.fix`. If that install fails (`detail.stage: "deps_install"`), hand it
  to the human with `detail.log_tail`; do not retry in a loop (a retry
  returns `previous_attempt`).

## Release builds

- Release builds never run in a workspace (no remotes, detached `HEAD`, push
  guard). Publish (and push), then in the main checkout `git pull --ff-only`,
  check `HEAD` equals `data.commits.<repo>`, and run the build there; the
  build script is the human's step unless they asked you to run it. See the
  README's [Release builds from a session](https://github.com/dickwu/git-strata/blob/main/README.md#release-builds-from-a-session).

## Removing a session

- `git-strata rm --session S` once its work is published. A file whose exact
  bytes and mode are in a publish of this session that origin already has
  does not count as unpublished.
- `Dirty`: nothing was removed and the session is still active.
  `detail.repos` lists unpublished paths per repo, `detail.untracked_repos`
  edited untracked siblings, `detail.unresolved` interrupted scenes. Publish
  the work (for following repos: sync, checks, checkpoint, publish, then rm),
  or hand `git-strata rm --session S --discard` to the human.
  `detail.reason: changed_during_remove` means files changed while rm ran:
  retry.
- Paths matching the repo's pinned `rm.regenerable` do not block; they are
  checkpointed and listed in `data.regenerable`.
- Never `rm` after an `Unknown` outcome.

Publish messages default to distinct latest manual checkpoint subjects in the
unpublished window, with a bounded body listing messages. `-m` and
`--message-for REPO=TEXT` override them. Empty/control/reserved-trailer messages
are refused. Generated sessions/paths/Strata-Publish trailers identify the
commit; paths counts changed files, not selectors.

A parked or rewound base not in B or main_tip refuses base_not_on_main before
confirmation; give its fix to the human. If a workspace baseline moved after
the checkpoint, HeadMoved baseline_moved gives the literal publish command to
rerun. This takes fresh checkpoints and does not require accepting divergence.
A successful non-discard rm reports unpublished=false even if automatic rebase
has not moved the stale base yet; discard retains its pre-rm tracker value.
