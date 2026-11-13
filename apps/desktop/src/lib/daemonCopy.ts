/** Strings for the status-bar daemon trigger and its modal (gui.md §5.2: sentence case). */
const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

/** What the daemon is doing when it holds a lock or a session queue itself. */
const DAEMON_WORK_LABEL: ReadonlyMap<string, string> = new Map([
  ['catch_up', 'auto-rebase'],
  ['old_tree', 'cleaning an old tree'],
  ['periodic_fetch', 'fetching'],
  ['reconcile', 'reconcile'],
]);

export const daemonCopy = {
  trigger: {
    running: 'Daemon running',
    offline: 'Daemon unreachable',
    connecting: 'Connecting to daemon',
    open: 'Show daemon status',
    saved: (b: string) => `${b} saved`,
  },
  modal: {
    title: 'Daemon',
    running: 'Running',
    down: 'Unreachable',
    updated: (ago: string) => `Updated ${ago}`,
  },
  /** Every status-bar chip, the ones past the bar's "+N" included. */
  now: {
    heading: 'Now',
  },
  warnings: {
    heading: 'Warnings',
    none: 'No warnings.',
    /** The disclosure holding a warning's raw entry. */
    details: 'Details',
  },
  activity: {
    heading: 'Activity',
    holds: 'Repo locks',
    queues: 'Session queues',
    columns: {
      repo: 'Repo',
      heldBy: 'Held by',
      session: 'Session',
      waiting: 'Waiting',
      elapsed: 'For',
    },
    /** A holder: the daemon's own work by name, a request by its method. */
    holder: (method: string, background: boolean) => {
      if (!background) return method;
      const what = DAEMON_WORK_LABEL.get(method);
      return what ? `Daemon work: ${what}` : 'Daemon work';
    },
    none: '—',
    background:
      'Daemon work (auto-rebase onto new main, old-tree cleanup, fetching) is normal, not an error. Write requests to the same repo or session wait for it; one that waits 30 s before it has written anything gets Busy and can be retried.',
    waiting: (list: string) => `Requests waiting: ${list}`,
    waitingRepo: (repo: string, n: number) => `${repo} ${n}`,
  },
  storage: {
    heading: 'Storage',
    headline: (b: string) => `git-strata alone holds at least ${b}`,
    headlineTip: 'Can be much more: blocks a base shares with its sessions are not counted.',
    headlineUnavailable: 'How much git-strata holds alone: unavailable',
    unavailableTip: "Some files didn't report a private size, so no lower bound can be given.",
    unavailable: 'unavailable',
    measuring: '(measuring)',
    asOf: (when: string) => `as of ${when}`,
    groupsHeading: 'Held alone, by group (at least)',
    groups: {
      sessions: 'Sessions',
      bases: 'Bases',
      deps: 'Dependencies',
      store: 'Store, without trash',
      trash: 'Trash',
    },
    /** Followed by the gc command and `trashHuman`. */
    trashEligible: (n: number, age: string) =>
      `${n.toLocaleString('en-US')} trash ${plural(n, 'entry', 'entries')} older than ${age} can be emptied by`,
    trashHuman: '(a human command).',
    trashYoung: (n: number, age: string) =>
      `${n.toLocaleString('en-US')} trash ${plural(n, 'entry is', 'entries are')} younger than ${age}, so gc leaves ${plural(n, 'it', 'them')} for now.`,
    details: 'Details',
    allocated: 'Allocated, counted once per file; overcounts clones',
    logical: 'Logical size of all files; counts every clone in full',
  },
  scan: {
    heading: 'Size scan',
    running: (done: string | null, queued: string, active: string | null) => {
      const counts = [
        done === null ? null : `${done} done`,
        active === null ? null : `${active} in progress`,
        `${queued} queued`,
      ].filter((p) => p !== null);
      return `Measuring storage · ${counts.join(active === null ? ', ' : ' · ')}`;
    },
    idle: (ended: string | null) => (ended ? `Idle, last full pass ${ended}` : 'Idle'),
    failed: 'Recent measuring errors',
    failedNote:
      'Attempt history of the size scan: an error stays listed after a later attempt succeeds.',
    times: (n: number) => `(${n} times)`,
  },
  savings: {
    heading: 'Compared with git worktree',
    intro:
      'Each session clones its repos and dependencies copy-on-write. A plain git worktree writes a full copy of the same files, node_modules and target included.',
    upTo: 'Up to',
    live: (n: number) => `not written to disk by ${n} live ${plural(n, 'session', 'sessions')}`,
    asOf: (when: string) => `As of ${when}`,
    measuringMark: '(measuring)',
    heldAlone: (held: string, logical: string) =>
      `${held} held only by the sessions (at least), for ${logical} of files`,
    files: (n: string) => `${n} files a worktree would have written`,
    cumulative: (saved: string, n: number, since: string) =>
      `Up to ${saved} saved across ${n} ${plural(n, 'session', 'sessions')} recorded since ${since}`,
    cumulativeNote: (removed: number, unsized: number) =>
      [
        removed > 0 ? `${removed} since removed` : null,
        unsized > 0 ? `${unsized} removed before they were measured` : null,
      ]
        .filter(Boolean)
        .join(', '),
    measuring: 'Still measuring some sessions, so these figures can grow.',
    unavailable:
      "This daemon doesn't report space savings. Update git-strata and restart the daemon to see them.",
    accuracy:
      'Held alone counts only the blocks a session holds by itself (APFS private size), so it is a lower bound: blocks shared with the base are not counted. Saved is full size minus that, so it is an upper bound.',
    timeHeading: 'Time to a ready workspace',
    measured: 'Measured',
    estimated: 'git worktree (estimate)',
    measuredValue: (avg: string, n: number) =>
      `${avg} on average for git-strata new, over ${n} ${plural(n, 'session', 'sessions')}`,
    estimatedValue: (est: string) => `at least ${est} to write the same files`,
    speedup: (x: string) => `About ${x} faster`,
    speedupNote: '(estimate)',
    liveEstimate: (est: string) =>
      `Writing the live sessions' files as full checkouts would take at least ${est}.`,
    formula:
      'Estimate = logical size ÷ 2 GB/s (2 × 10⁹ bytes a second, a fast SSD). It leaves out per-file overhead and installing dependencies, so a real git worktree takes longer.',
    timingOld:
      "This daemon doesn't record how long creating a session takes. A newer git-strata daemon times every session it creates.",
    timingNone: 'No session has been created since the daemon started timing them.',
    columns: {
      session: 'Session',
      logical: 'Full size',
      heldAlone: 'Held alone (at least)',
      saved: 'Saved (up to)',
      created: 'Created in',
      estimate: 'Worktree (est.)',
    },
    /** "Created in" cell for a session with no recorded time. */
    notTimed: '—',
    notTimedTip:
      'Created before the daemon recorded creation time. Only sessions created by a newer daemon are timed.',
    notTimedTipOld: "This daemon doesn't record creation time.",
    notTimedNote: (n: number) =>
      `— marks ${n} ${plural(n, 'session', 'sessions')} created before the daemon recorded creation time. Only sessions created by a newer daemon are timed.`,
  },
  status: {
    heading: 'Status',
    state: 'State',
    version: 'Version',
    pid: 'Process ID',
    uptime: 'Uptime',
    sessions: 'Sessions',
    fds: 'File descriptors',
    fdsValue: (open: string, limit: string) => `${open} open of ${limit}`,
    acceptErrors: (n: number) => (n === 0 ? 'no accept errors' : `${n} accept errors`),
    reconcile: 'Reconcile runs',
    holds: 'Holds',
    waiting: 'Requests waiting',
    warnings: 'Warnings',
    socket: 'Socket',
    notReported: 'Not reported by this daemon',
  },
} as const;
