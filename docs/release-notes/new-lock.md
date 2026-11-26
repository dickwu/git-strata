# Unreleased: `new`/`add` without the repo lock, a lock-free old-tree check, retry by default

**Upgrade the CLI and the daemon together.** The retry needs both: an older
daemon never marks a refusal replay-safe, so a new CLI retries nothing against
it and keeps the old blocking `new`/`add`; an older CLI against a new daemon
gets a certified refusal once, as a plain `Busy lock_wait`, without retrying.
The daemon advertises the change as the feature `new.fetch_skip`.

- `new` and `add` no longer wait behind a busy repo lock before they create
  anything; once the session exists, their follow-up steps can still wait
  behind the current holder before the reply. Main's tip, the
  publish-chain top and the anchor are read from a snapshot the daemon refreshes
  each time a repo lock is released. A due fetch that cannot take the lock is
  skipped: the session is built on main as last fetched, and `warnings` gets
  `{kind: "FetchSkipped", repo, holder, holder_session, last_fetch_at}` (also
  printed on stderr). Pinning, shadow refs and unpinning on `rm` no longer take
  the repo lock either.
- When a lock or session-queue wait expires before anything was created,
  `new`/`add` refuse with `Busy lock_wait` and `detail.retry_safe: true`. The CLI
  sends such a refusal again, backing off from 1 s to 10 s, for `--timeout SECS`
  (else `GIT_STRATA_TIMEOUT`, else 600 s; 0 = unlimited). The budget bounds only
  the retries, never one call. On a TTY or with `--progress`, stderr names the
  holder before each retry, e.g.
  `waiting (45s): new; repo tool held by sync (session s-uijvlo3l, request, 2m10s); retrying`.
  Once the budget is spent the last refusal gains `detail.retries`,
  `detail.waited_ms` and `detail.fix`. Every `lock_wait` refusal now names its
  holder (`holder`, `holder_session`, `holder_origin`, `holder_since`, `held_ms`).
- A `new` that an agent's own tool timeout kills may still have created the
  session: check `git-strata ls` for the ticket before running it again.
- The old-tree check (whether a tree left behind by a move or sync still has
  writers) now runs on its own worker without the repo lock for its walk. It
  takes the session queue only briefly, yields to foreground requests, and holds
  the repo lock only for the final comparison. `rm`, `restore`, `sync`,
  `deps approve`, publish's rebase, `resolve` and automatic closing stop a
  check of their session and wait until it has left, instead of racing it.
- The desktop status bar shows the check as "Checking old tree · <repo>"; it
  never hides lock-holding work.
