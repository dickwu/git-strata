/** Every user-visible string (gui.md §5.2: English only, sentence case, say what to do). */
export const copy = {
  app: 'GIT STRATA',
  nav: {
    sessions: 'Sessions',
    daemon: 'Daemon',
    waiting: (n: number) => (n === 1 ? '1 waiting for you' : `${n} waiting for you`),
  },
  waiting: {
    title: 'Waiting for you',
    show: (n: number) =>
      n === 1 ? 'Show the 1 item waiting for you' : `Show the ${n} items waiting for you`,
    deps: (id: number, repo: string) => `Deps proposal ${id} in ${repo}`,
    conflict: (n: number, repo: string) =>
      n === 1 ? `1 conflict in ${repo}` : `${n} conflicts in ${repo}`,
    fromMain: 'From the main branch',
    approve: (id: number) => `Review in a terminal: \`git-strata deps approve ${id}\``,
    resolve: 'Resolve it in the session workspace.',
    open: (session: string) => `Open ${session}`,
    stale: 'Could not refresh. These may be out of date.',
  },
  palette: {
    placeholder: 'Search sessions and screens',
    open: 'Open command palette',
    empty: 'No match.',
    goTo: (name: string) => `Go to ${name}`,
    theme: (name: string) => `Theme: ${name}`,
    refresh: 'Refresh now',
    withRepo: (repo: string) => `Sessions with ${repo}`,
    byAgent: (agent: string) => `Sessions by ${agent}`,
  },
  daemon: {
    title: 'Daemon',
    running: 'Running',
    down: "Daemon isn't running. Start it to see live sessions.",
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
  /** The Start daemon control in the down notice (S0, design §6.1: start only, never stop). */
  start: {
    button: 'Start daemon',
    retry: 'Try again',
    done: 'Daemon started',
    fallback: 'Or run `git-strata daemon start` in a terminal.',
    phase: {
      kickstart: 'Asking launchd to start the daemon',
      bootstrap: 'Loading the launch agent into launchd',
      waiting: 'Waiting for the daemon to answer',
      store: 'Waiting for the daemon to open its store',
      tcc: 'Waiting for macOS. Approve the removable-volume prompt if one is showing.',
      recovering: 'The daemon is recovering its sessions',
      ready: 'Daemon started',
    },
    setup: {
      title: 'No launch agent is installed, so the daemon cannot be started from here yet.',
      hint: (plist: string) =>
        `Install \`${plist}\` as the README section "Install and daemon" describes, then start it. Setting it up from here comes later.`,
    },
    errors: {
      plistMissing: (plist: string) => `No launch agent at \`${plist}\`.`,
      launchctl: (step: string, status: number | null) =>
        status === null
          ? `launchctl ${step} did not finish.`
          : `launchctl ${step} failed with exit code ${status}.`,
      launchctlHint:
        'Its output is below. Running `git-strata daemon start` in a terminal shows the same.',
      tcc: 'macOS is still asking whether the daemon may use its removable volume.',
      tccHint:
        'Approve the removable-volume prompt, or allow it in System Settings, Privacy & Security, then try again.',
      store: 'The daemon started but cannot open its store.',
      storeHint: 'Check that the volume is mounted, then try again.',
      timeout: (secs: number) =>
        `launchd took the start, but the daemon did not answer within ${secs} seconds.`,
      timeoutHint:
        'Its log is `~/Library/Application Support/git-strata/daemon.err.log`. Try again once it settles.',
      inFlight: 'A start is already in progress.',
      notMain: 'Start the daemon from the main window.',
    },
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
      time: 'Time',
    },
    time: {
      created: (t: string) => `Created ${t}`,
      running: (d: string) => `Running for ${d}`,
      ended: 'Removed. The daemon does not report when, so no final duration.',
    },
    slow: 'Session status is checked every 30 seconds while the window is open.',
    filter: {
      placeholder: 'Filter by ticket, session, repo or agent',
      label: 'Filter sessions',
      repos: 'Repos',
      reposHint: 'Show sessions that use all selected repos',
      agents: 'Agents',
      agentsHint: 'Show sessions from any selected agent',
      toggle: (name: string, n: number, on: boolean) =>
        `${name}, used by ${n} ${n === 1 ? 'session' : 'sessions'}${on ? ', selected' : ''}`,
      rowTag: (repo: string, on: boolean) =>
        on ? `${repo}: remove from filter` : `${repo}: show only sessions that use it`,
      unused: (repo: string) => `${repo} is in this session but unused (not tracked, no changes)`,
      clear: 'Clear',
      none: 'No session matches this filter.',
      count: (shown: number, total: number) => `${shown} of ${total}`,
    },
    openRow: (name: string) => `Open ${name}`,
    tracked: 'Tracked',
    reference: 'Reference',
  },
  detail: {
    back: 'All sessions',
    close: 'Close session details',
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
  theme: {
    label: 'Theme',
    names: { system: 'System', light: 'Light', dark: 'Dark' },
    /** Tooltip on each status bar segment. */
    tip: {
      system: 'Match macOS appearance',
      light: 'Light theme',
      dark: 'Dark theme',
    },
    /** Appended to the System tooltip: what System resolves to right now. */
    now: (name: string) => `now ${name.toLowerCase()}`,
  },
  errors: {
    notInTauri: 'Open this inside the GIT STRATA app.',
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

/**
 * How long something has existed: "<1m", "14m", "2h 14m", "3d 4h". Two units at most, so
 * the list column stays narrow; the tooltip carries the exact times.
 */
export function duration(ms: number): string {
  const m = Math.floor(Math.max(0, ms) / 60_000);
  if (m < 1) return '<1m';
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`;
}

/** Local time, as short as it can be: "2:05 PM" today, "Oct 27, 2:05 PM" this year, else "Oct 27, 2025". */
export function compactTime(t: Date, now: Date): string {
  const sameDay = t.toDateString() === now.toDateString();
  if (sameDay) return hm(t);
  // Date and time joined by hand: ICU versions differ on ", " vs " at ".
  if (t.getFullYear() === now.getFullYear())
    return `${t.toLocaleDateString('en', { month: 'short', day: 'numeric' })}, ${hm(t)}`;
  return t.toLocaleDateString('en', { month: 'short', day: 'numeric', year: 'numeric' });
}

const hm = (t: Date) => t.toLocaleTimeString('en', { hour: 'numeric', minute: '2-digit' });

/** Full local timestamp with its zone, for tooltips: "Oct 27, 2026, 2:05:09 PM CDT". */
export function fullTime(t: Date): string {
  const date = t.toLocaleDateString('en', { month: 'short', day: 'numeric', year: 'numeric' });
  return `${date}, ${t.toLocaleTimeString('en', { timeStyle: 'long' })}`;
}
