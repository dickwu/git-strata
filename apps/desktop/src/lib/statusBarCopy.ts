import { copy } from './copy';

/** Model copy only; repo names, session ids and daemon reasons remain plain text (G6). */
export const statusBarCopy = {
  daemon: {
    running: 'Daemon running',
    offline: 'Daemon unreachable',
    connecting: 'Connecting to daemon',
    checking: 'Checking the daemon…',
    notRunning: "Daemon isn't running",
    notResponding: "Daemon isn't responding",
    starting: 'Daemon starting (recovering)',
    permission:
      'Waiting for its data volume: allow Removable Volumes access in System Settings → Privacy & Security → Files and Folders',
    volumeUnavailable: 'Data volume not available',
    fileHandlesLow: 'Daemon is close to its file-handle limit',
    fileHandlesExhausted: 'Daemon is out of file handles',
    live: (age: string) => {
      const updated = copy.status.live(age);
      return updated.startsWith('Live · ') ? updated : `Live · ${updated.toLowerCase()}`;
    },
    stale: (elapsed: string) => `Not updated for ${elapsed}`,
    needsAttention: (n: number) => `Daemon needs attention: ${n}`,
    restarted: (age: string) => `Daemon restarted ${age}`,
    versionMismatch: (app: string, daemon: string) => `App ${app} · daemon ${daemon}`,
    reconcilePanicked: 'Daemon reconciliation failed',
    auditFailed: 'Some session details could not be audited',
    timeout: copy.daemon.stuck,
    busy: copy.daemon.busy,
    notInTauri: copy.errors.notInTauri,
    unknownError: copy.errors.unknown,
  },
  waiting: copy.nav.waiting,
  conflicts: copy.tray.conflicts,
  unavailable: 'Some session details unavailable',
  needsPush: (repo: string) => `Published but not pushed · ${repo}`,
  warning: {
    unknown: (kind: string) => `Daemon notice: ${kind}`,
    storePoisoned: 'The daemon store needs attention',
    verifyFailed: (message: string) => `Store verification failed${message ? `: ${message}` : ''}`,
    unknownPublish: (id: string) => `Publish outcome is unknown${id ? `: ${id}` : ''}`,
    unknownPush: (repo: string) => `Push outcome is unknown${repo ? `: ${repo}` : ''}`,
    quarantine: (path: string) => `Quarantined data needs attention${path ? `: ${path}` : ''}`,
    attachOutside: (reason: string) =>
      `An attachment is outside its repository${reason ? `: ${reason}` : ''}`,
    configRelax: (repo: string) => `${copy.tray.relax}${repo ? ` · ${repo}` : ''}`,
    configReload: (reason: string) => `config.toml not applied: ${reason || 'see details'}`,
    configRestart: (fields: string[]) =>
      `Restart the daemon to apply: ${fields.length ? fields.join(', ') : 'configuration changes'}`,
    fetch: (repo: string) => `Fetching ${repo || 'a repository'} failed`,
    catalogGrowth: (n: number | undefined) =>
      n === undefined ? 'The catalog has grown' : `The catalog has ${n} generations`,
    staleRefLock: (repo: string) => `Removed a stale ref lock${repo ? ` · ${repo}` : ''}`,
    publishRunning: 'A publish is running',
    autoRebaseStuck: (repo: string, session: string | undefined, reason: string) =>
      `Auto-rebase stuck: ${repo}${session ? ` · ${session}` : ''} (${reason.replace(/_/g, ' ') || 'see details'})`,
    longWait: (repo: string | undefined, elapsed: string) =>
      `A request is waiting${repo ? ` on ${repo}` : ''} (current attempt ${elapsed})`,
    abnormalHold: (repo: string, method: string, elapsed: string) =>
      `${repo} locked by ${method} for ${elapsed}`,
  },
  activity: {
    catchUp: (repo: string, session: string | undefined, elapsed: string) =>
      `Auto-rebase work · ${repo}${session ? ` · ${session}` : ''} · ${elapsed}`,
    oldTree: (repo: string, elapsed: string) => `Cleaning old tree · ${repo} · ${elapsed}`,
    fetch: (repo: string) => `Fetching · ${repo}`,
    daemon: (repo: string, elapsed: string) => `${repo} is busy with daemon work · ${elapsed}`,
    counts: (text: string, waiting: number, pending: number) =>
      `${text}${waiting > 0 ? ` · ${waiting} waiting` : ''}${pending > 0 ? ` · ${pending} pending` : ''}`,
    tooltip: (repo: string) =>
      `New write requests to ${repo} may wait for this work. A request that waits 30 seconds before writing gets Busy and can be retried.`,
  },
  scan: (done: number | undefined, queued: number) =>
    `Measuring storage · ${done === undefined ? '' : `${done} done, `}${queued} queued`,
  saved: (amount: string, measuring: boolean) =>
    `up to ${amount} saved${measuring ? ' (measuring)' : ''}`,
  savedTooltip: (asOf: string | null, measuring: boolean) =>
    `An upper bound on space saved compared with full copies. APFS private sizes can substantially understate space held by sessions.${asOf ? ` Measured as of ${asOf}.` : ''}${measuring ? ' Some sessions are still being measured.' : ''}`,
  /** Whole units keep activity copy compact at the bar's five-second refresh cadence. */
  elapsed: (ms: number) => {
    const seconds = Math.max(0, Math.floor(ms / 1000));
    if (seconds < 60) return `${seconds} s`;
    const minutes = Math.floor(seconds / 60);
    return minutes < 60 ? `${minutes} min` : `${Math.floor(minutes / 60)} h`;
  },
} as const;
