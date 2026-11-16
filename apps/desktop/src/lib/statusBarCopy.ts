import { copy } from './copy';

/** Model copy only; repo names, session ids and daemon reasons remain plain text (G6). */
export const statusBarCopy = {
  daemon: {
    running: 'Daemon running',
    connecting: 'Connecting to daemon',
    checking: 'Checking the daemon…',
    notRunning: "Daemon isn't running",
    notResponding: "Daemon isn't responding",
    starting: 'Daemon starting (recovering)',
    waitingForVolume: 'Daemon waiting for its data volume',
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
    auditFailed: 'The sessions audit log could not be written',
    timeout: copy.daemon.stuck,
    waitingForReply: 'An earlier request is still waiting for the daemon.',
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
    /** P-b: `session` is the ticket when known, else a short id (§3.1). */
    reportedHold: (repo: string, method: string, elapsed: string, session?: string) =>
      `${repo} locked for ${elapsed} by ${method === 'catch_up' ? 'auto-rebase' : method}${session ? ` (${session})` : ''}`,
  },
  activity: {
    move: (cause: string, session: string) =>
      cause === 'catch_up'
        ? `Moving ${session} to new main`
        : cause === 'publish'
          ? 'Rebasing after publish'
          : cause === 'sync'
            ? `Sync moving ${session}`
            : cause === 'deps'
              ? `Moving ${session} to new dependencies`
              : `Replaying ${session}`,
    rollout: (done: number, total: number) => `Rolling out deps ${done}/${total}`,
    base: 'Building base',
    create: (session: string | undefined, repos: number) =>
      `Creating session${session ? ` ${session}` : ''}${repos > 0 ? ` · ${repos} ${repos === 1 ? 'repo' : 'repos'}` : ''}`,
    install: 'Installing dependencies',
    publish: (repo: string) => `Publishing ${repo}`,
    push: (repo: string) => `Pushing ${repo}`,
    background: (repo: string) => `${repo} busy with daemon work`,
    phase: (phase: string) => `Phase: ${phase.replace(/_/g, ' ')}.`,
    autoRebaseWaiting: (repo: string, session: string | undefined, reason: string) =>
      `Auto-rebase waiting · ${repo}${session ? ` · ${session}` : ''} (${reason.replace(/_/g, ' ') || 'see details'})`,
    catchUp: (repo: string, session: string | undefined, elapsed: string) =>
      `Auto-rebase work · ${repo}${session ? ` · ${session}` : ''} · ${elapsed}`,
    oldTree: (repo: string, elapsed: string) => `Cleaning old tree · ${repo} · ${elapsed}`,
    fetch: (repo: string) => `Fetching · ${repo}`,
    daemon: (repo: string, elapsed: string) => `${repo} is busy with daemon work · ${elapsed}`,
    counts: (text: string, waiting: number, pending: number) =>
      `${text}${waiting > 0 ? ` · ${waiting} waiting` : ''}${pending > 0 ? ` · ${pending} pending` : ''}`,
    deferred: (text: string, n: number) => `${text}${n > 0 ? ` · ${n} deferred` : ''}`,
    tooltip: (repo: string) =>
      `New write requests to ${repo} may wait for this work. A request that waits 30 seconds before writing gets Busy and can be retried.`,
  },
  scan: (done: number | undefined, queued: number) =>
    `Measuring storage · ${done === undefined ? '' : `${done} done, `}${queued} queued`,
  scanProgress: (done: number, active: number, queued: number) =>
    `Measuring storage · ${done} done · ${active} in progress · ${queued} queued`,
  scanUnresolved: (n: number) => `Couldn't measure ${n} ${n === 1 ? 'item' : 'items'}`,
  busy: (n: string, scope: string | undefined, holder: string | undefined) =>
    scope
      ? `${n} ${n === '1' ? 'request' : 'requests'} got Busy · ${scope}${holder ? ` (${holder})` : ''}`
      : 'Some requests got Busy',
  /** The ring (50 entries) dropped a reply younger than ten minutes: no per-repo count. */
  busyTruncated: '50+ requests got Busy',
  free: (amount: string) => `${amount} free`,
  lowSpace: (amount: string) => `Data volume is low on space · ${amount} free`,
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
