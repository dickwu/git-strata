/** Every user-visible string (gui.md §5.2: English only, sentence case, say what to do). */
export const copy = {
  app: 'Git Strata',
  nav: {
    sessions: 'Sessions',
    daemon: 'Daemon',
  },
  palette: {
    placeholder: 'Search sessions and screens',
    open: 'Open command palette',
    empty: 'No match.',
    goTo: (name: string) => `Go to ${name}`,
    toggleTheme: 'Switch light / dark theme',
    refresh: 'Refresh now',
  },
  daemon: {
    title: 'Daemon',
    running: 'Running',
    down: "Daemon isn't running. Start it to see live sessions.",
    downHint: 'Run `git-strata daemon start` in a terminal. Starting it from here comes later.',
    stuck: (secs: number) =>
      `The daemon did not answer within ${secs} seconds — it may have run out of file descriptors or be stuck.`,
    busy: 'The daemon is busy. Retrying.',
    legacy:
      'This daemon predates the API version field, so the app reads it gently: session status is fetched less often.',
    reconcile: (runs: number) =>
      runs === 0 ? 'No reconcile has run since the daemon started.' : `Reconcile runs: ${runs}`,
    holds: (n: number) => (n === 0 ? 'No holds' : n === 1 ? '1 hold' : `${n} holds`),
    waiting: 'Requests waiting per repo',
    noneWaiting: 'Nothing is queued.',
    warnings: 'Warnings',
    noWarnings: 'No warnings.',
    storage: 'Storage',
    socket: 'Socket',
  },
  sessions: {
    title: 'Sessions',
    empty: 'No sessions. Agents create one with `git-strata new`.',
    columns: {
      ticket: 'Ticket',
      id: 'Session',
      state: 'State',
      repos: 'Repos',
      changes: 'Pending',
      conflicts: 'Conflicts',
      created: 'Created',
    },
    slow: 'Session status is checked every 30 seconds while the window is open.',
  },
  detail: {
    back: 'All sessions',
    repos: 'Repos',
    checkpoints: 'Checkpoints',
    noCheckpoints: 'No checkpoints yet.',
    head: 'Head',
    baseTree: 'Base tree',
    depsSet: 'Deps set',
    path: 'Path',
    health: 'Health',
    pending: 'Pending',
    conflicts: 'Conflicts',
    chain: 'Published chain',
    notFound: 'This session no longer exists.',
  },
  status: {
    live: (ago: string) => `Updated ${ago}`,
    offline: 'Daemon unreachable',
    loading: 'Connecting…',
    sessions: (n: number) => (n === 1 ? '1 session' : `${n} sessions`),
  },
  errors: {
    notInTauri: 'Open this inside the Git Strata app.',
    unknown: 'Something went wrong talking to the daemon.',
  },
} as const;

export function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 10) return 'just now';
  if (s < 60) return `${s} s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return m === 1 ? '1 min ago' : `${m} min ago`;
  return `${Math.floor(m / 60)} h ago`;
}

export function bytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 10 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}
