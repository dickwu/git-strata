/** Every user-visible string (gui.md §5.2: English only, sentence case, say what to do). */
/**
 * The text the daemon's prompt names for an approval (strata-confirm reason.rs:
 * "approve dependency proposal {}"); copy.test.ts checks the core still says this.
 */
export const promptText = (id: number) => `approve dependency proposal ${id}`;

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
    review: 'Review',
    reviewLabel: (what: string) => `Review ${what}`,
  },
  /** The review modal (gui-approve design §4, decisions D1–D6). Sentence case, no "we". */
  review: {
    depsTitle: (id: number, repo: string) => `Deps proposal ${id} in ${repo}`,
    conflictTitle: (repo: string) => `Conflicts in ${repo}`,
    fromMain: 'From the main branch',
    fromSession: (s: string) => `From session ${s}`,
    proposed: 'proposed',
    mainAt: 'Main at',
    deps: 'Dependencies',
    inputsChanged: 'install inputs changed',
    inputsSame: 'install inputs unchanged (toolchain or deps config changed)',
    loading: 'Reading the latest state from the daemon…',
    readFailed: 'Could not read the latest state, so Approve is off.',
    scopeHeading: 'What Approve covers',
    scope: (id: number, repo: string) =>
      `Approve applies proposal ${id} to every session on ${repo} that can move at the moment you confirm, as the daemon sees it then. This view does not list those sessions yet; the result lists each one that moved or was skipped.`,
    promptHeading: 'What happens when you approve',
    prompt: (id: number) =>
      `macOS asks for Touch ID or your password. The prompt names “${promptText(id)}”.`,
    promptCheck: "If the number is different, or you didn't click Approve, cancel it.",
    terminal: 'Or approve it in a terminal:',
    command: (id: number) => `git-strata deps approve ${id}`,
    copyCommand: 'Copy terminal command',
    copyPath: 'Copy path',
    copied: 'Copied',
    copyFailed: 'Could not copy',
    close: 'Close',
    approve: 'Approve',
    reviewAgain: 'Review again',
    reviewOther: (id: number) => `Review proposal ${id}`,
    openSession: (s: string) => `Open ${s}`,
    confirming: (id: number) => `Look for the macOS prompt for proposal ${id}.`,
    confirmingNote: 'Nothing moves until you pass it. Cancel the prompt to stop.',
    stillWorking: 'Still working: installing dependencies can take minutes.',
    stopWaiting: 'Stop waiting',
    gone: (id: number) => `Proposal ${id} is no longer pending.`,
    goneNote:
      'It was approved or replaced. The daemon only lists open proposals, so this view cannot tell which.',
    otherPending: (id: number, repo: string) => `Proposal ${id} is now waiting in ${repo}.`,
    conflictSession: 'Session',
    conflictRepo: 'Repo',
    conflictNote:
      "These files kept the session's version when it moved to the new base. Fix them in the session's workspace.",
    noConflicts: (repo: string) => `No conflicts left in ${repo}.`,
    conflictPaths: (n: number) => (n === 1 ? '1 conflicted path' : `${n} conflicted paths`),
    result: {
      heading: 'Result',
      unknown: 'Result unknown.',
      unknownRows: (s: string[]) =>
        `Result unknown for ${s.join(', ')}: the move may have happened.`,
      unknownNote:
        'Check the sessions before doing anything else. Close this and open the item again to read the current state.',
      detachedNote:
        'The modal stopped waiting; the daemon may still finish. The lists refresh when it answers.',
      applied: (n: number) =>
        n === 0
          ? 'Approved. No session needed to move.'
          : n === 1
            ? 'Approved. Moved 1 session.'
            : `Approved. Moved ${n} sessions.`,
      subset: (id: number) =>
        `Selected sessions updated. Proposal ${id} stays waiting for the others.`,
      skipped: (k: number, id: number) =>
        k === 1
          ? `Approved, but 1 session was skipped, so proposal ${id} stays waiting.`
          : `Approved, but ${k} sessions were skipped, so proposal ${id} stays waiting.`,
      failed: (k: number, id: number) =>
        k === 1
          ? `1 session failed to move, so proposal ${id} stays waiting.`
          : `${k} sessions failed to move, so proposal ${id} stays waiting.`,
      state: (id: number, state: string) => `Proposal ${id} is now ${state}.`,
      notApproved: 'Not approved.',
      notConfirmed: {
        cancelled: 'The prompt was cancelled. Nothing moved.',
        failed: 'Authentication failed. Nothing moved.',
        timeout: 'The prompt timed out. Nothing moved.',
        interrupted: 'The prompt was interrupted. Nothing moved.',
      } as Record<string, string>,
      promptOpen:
        "Another git-strata confirmation prompt is already open. If you didn't start it, cancel it.",
      planChanged: 'The sessions to move changed since you looked. Nothing moved.',
      replaced: (id: number, by: number) =>
        `Proposal ${id} was replaced by proposal ${by}. Nothing moved.`,
      alreadyApproved: 'Already approved.',
      notPending: (id: number, state: string) =>
        `Proposal ${id} is no longer pending (${state}). Nothing moved.`,
      headMoved:
        'Main moved after this proposal was made. Nothing moved. An updated proposal will be registered; review that one.',
      lockWait: (holder: string, lock: string) =>
        `Another change (${holder}) was holding ${lock}. Nothing moved. Try again; it asks for confirmation again.`,
      unknownHolder: 'holder unknown',
      shuttingDown: 'The daemon is shutting down. Nothing moved.',
      buildLock: 'Dependencies are being installed by another task. Nothing moved. Try again later.',
      daemonDown: "The daemon isn't running, so nothing was sent.",
      inFlight: 'An approval is already being sent. Wait for its result.',
      unsupported: (r: string) => `This Mac can't show the confirmation prompt (${r}).`,
      other: (message: string, code: string) => `${message} (${code})`,
      rowMoved: '✓ Moved',
      rowSkipped: '– Skipped',
      rowFailed: '✕ Failed',
      rowUnknown: '? Unknown',
      conflicts: (n: number) => (n === 1 ? '1 conflict' : `${n} conflicts`),
      checkStatus: 'Check its status before trying again.',
      skip: {
        Untracked: 'Does not track this repo',
        AlreadyThere: 'Already on these dependencies',
        NotReady: 'Not ready yet',
        OwnChange: 'Has its own dependency edits',
        Drift: 'Has its own dependency edits',
        OpenFiles: 'Files in the old tree are open',
      } as Record<string, string>,
      ownProposal: (id: number) => `proposal ${id}`,
      openFile: (path: string, command: string, pid: number | string) =>
        `${path} (${command}, pid ${pid})`,
    },
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
