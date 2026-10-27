import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { DaemonStatus, DuReply, DuSession, Usage } from './types';
import {
  ASSUMED_COPY_RATE,
  copyEstimateMs,
  duration,
  savingsView,
  statusFacts,
  times,
} from './daemonPanel';

const fx = <T>(name: string): T =>
  JSON.parse(
    readFileSync(join(import.meta.dir, '../../src-tauri/tests/fixtures/p1', `${name}.json`), 'utf8')
  ) as T;

const usage = (o: Partial<Usage> = {}): Usage => ({
  allocated: 0,
  logical: 0,
  private: 0,
  shared: 0,
  as_of: '2026-10-27T19:57:48.469Z',
  complete: true,
  pending: 0,
  units: 2,
  ...o,
});

const sess = (
  session: string,
  logical: number,
  priv: number,
  create_ms?: number | null
): DuSession => ({
  ...usage({ logical, private: priv }),
  session,
  files: 1000,
  savings: Math.max(0, logical - priv),
  ...(create_ms === undefined ? {} : { create_ms }),
});

/** A newer daemon: two live sessions, one timed; cumulative has the timing sums. */
const timedDu = (): DuReply => {
  const p1 = fx<DuReply>('du');
  return {
    ...p1,
    sessions: {
      all: { ...usage({ logical: 12e9, private: 2e6 }), files: 2000 },
      list: [sess('s-small', 2e9, 1e6, null), sess('s-big', 10e9, 1e6, 2500)],
    },
    savings: {
      live: { logical: 12e9, private: 2e6, savings: 12e9 - 2e6, as_of: null, complete: true },
      cumulative: {
        since: '2026-10-27T13:30:13.269Z',
        sessions: 3,
        sized: 3,
        unsized: 0,
        removed: 1,
        logical: 14e9,
        private: 3e6,
        savings: 14e9 - 3e6,
        timed: 2,
        create_ms: 4000,
        timed_logical: 16e9,
        timed_files: 9000,
      },
    },
  };
};

describe('savingsView', () => {
  test('a daemon without the du savings report gives nothing, not zeros', () => {
    const { savings: _s, sessions: _l, ...old } = fx<DuReply>('du');
    expect(savingsView(old as DuReply)).toBeNull();
    expect(savingsView(undefined)).toBeNull();
  });

  test('p1 fixture: figures pass through; no create_ms means an older daemon', () => {
    const v = savingsView(fx<DuReply>('du'))!;
    expect(v.live.saved).toBe(0); // tiny files: private ≥ logical, nothing saved
    expect(v.live.sessions).toBe(2);
    expect(v.live.files).toBe(64);
    expect(v.cumulative.sessions).toBe(2);
    expect(v.timing).toBeNull();
    expect(v.timingMissing).toBe('old-daemon');
    expect(v.rows.every((r) => r.createMs === null && r.speedup === null)).toBe(true);
  });

  test('timed sessions: measured average against the copy estimate of the same sessions', () => {
    const v = savingsView(timedDu())!;
    expect(v.timing).toEqual({
      timed: 2,
      avgCreateMs: 2000,
      copyEstimateMs: 4000, // 16 GB ÷ 2 GB/s ÷ 2 sessions
      speedup: 2, // 8000 ms estimated ÷ 4000 ms measured
    });
    expect(v.live.saved).toBe(12e9 - 2e6);
    expect(v.live.copyEstimateMs).toBe(6000);
  });

  test('rows: biggest saving first; speed-up only where new was timed', () => {
    const v = savingsView(timedDu())!;
    expect(v.rows.map((r) => r.session)).toEqual(['s-big', 's-small']);
    expect(v.rows[0].speedup).toBe(5000 / 2500);
    expect(v.rows[1].createMs).toBeNull();
    expect(v.rows[1].speedup).toBeNull();
  });

  test('a daemon that times new but has no timed session yet says so', () => {
    const du = timedDu();
    du.savings!.cumulative = {
      ...du.savings!.cumulative,
      timed: 0,
      create_ms: 0,
      timed_logical: 0,
    };
    const v = savingsView(du)!;
    expect(v.timing).toBeNull();
    expect(v.timingMissing).toBe('none-timed');
  });

  test('a zero-ms create gives no speed-up rather than infinity', () => {
    const du = timedDu();
    du.sessions!.list = [sess('s-zero', 1e9, 0, 0)];
    expect(savingsView(du)!.rows[0].speedup).toBeNull();
  });

  test('the estimate is logical bytes over the stated rate', () => {
    expect(ASSUMED_COPY_RATE).toBe(2e9);
    expect(copyEstimateMs(2e9)).toBe(1000);
    expect(copyEstimateMs(-5)).toBe(0);
  });
});

describe('statusFacts', () => {
  test('p1 daemon_status: no version, pid, uptime or fds; counts from the reply', () => {
    const f = statusFacts(fx<DaemonStatus>('daemon_status'), Date.now());
    expect(f).toEqual({
      version: null,
      api: null,
      pid: null,
      uptimeMs: null,
      fds: null,
      reconcileRuns: 0,
      holds: 0,
      waiting: 0,
      warnings: 0,
    });
  });

  test('a daemon with fds, version, pid and start time', () => {
    const s: DaemonStatus = {
      ...fx<DaemonStatus>('daemon_status'),
      repo_waiting: { tool: 2, web: 1 },
      fds: { open: 36, limit: 10240, accept_errors: 0 },
      version: '0.1.0',
      api: 2,
      pid: 4242,
      started: '2026-10-27T13:00:00Z',
    };
    const f = statusFacts(s, Date.parse('2026-10-27T14:30:00Z'));
    expect(f.fds).toEqual({ open: 36, limit: 10240, acceptErrors: 0, share: 36 / 10240 });
    expect(f.uptimeMs).toBe(90 * 60 * 1000);
    expect([f.version, f.api, f.pid, f.waiting]).toEqual(['0.1.0', 2, 4242, 3]);
  });
});

describe('format', () => {
  test('duration', () => {
    expect(duration(400)).toBe('0.4 s');
    expect(duration(12_000)).toBe('12 s');
    expect(duration(254_000)).toBe('4.2 min');
    expect(duration(5_400_000)).toBe('1.5 h');
  });
  test('times', () => {
    expect(times(3.24)).toBe('3.2×');
    expect(times(1234.4)).toBe('1,234×');
  });
});
