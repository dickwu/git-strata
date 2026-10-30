/**
 * The status-bar daemon modal: daemon facts and savings against plain `git worktree`
 * checkouts. Pure; every number comes from `daemon_status` or `du`.
 *
 * Space is measured: the daemon sizes each session with APFS private sizes, and
 * `savings` = logical − private is what full checkouts of the same files would have
 * written (daemon.md §5.12). Time is measured only on the git-strata side (`create_ms`,
 * newer daemons). The plain-worktree side is an estimate: logical bytes ÷ ASSUMED_COPY_RATE,
 * with no per-file cost and no dependency install, so it is a lower bound.
 */
import type { DaemonStatus, DuReply } from './types';

/** Optimistic sustained copy rate for the estimate (bytes per second, decimal GB). */
export const ASSUMED_COPY_RATE = 2_000_000_000;

export function copyEstimateMs(logical: number): number {
  return (Math.max(0, logical) / ASSUMED_COPY_RATE) * 1000;
}

export interface SavingsRow {
  session: string;
  logical: number;
  onDisk: number;
  saved: number;
  files: number | null;
  createMs: number | null;
  copyEstimateMs: number;
  /** Estimate ÷ measured; null without a measured, non-zero create time. */
  speedup: number | null;
}

export interface SavingsView {
  live: {
    saved: number;
    logical: number;
    onDisk: number;
    files: number | null;
    sessions: number;
    complete: boolean;
    asOf: string | null;
    copyEstimateMs: number;
  };
  cumulative: {
    saved: number;
    sessions: number;
    sized: number;
    unsized: number;
    removed: number;
    since: string | null;
  };
  /** Over sessions with a measured `new`; null when there are none. */
  timing: {
    timed: number;
    avgCreateMs: number;
    copyEstimateMs: number;
    speedup: number | null;
  } | null;
  /** Why `timing` is null. */
  timingMissing: 'old-daemon' | 'none-timed' | null;
  rows: SavingsRow[];
  /** Rows with no recorded create time (made before the daemon timed sessions). */
  untimed: number;
}

const ratio = (est: number, measured: number | null) =>
  measured !== null && measured > 0 ? est / measured : null;

/** null: this daemon does not report savings (before the du savings report). */
export function savingsView(du: DuReply | undefined): SavingsView | null {
  if (!du?.savings || !du.sessions) return null;
  const { live, cumulative: c } = du.savings;
  const rows: SavingsRow[] = du.sessions.list
    .map((s) => {
      const createMs = typeof s.create_ms === 'number' ? s.create_ms : null;
      const est = copyEstimateMs(s.logical);
      return {
        session: s.session,
        logical: s.logical,
        onDisk: s.private,
        saved: s.savings,
        files: typeof s.files === 'number' ? s.files : null,
        createMs,
        copyEstimateMs: est,
        speedup: ratio(est, createMs),
      };
    })
    .sort((a, b) => b.saved - a.saved);
  const files = du.sessions.all.files;
  let timing: SavingsView['timing'] = null;
  let timingMissing: SavingsView['timingMissing'] = null;
  if (typeof c.timed !== 'number' || typeof c.create_ms !== 'number') {
    timingMissing = 'old-daemon';
  } else if (c.timed === 0) {
    timingMissing = 'none-timed';
  } else {
    const est = copyEstimateMs(c.timed_logical ?? 0);
    timing = {
      timed: c.timed,
      avgCreateMs: c.create_ms / c.timed,
      copyEstimateMs: est / c.timed,
      speedup: ratio(est, c.create_ms),
    };
  }
  return {
    live: {
      saved: live.savings,
      logical: live.logical,
      onDisk: live.private,
      files: typeof files === 'number' ? files : null,
      sessions: du.sessions.list.length,
      complete: live.complete,
      asOf: live.as_of,
      copyEstimateMs: copyEstimateMs(live.logical),
    },
    cumulative: {
      saved: c.savings,
      sessions: c.sessions,
      sized: c.sized,
      unsized: c.unsized,
      removed: c.removed,
      since: c.since,
    },
    timing,
    timingMissing,
    rows,
    untimed: rows.filter((r) => r.createMs === null).length,
  };
}

export interface StatusFacts {
  version: string | null;
  api: number | null;
  pid: number | null;
  /** ms since `started`, from the reply's own clock reading. */
  uptimeMs: number | null;
  fds: { open: number; limit: number; acceptErrors: number; share: number } | null;
  reconcileRuns: number;
  holds: number;
  waiting: number;
  warnings: number;
}

export function statusFacts(s: DaemonStatus, now: number): StatusFacts {
  const started = s.started ? Date.parse(s.started) : NaN;
  const fds = s.fds;
  return {
    version: s.version ?? null,
    api: typeof s.api === 'number' ? s.api : null,
    pid: typeof s.pid === 'number' ? s.pid : null,
    uptimeMs: Number.isFinite(started) ? Math.max(0, now - started) : null,
    fds:
      fds && typeof fds.open === 'number' && typeof fds.limit === 'number'
        ? {
            open: fds.open,
            limit: fds.limit,
            acceptErrors: fds.accept_errors ?? 0,
            share: fds.limit > 0 ? fds.open / fds.limit : 0,
          }
        : null,
    reconcileRuns: s.reconcile?.runs ?? 0,
    holds: s.holds?.length ?? 0,
    waiting: Object.values(s.repo_waiting ?? {}).reduce((a, n) => a + n, 0),
    warnings: s.warnings?.length ?? 0,
  };
}

/** "0.4 s", "12 s", "4.2 min", "1.5 h". */
export function duration(ms: number): string {
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)} s`;
  if (s < 60) return `${Math.round(s)} s`;
  const m = s / 60;
  if (m < 60) return `${m < 10 ? m.toFixed(1) : Math.round(m)} min`;
  return `${(m / 60).toFixed(1)} h`;
}

/** "3.2×", "41×", "1,200×". */
export function times(x: number): string {
  return x < 10 ? `${x.toFixed(1)}×` : `${Math.round(x).toLocaleString('en-US')}×`;
}

export function count(n: number): string {
  return n.toLocaleString('en-US');
}
