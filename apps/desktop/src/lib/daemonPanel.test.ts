import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { bytes } from './copy';
import { daemonCopy } from './daemonCopy';
import { classifyWarning } from './statusBar';
import type { DaemonStatus, DuReply, DuScan, DuSession, Usage } from './types';
import {
  activityView,
  age,
  ASSUMED_COPY_RATE,
  copyEstimateMs,
  duration,
  isLive,
  rawJson,
  savingsView,
  scanText,
  scanView,
  sectionLanding,
  statusFacts,
  storageView,
  systemTimeMs,
  times,
  warningSev,
  when,
  type SectionBox,
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

  test('untimed counts the rows made before the daemon recorded creation time', () => {
    expect(savingsView(timedDu())!.untimed).toBe(1);
    expect(savingsView(fx<DuReply>('du'))!.untimed).toBe(2);
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

  test('exact or not reported: live and session bounds stay available', () => {
    for (const v of [savingsView(timedDu())!, savingsView(fx<DuReply>('du'))!]) {
      expect(v.inexact).toBe(false);
      expect([v.live.saved, v.live.onDisk]).not.toContain(null);
      expect(v.rows.map((r) => [r.onDisk, r.saved]).flat()).not.toContain(null);
      expect(v.cumulative.saved).toBeNull(); // 0.2.2 reports no historical accuracy
    }
  });

  // usage.rs: a file with no APFS private size counts its allocated size as private, so the
  // "at least" can overstate and logical − private understate: neither bound holds.
  test('exact false: the live and per-session bounds are null, never a number', () => {
    const du = timedDu();
    const inexact = { ...sess('s-big', 10e9, 1e6, 2500), exact: false } as DuSession;
    du.sessions = {
      all: { ...du.sessions!.all, exact: false } as Usage & { files?: number },
      list: [inexact, sess('s-small', 2e9, 1e6, null)],
    };
    const v = savingsView(du)!;
    expect([v.live.saved, v.live.onDisk]).toEqual([null, null]);
    expect(v.live.logical).toBe(12e9); // logical sizes are exact either way
    // Inexact rows have no saving to rank by: after the exact ones.
    expect(v.rows.map((r) => [r.session, r.onDisk, r.saved])).toEqual([
      ['s-small', 1e6, 2e9 - 1e6],
      ['s-big', null, null],
    ]);
    // Cumulative accuracy is unknown independently of the live figures.
    expect(v.cumulative.saved).toBeNull();
    expect(v.inexact).toBe(true);
    // Time is measured, not derived from private sizes.
    expect(v.timing?.speedup).toBe(2);
    expect(v.rows[1].speedup).toBe(2);
  });

  test('removed inexact history stays unavailable after current totals become exact', () => {
    const du = timedDu();
    du.total = { ...du.total, exact: false } as Usage;
    du.sessions!.all = { ...du.sessions!.all, exact: false } as Usage;
    du.sessions!.list[0] = { ...du.sessions!.list[0], exact: false } as DuSession;
    expect(savingsView(du)!.cumulative.saved).toBeNull();

    // Removing that session restores exact current measurements, but its persisted savings
    // remain in cumulative alongside other removed sessions (du.rs `cumulative`).
    const remaining = { ...du.sessions!.list[1], exact: true } as DuSession;
    du.sessions = {
      all: { ...remaining, files: 1000 },
      list: [remaining],
    };
    du.total = {
      ...du.total,
      logical: remaining.logical,
      private: remaining.private,
      exact: true,
    } as Usage;
    du.savings!.live = {
      ...du.savings!.live,
      logical: remaining.logical,
      private: remaining.private,
      savings: remaining.savings,
    };
    du.savings!.cumulative.removed = 2;
    const v = savingsView(du)!;
    expect(v.cumulative).toEqual({
      saved: null,
      sessions: 3,
      sized: 3,
      unsized: 0,
      removed: 2,
      since: du.savings!.cumulative.since,
    });
    expect(v.live.saved).toBe(10e9 - 1e6);
    expect(v.live.onDisk).toBe(1e6);
    expect(v.rows[0]).toMatchObject({ onDisk: 1e6, saved: 10e9 - 1e6 });
    expect(v.inexact).toBe(false); // Unknown history does not mean current figures are inexact.
    expect(v.timing).toEqual({ timed: 2, avgCreateMs: 2000, copyEstimateMs: 4000, speedup: 2 });
  });

  test('one inexact row alone: its bounds are null and the view says why', () => {
    const du = timedDu();
    du.sessions!.list = [{ ...sess('s-odd', 5e9, 1e9), exact: false } as DuSession];
    const v = savingsView(du)!;
    expect(v.rows[0]).toMatchObject({ onDisk: null, saved: null, logical: 5e9 });
    expect(v.live.saved).toBe(12e9 - 2e6); // sessions.all does not say inexact
    expect(v.inexact).toBe(true);
  });
});

describe('isLive', () => {
  test('a cached reply after a failed poll is not live; an answer, or data with no error, is', () => {
    expect(isLive({ isSuccess: true, isError: false, data: {} })).toBe(true);
    expect(isLive({ isSuccess: false, isError: true, data: {} })).toBe(false);
    expect(isLive({ isSuccess: false, isError: false, data: {} })).toBe(true);
    expect(isLive({ isSuccess: false, isError: false, data: undefined })).toBe(false);
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

const GiB = 1024 ** 3;

/** One `du` group as du.rs `Agg::json` writes it; fresh and exact unless overridden. */
const agg = (o: Record<string, unknown> = {}) => ({
  logical: 0,
  allocated: 0,
  private: 0,
  shared: 0,
  files: 0,
  exact: true,
  errors: 0,
  units: 1,
  stale: 0,
  pending: 0,
  complete: true,
  as_of: '2026-11-13T15:30:00Z',
  ...o,
});

/** The p1 `du` reply with its `total` and `store` patched (types.ts leaves both loose). */
const duWith = (patch: { total?: object; store?: object | null }): DuReply => {
  const p1 = fx<DuReply & { store: Record<string, unknown> }>('du');
  const { store: _store, ...rest } = p1;
  return {
    ...rest,
    total: { ...p1.total, ...patch.total },
    ...(patch.store === null ? {} : { store: { ...p1.store, ...patch.store } }),
  } as DuReply;
};

/** The 2026-11-13 09:27 incident (design §0): 67 TB allocated, 43 GiB private, 33 GiB trash. */
const incidentDu = () =>
  duWith({
    total: agg({ allocated: 67e12, logical: 66e12, private: 43 * GiB, units: 2000 }),
    store: {
      trash: {
        ...agg({ allocated: 66e12, private: 33 * GiB, units: 1796 }),
        entries: 1796,
        eligible: { ...agg({ private: 33 * GiB }), entries: 1790, reclaim_min: 1, reclaim_max: 2 },
        young: { ...agg(), entries: 6 },
        floor_secs: 3600,
        gc: 'git-strata gc',
      },
    },
  });

describe('storageView', () => {
  test('p1 fixture: headline is total.private; groups add up to it, store without trash', () => {
    const v = storageView(fx<DuReply>('du'))!;
    // 270 units still pending: a lower bound that can still grow.
    expect(v.headline).toEqual({
      bytes: 856064,
      state: 'measuring',
      asOf: '2026-10-27T17:31:14.821Z',
    });
    expect(v.groups.map((g) => g.key)).toEqual(['sessions', 'bases', 'deps', 'store', 'trash']);
    const by = Object.fromEntries(v.groups.map((g) => [g.key, g.fig]));
    expect(by.sessions).toEqual({ bytes: 180224, state: 'ok', asOf: '2026-10-27T17:31:14.821Z' });
    // db + blobs + tmp + other; blobs are pending, the oldest measurement is the db's.
    expect(by.store).toEqual({
      bytes: 675840,
      state: 'measuring',
      asOf: '2026-10-27T17:31:16.645Z',
    });
    expect(v.groups.reduce((a, g) => a + (g.fig.bytes ?? 0), 0)).toBe(v.headline.bytes!);
    expect(v.trash).toEqual({
      entries: 4,
      eligible: 0,
      young: 4,
      floorSecs: 3600,
      gc: 'git-strata gc',
    });
    expect([v.allocated, v.logical]).toEqual([937984, 754638]);
  });

  test('exact false ⇒ unavailable, with no number to show', () => {
    const v = storageView(duWith({ total: { exact: false } }))!;
    expect(v.headline).toMatchObject({ bytes: null, state: 'unavailable' });
    const db = storageView(duWith({ store: { db: agg({ private: 9, exact: false }) } }))!;
    expect(db.groups.find((g) => g.key === 'store')!.fig).toMatchObject({
      bytes: null,
      state: 'unavailable',
    });
  });

  test('complete false, pending or stale units ⇒ measuring; fresh and complete ⇒ ok', () => {
    const state = (total: object) => storageView(duWith({ total }))!.headline.state;
    expect(state(agg({ complete: false, pending: 3 }))).toBe('measuring');
    expect(state(agg({ stale: 2 }))).toBe('measuring');
    expect(state(agg())).toBe('ok');
    expect(storageView(duWith({ total: agg({ as_of: null }) }))!.headline.asOf).toBeNull();
  });

  test('incident: the headline is the private bound; allocated is only for Details', () => {
    const v = storageView(incidentDu())!;
    expect(v.headline).toEqual({ bytes: 43 * GiB, state: 'ok', asOf: '2026-11-13T15:30:00Z' });
    const headline = daemonCopy.storage.headline(bytes(v.headline.bytes!));
    expect(headline).toBe(`git-strata alone holds at least ${bytes(43 * GiB)}`);
    expect(headline).not.toContain(bytes(67e12));
    expect(v.allocated).toBe(67e12);
    expect(daemonCopy.storage.headlineTip).toBe(
      'Can be much more: blocks a base shares with its sessions are not counted.'
    );
    expect(daemonCopy.storage.allocated).toContain('counted once per file; overcounts clones');
  });

  test('trash: entries gc may empty, by the human command; never a promise of freed space', () => {
    const v = storageView(incidentDu())!;
    expect(v.trash).toEqual({
      entries: 1796,
      eligible: 1790,
      young: 6,
      floorSecs: 3600,
      gc: 'git-strata gc',
    });
    const s = daemonCopy.storage;
    expect(`${s.trashEligible(1790, age(3600))} ${v.trash!.gc} ${s.trashHuman}`).toBe(
      '1,790 trash entries older than 1 h can be emptied by git-strata gc (a human command).'
    );
    expect(s.trashEligible(1, '1 h')).toStartWith('1 trash entry older');
    const text = [
      ...Object.values(s).filter((x) => typeof x === 'string'),
      s.trashEligible(1790, '1 h'),
      s.trashYoung(6, '1 h'),
    ].join('\n');
    expect(text).not.toMatch(/\bfrees?\b|reclaim/i);
  });

  test('an older du without store figures or trash eligibility shows what it has', () => {
    const v = storageView(duWith({ store: null }))!;
    expect(v.groups.map((g) => g.key)).toEqual(['sessions', 'bases', 'deps']);
    expect(v.trash).toBeNull();
    const noFloor = duWith({ store: { trash: { ...agg(), entries: 2 } } });
    expect(storageView(noFloor)!.trash).toBeNull();
  });

  test('nothing to show without a reply or a total figure', () => {
    expect(storageView(undefined)).toBeNull();
    expect(storageView(duWith({ total: { private: undefined } }))).toBeNull();
  });

  test('age words the gc floor', () => {
    expect([age(3600), age(7200), age(1800), age(45), age(5400)]).toEqual([
      '1 h',
      '2 h',
      '30 min',
      '45 s',
      '90 min',
    ]);
  });
});

describe('scanView', () => {
  const scan = (o: Partial<DuScan> & Record<string, unknown>): DuScan => ({
    running: true,
    queued: 140,
    last_full: null,
    started: '2026-11-13T15:27:00Z',
    done: 412,
    full: false,
    failed: [],
    ...o,
  });
  const fmt = (iso: string) => `<${iso}>`;

  test('running on 0.2.2: counts only, "412 done, 140 queued"', () => {
    expect(scanText(scanView(scan({}))!, fmt)).toBe('Measuring storage · 412 done, 140 queued');
    expect(scanText(scanView(fx<DuReply>('du').scan)!, fmt)).toBe(
      'Measuring storage · 13 done, 253 queued'
    );
    expect(scanText(scanView(scan({ done: 12_345, queued: 1_000 }))!, fmt)).toBe(
      'Measuring storage · 12,345 done, 1,000 queued'
    );
  });

  test('a newer daemon adds units in progress; an older one may lack done', () => {
    expect(scanText(scanView(scan({ active: 4, queued: 136 }))!, fmt)).toBe(
      'Measuring storage · 412 done · 4 in progress · 136 queued'
    );
    expect(scanText(scanView(scan({ done: undefined }))!, fmt)).toBe(
      'Measuring storage · 140 queued'
    );
  });

  test('never a percentage or a time left', () => {
    for (const s of [scan({}), scan({ active: 4 }), scan({ full: true, done: 999, queued: 1 })])
      expect(scanText(scanView(s)!, fmt)).not.toMatch(/%|eta|left|remaining/i);
  });

  test('idle: when the last full pass ended, or just idle', () => {
    const ended = '2026-11-13T15:20:00Z';
    const idle = scan({ running: false, last_full: { started: '2026-11-13T15:00:00Z', ended } });
    expect(scanText(scanView(idle)!, fmt)).toBe(`Idle, last full pass <${ended}>`);
    expect(scanText(scanView(scan({ running: false }))!, fmt)).toBe('Idle');
  });

  test('failed is history: repeats grouped, newest first, malformed entries skipped', () => {
    const failed = [
      { unit: 'sessions/a', error: 'EIO' },
      { unit: 'bases/b', error: 'ENOENT' },
      { unit: 'sessions/a', error: 'EIO' },
      { unit: 7, error: 'x' },
      { unit: 'store/trash/c', error: 'EPERM' },
    ] as DuScan['failed'];
    expect(scanView(scan({ failed }))!.failed).toEqual([
      { unit: 'store/trash/c', error: 'EPERM', times: 1 },
      { unit: 'sessions/a', error: 'EIO', times: 2 },
      { unit: 'bases/b', error: 'ENOENT', times: 1 },
    ]);
  });

  test('no scan object, no view', () => {
    expect(scanView(undefined)).toBeNull();
    expect(scanView({} as DuScan)).toBeNull();
  });
});

describe('activityView', () => {
  const T = 1_794_511_672; // 2026-11-13 09:27:52 America/Winnipeg
  const since = { secs_since_epoch: T, nanos_since_epoch: 500_000_000 };
  const at = (ms: number) => T * 1000 + 500 + ms;
  const status = (o: Partial<DaemonStatus>): DaemonStatus => ({
    ...fx<DaemonStatus>('daemon_status'),
    ...o,
  });

  test('incident: the catch-up hold is daemon work with no session; the queue names it', () => {
    const v = activityView(
      status({
        holds: [{ repo: 'tool', method: 'catch_up', session: null, since }],
        queues: [{ session: 's-eqyyt2k7', holder: 'catch_up', since, queued: ['sync'] }],
        repo_waiting: { tool: 2, web: 0 },
      }),
      at(150_000)
    );
    expect(v.holds).toEqual([
      { repo: 'tool', method: 'catch_up', background: true, session: null, elapsedMs: 150_000 },
    ]);
    expect(v.queues).toEqual([
      {
        session: 's-eqyyt2k7',
        holder: 'catch_up',
        background: true,
        queued: ['sync'],
        elapsedMs: 150_000,
      },
    ]);
    expect(v.waiting).toEqual([{ repo: 'tool', n: 2 }]);
    expect(daemonCopy.activity.holder('catch_up', true)).toBe('Daemon work: auto-rebase');
  });

  test('a request hold is the method itself; old-tree and reconcile are daemon work', () => {
    const v = activityView(
      status({
        holds: [
          { repo: 'web', method: 'publish', session: 's1', since },
          { repo: 'tool', method: 'old_tree', session: null, since },
        ],
        queues: [{ session: 's2', holder: 'reconcile', since, queued: [] }],
      }),
      at(0)
    );
    expect(v.holds.map((h) => [h.method, h.background, h.session])).toEqual([
      ['publish', false, 's1'],
      ['old_tree', true, null],
    ]);
    expect(v.queues[0].background).toBe(true);
    const label = daemonCopy.activity.holder;
    expect([label('publish', false), label('old_tree', true), label('daemon', true)]).toEqual([
      'publish',
      'Daemon work: cleaning an old tree',
      'Daemon work',
    ]);
  });

  test('a newer daemon: origin decides, and its elapsed_ms is used', () => {
    const v = activityView(
      status({
        holds: [
          {
            repo: 'tool',
            method: 'new',
            session: null,
            since,
            origin: 'background',
            elapsed_ms: 5000,
          },
          { repo: 'web', method: 'catch_up', session: null, since, origin: 'request' },
        ],
      }),
      at(60_000)
    );
    expect(v.holds.map((h) => [h.method, h.background, h.elapsedMs])).toEqual([
      ['catch_up', false, 60_000],
      ['new', true, 5000],
    ]);
  });

  test('longest first; malformed entries and a missing queues key are skipped', () => {
    const later = { secs_since_epoch: T + 60, nanos_since_epoch: 500_000_000 };
    const v = activityView(
      status({
        holds: [
          { repo: 'a', method: 'sync', session: 's', since: later },
          null,
          { repo: 1, method: 'x' },
          { repo: 'b', method: 'sync', session: 's', since },
        ],
      }),
      at(120_000)
    );
    expect(v.holds.map((h) => [h.repo, h.elapsedMs])).toEqual([
      ['b', 120_000],
      ['a', 60_000],
    ]);
    expect(v.queues).toEqual([]);
    expect(systemTimeMs(since)).toBe(T * 1000 + 500);
    expect(systemTimeMs({ secs_since_epoch: '1' })).toBeNull();
  });
});

describe('warnings', () => {
  const view = (cls: 'error' | 'waiting' | 'notice' | 'activity', sev: string, kind = 'X') =>
    warningSev({ kind, cls, sev: sev as 'red' });

  test('red only for the error class, never for a LongHold; activity is blue', () => {
    expect(view('error', 'red', 'StorePoisoned')).toBe('red');
    expect(view('error', 'amber', 'VerifyFailed')).toBe('red');
    expect(view('error', 'red', 'LongHold')).toBe('amber');
    expect(view('notice', 'red', 'LongHold')).toBe('amber');
    expect(view('activity', 'amber', 'LongHold')).toBe('blue');
    expect(view('notice', 'red')).toBe('amber');
    expect(view('waiting', 'amber')).toBe('amber');
    expect(view('notice', 'muted')).toBe('muted');
  });

  test('the incident LongHold through the shared classifier: one line, never red', () => {
    const since = { secs_since_epoch: 1_794_511_672, nanos_since_epoch: 0 };
    const raw = { LongHold: { repo: 'tool', method: 'catch_up', since } };
    const w = classifyWarning(raw, 1_794_511_672_000 + 120_000);
    expect(w.kind).toBe('LongHold');
    expect(warningSev(w)).not.toBe('red');
    expect(w.text).not.toMatch(/[{}"]|secs_since_epoch/);
    expect(w.raw).toEqual(raw);
  });

  test('rawJson: the raw entry, pretty, for Details only', () => {
    expect(rawJson('StorePoisoned')).toBe('"StorePoisoned"');
    expect(rawJson({ FetchFailed: { repo: 'tool' } })).toBe(
      '{\n  "FetchFailed": {\n    "repo": "tool"\n  }\n}'
    );
    expect(rawJson(undefined)).toBe('undefined');
  });
});

describe('sectionLanding (a chip opens the modal at its section)', () => {
  // The modal body's children as laid out: the Now list (no data-section) first.
  const body: SectionBox[] = [
    { name: undefined, offsetTop: 0 },
    { name: 'warnings', offsetTop: 180 },
    { name: 'activity', offsetTop: 420 },
    { name: 'daemon', offsetTop: 900 },
  ];

  test('lands on the section and stops', () => {
    expect(sectionLanding(body, 'activity', 0)).toEqual({ top: 420, done: true });
    expect(sectionLanding(body, 'warnings', 0)).toEqual({ top: 180, done: true });
  });

  test('no section asked for: the top, nothing to do', () => {
    expect(sectionLanding(body, undefined, 0)).toEqual({ top: null, done: true });
  });

  test('a section that is not there leaves the view at the top and tries again', () => {
    expect(sectionLanding(body, 'scan', 0)).toEqual({ top: null, done: false });
    expect(sectionLanding([], 'scan', 0)).toEqual({ top: null, done: false });
  });

  test('once the reader has scrolled, nothing moves the view', () => {
    expect(sectionLanding(body, 'activity', 35)).toEqual({ top: null, done: true });
  });

  /** The modal's effect over a run of renders: lands at most once, then never moves. */
  const open = (section: string | undefined, renders: Array<SectionBox[] | 'scroll'>) => {
    let scrollTop = 0;
    let landed = false;
    const tops: number[] = [];
    for (const r of renders) {
      if (r === 'scroll') {
        scrollTop += 50; // the reader scrolls
        continue;
      }
      if (landed) continue;
      const { top, done } = sectionLanding(r, section, scrollTop);
      if (top !== null) tops.push((scrollTop = top));
      landed = done;
    }
    return tops;
  };
  const scan = { name: 'scan', offsetTop: 700 };

  test('renders: the section comes later (du answered) → lands then, once', () => {
    expect(open('scan', [body, body, [...body, scan], [...body, scan]])).toEqual([700]);
  });

  test('renders: the section goes and comes back with polls → no second jump', () => {
    const noActivity = body.filter((c) => c.name !== 'activity');
    expect(open('activity', [body, noActivity, body])).toEqual([420]);
  });

  test('renders: the reader scrolls before the section shows up → the view stays', () => {
    expect(open('scan', [body, 'scroll', [...body, scan]])).toEqual([]);
  });
});

describe('format', () => {
  test('when: a dash without a valid time', () => {
    expect(when(null)).toBe('—');
    expect(when('not a time')).toBe('—');
    expect(when('2026-11-13T15:27:52Z')).not.toBe('—');
  });
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
