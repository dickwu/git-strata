/** Strings for the status-bar daemon trigger and its modal (gui.md §5.2: sentence case). */
const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

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
  savings: {
    heading: 'Compared with git worktree',
    intro:
      'Each session clones its repos and dependencies copy-on-write. A plain git worktree writes a full copy of the same files, node_modules and target included.',
    live: (n: number) => `not written to disk by ${n} live ${plural(n, 'session', 'sessions')}`,
    onDisk: (onDisk: string, logical: string) => `${onDisk} on disk for ${logical} of files`,
    files: (n: string) => `${n} files a worktree would have written`,
    cumulative: (saved: string, n: number, since: string) =>
      `${saved} across ${n} ${plural(n, 'session', 'sessions')} recorded since ${since}`,
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
      'On disk counts only the blocks a session holds alone (APFS private size). Blocks shared with the base are not counted.',
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
      "This daemon doesn't time new sessions. Once it runs a build that records create_ms, measured times appear here.",
    timingNone: 'No session has been created since the daemon started timing them.',
    columns: {
      session: 'Session',
      logical: 'Full size',
      onDisk: 'On disk',
      saved: 'Saved',
      created: 'Created in',
      estimate: 'Worktree (est.)',
    },
    notTimed: 'not timed',
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
    storage: 'Storage',
    storageValue: (onDisk: string, logical: string) => `${onDisk} on disk, ${logical} logical`,
    socket: 'Socket',
    scan: 'Size scan',
    scanRunning: (queued: number) => `Running, ${queued} queued`,
    scanIdle: (ended: string | null) => (ended ? `Idle, last full pass ${ended}` : 'Idle'),
    notReported: 'Not reported by this daemon',
    down: "The daemon isn't running. Run `git-strata daemon start` in a terminal.",
  },
} as const;
