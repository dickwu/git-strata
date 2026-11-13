import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import appPackage from '../../package.json';
import incident from './fixtures/statusBar/incident-0927.json';
import pbFixture from './fixtures/statusBar/pb.json';
import { copy } from './copy';
import {
  BACKGROUND_HOLD_MAX_MS,
  MAX_CHIPS,
  capChips,
  classifyWarning,
  statusBarModel,
  type Chip,
  type StatusBarInput,
} from './statusBar';
import type { DaemonActivity, DaemonStatus, DepsList, DuReply, StatusReply } from './types';
import { waitingCount } from './view';

const NOW = Date.parse(incident.now);
const { version } = appPackage;
const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;
const stamp = (ms: number) => ({
  secs_since_epoch: Math.floor(ms / 1000),
  nanos_since_epoch: Math.round((ms % 1000) * 1_000_000),
});
const fx = <T>(name: string): T =>
  JSON.parse(
    readFileSync(join(import.meta.dir, '../../src-tauri/tests/fixtures/p1', `${name}.json`), 'utf8')
  ) as T;
const daemon = (warnings: unknown[] = []): DaemonStatus => ({
  holds: [],
  queues: [],
  reconcile: { runs: 0, last: null },
  repo_waiting: {},
  verify: null,
  warnings,
});
const input = (data = daemon()): StatusBarInput => ({
  daemon: { pending: false, data, error: null, updatedAt: NOW },
  sessions: undefined,
  sessionsUpdatedAt: NOW,
  deps: undefined,
  du: undefined,
});
const allChips = (i: StatusBarInput, now = NOW) => {
  const m = statusBarModel(i, now);
  return [...m.chips, ...m.overflow];
};
const hold = (method = 'catch_up', age = 2 * MINUTE, extra = {}) => ({
  LongHold: { repo: 'tool', method, since: stamp(NOW - age), ...extra },
});
const wait = (age: number) => ({
  LongWait: {
    repo: 'tool',
    session: 's-waiting',
    method: 'publish',
    since: stamp(NOW - 2 * 60 * MINUTE),
    attempt_since: stamp(NOW - age),
  },
});
const stuck = (reason: string, age = MINUTE) => ({
  AutoRebaseStuck: {
    repo: 'tool',
    session: 's-stuck',
    reason,
    since: stamp(NOW - age),
    attempts: 3,
    rev: 1,
  },
});

describe('warning classification', () => {
  test.each([
    'StorePoisoned',
    'VerifyFailed',
    'UnknownPublish',
    'UnknownPush',
    'Quarantine',
    'AttachOutside',
  ])('%s is an Error warning', (kind) => {
    const w = kind === 'StorePoisoned' ? kind : { [kind]: {} };
    expect(classifyWarning(w, NOW)).toMatchObject({ kind, cls: 'error', sev: 'red', raw: w });
  });

  test('TS Error variants equal the Rust warning_class literal list', () => {
    const rust = readFileSync(join(import.meta.dir, '../../src-tauri/src/tray_model.rs'), 'utf8');
    const arm = rust.match(/((?:"[A-Za-z]+"\s*\|\s*)*"[A-Za-z]+")\s*=>\s*WarningClass::Error/);
    expect(arm).not.toBeNull();
    const rustNames = [...arm![1].matchAll(/"([A-Za-z]+)"/g)].map((m) => m[1]).sort();
    const ts = readFileSync(join(import.meta.dir, 'statusBar.ts'), 'utf8');
    const list = ts.match(/const ERROR_WARNING_KINDS[^=]*=\s*\[([\s\S]*?)\]/);
    expect(list).not.toBeNull();
    const tsNames = [...list![1].matchAll(/'([A-Za-z]+)'/g)].map((m) => m[1]).sort();
    expect(tsNames).toEqual(rustNames);
  });

  test.each([
    ['catch_up', 'Auto-rebase work · tool · 2 min'],
    ['old_tree', 'Cleaning old tree · tool · 2 min'],
    ['periodic_fetch', 'Fetching · tool'],
    ['daemon', 'tool is busy with daemon work · 2 min'],
  ])('%s is blue activity based on its own method', (method, text) => {
    expect(classifyWarning(hold(method), NOW)).toMatchObject({
      cls: 'activity',
      sev: 'blue',
      text,
    });
  });

  test.each(['publish', 'push', 'sync', 'new', 'future_request'])(
    '%s request holds stay amber',
    (method) => {
      expect(classifyWarning(hold(method), NOW)).toMatchObject({
        cls: 'notice',
        sev: 'amber',
        text: `tool locked by ${method} for 2 min`,
      });
    }
  );

  test.each(['catch_up', 'old_tree', 'periodic_fetch', 'daemon'])(
    '%s background holds turn amber at 30 minutes, never red',
    (method) => {
      expect(classifyWarning(hold(method, BACKGROUND_HOLD_MAX_MS - 1), NOW).sev).toBe('blue');
      expect(classifyWarning(hold(method, BACKGROUND_HOLD_MAX_MS), NOW)).toMatchObject({
        cls: 'notice',
        sev: 'amber',
      });
    }
  );

  test.each(['background', 'request'])('P-b LongHold.origin=%s means abnormal amber', (origin) => {
    expect(classifyWarning(hold('catch_up', MINUTE, { origin }), NOW)).toMatchObject({
      cls: 'notice',
      sev: 'amber',
    });
  });

  test('null LongHold.origin keeps the 0.2.2 fallback', () => {
    expect(classifyWarning(hold('catch_up', MINUTE, { origin: null }), NOW)).toMatchObject({
      cls: 'activity',
      sev: 'blue',
    });
  });

  test('LongWait uses the current attempt, and old attempts are modal-only', () => {
    expect(classifyWarning(wait(2 * MINUTE), NOW)).toMatchObject({
      cls: 'notice',
      sev: 'amber',
      since: NOW - 2 * MINUTE,
      text: 'A request is waiting on tool (current attempt 2 min)',
    });
    expect(allChips(input(daemon([wait(2 * MINUTE)])))[0].sev).toBe('amber');
    expect(classifyWarning(wait(15 * MINUTE), NOW).sev).toBe('muted');
    expect(allChips(input(daemon([wait(10 * MINUTE)])))).toEqual([]);
    expect(allChips(input(daemon([wait(15 * MINUTE)])))).toEqual([]);
  });

  test.each(['open_files', 'failed', 'error', 'install_failed', 'unknown_reason'])(
    'AutoRebaseStuck %s needs attention immediately',
    (reason) => expect(classifyWarning(stuck(reason), NOW).sev).toBe('amber')
  );
  test('open-files stuck copy names the repo, session and action reason', () => {
    expect(classifyWarning(stuck('open_files'), NOW).text).toBe(
      'Auto-rebase stuck: tool · s-stuck (open files)'
    );
  });
  test.each(['recent_writes', 'changed_during_move', 'busy', 'installing', 'waiting_install'])(
    'AutoRebaseStuck %s is blue until one day, then amber',
    (reason) => {
      expect(classifyWarning(stuck(reason, DAY - 1), NOW)).toMatchObject({
        cls: 'activity',
        sev: 'blue',
        text: `Auto-rebase waiting · tool · s-stuck (${reason.replace(/_/g, ' ')})`,
      });
      expect(classifyWarning(stuck(reason, DAY), NOW)).toMatchObject({
        cls: 'notice',
        sev: 'amber',
      });
    }
  );

  test('config, fetching and modal-only variants all get human copy', () => {
    const warnings = [
      { ConfigRelaxPending: { repo: 'tool' } },
      { ConfigReloadFailed: { reason: 'invalid path' } },
      { ConfigRestartRequired: { fields: ['root', 'socket'] } },
      { FetchFailed: { repo: 'tool' } },
      { AnchorNotAdvanced: { repo: 'tool' } },
      { CatalogGrowth: { generations: 42 } },
      { StaleRefLock: { repo: 'tool', path: 'x.lock' } },
      { PublishRunning: { id: 7 } },
    ];
    const views = warnings.map((w) => classifyWarning(w, NOW));
    expect(views[0]).toMatchObject({ cls: 'waiting', sev: 'amber' });
    expect(views[1].text).toBe('config.toml not applied: invalid path');
    expect(views[2].text).toBe('Restart the daemon to apply: root, socket');
    expect(views.slice(3, 5).map((v) => [v.sev, v.text])).toEqual([
      ['amber', 'Fetching tool failed'],
      ['amber', 'Fetching tool failed'],
    ]);
    expect(views.slice(5).every((v) => v.cls === 'notice' && v.sev === 'muted')).toBe(true);
  });

  test('malformed and future warning shapes remain safe modal notices', () => {
    for (const w of [null, 42, [], {}, { A: {}, B: {} }, { FutureWarning: null }]) {
      expect(classifyWarning(w, NOW)).toMatchObject({ cls: 'notice', sev: 'muted', raw: w });
    }
    expect(classifyWarning({ LongHold: null }, NOW).sev).not.toBe('red');
  });
});

describe('P-a activity and real incident', () => {
  const incidentInput = () => ({
    ...input(structuredClone(incident.daemon)),
    du: structuredClone(incident.du),
  });
  test('09:27 incident has one blue tool/session chip, no red and no allocated-size copy', () => {
    const model = statusBarModel(incidentInput(), NOW);
    const blue = model.chips.filter((c) => c.sev === 'blue');
    expect(blue).toHaveLength(1);
    expect(blue[0].text).toContain('Auto-rebase work · tool · s-eqyyt2k7');
    expect(blue[0].target).toEqual({ kind: 'modal', section: 'daemon' });
    expect(model.chips.some((c) => c.sev === 'red')).toBe(false);
    expect(JSON.stringify(model)).not.toMatch(/allocated|67 TB|67000000000000/);
    expect(model.chips.find((c) => c.id === 'scan')?.text).toBe(
      'Measuring storage · 412 done, 140 queued'
    );
    expect(model.right.free).toBeNull();
  });

  test('a hold snapshot match is unnecessary', () => {
    const i = incidentInput();
    i.daemon.data!.holds = [];
    expect(allChips(i).find((c) => c.sev === 'blue')?.text).toContain('s-eqyyt2k7');
  });

  test('ambiguous catch_up queues omit the session', () => {
    const i = incidentInput();
    i.daemon.data!.queues!.push({ ...i.daemon.data!.queues![0], session: 's-other' });
    const text = allChips(i).find((c) => c.sev === 'blue')!.text;
    expect(text).toContain('Auto-rebase work · tool');
    expect(text).not.toContain('s-eqyyt2k7');
    expect(text).not.toContain('s-other');
  });

  test('queue comparison includes nanoseconds, not rounded milliseconds', () => {
    const i = incidentInput();
    const queue = i.daemon.data!.queues![0];
    queue.since!.nanos_since_epoch = 501;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).not.toContain(queue.session);
    queue.since!.nanos_since_epoch = 499;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).toContain(queue.session);
    queue.since = null;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).not.toContain(queue.session);
  });

  test('new-daemon feature marks every LongHold amber', () => {
    const i = input(daemon([hold()]));
    i.daemon.data!.features = ['daemon_status.activity'];
    expect(allChips(i)).toHaveLength(1);
    expect(allChips(i)[0]).toMatchObject({
      rank: 4,
      sev: 'amber',
      target: { kind: 'modal', section: 'warnings' },
    });
  });

  test('warning chip identities survive unrelated warning insertion and distinguish waiting methods', () => {
    const i = input(daemon([hold('publish')]));
    const id = allChips(i)[0].id;
    i.daemon.data!.warnings.unshift({ FetchFailed: { repo: 'web' } });
    expect(allChips(i).find((c) => c.text.includes('locked by publish'))!.id).toBe(id);
    i.daemon.data!.warnings = [
      wait(MINUTE),
      {
        LongWait: { ...wait(MINUTE).LongWait, method: 'sync' },
      },
    ];
    expect(new Set(allChips(i).map((c) => c.id)).size).toBe(2);
  });

  test('catch-up beats an older deferred row; only its chip gets lock counts and tooltip', () => {
    const i = input(daemon([stuck('recent_writes', 10 * MINUTE), hold()]));
    i.daemon.data!.queues = [
      { session: 's-eqyyt2k7', holder: 'catch_up', since: stamp(NOW - 3 * MINUTE), queued: [] },
    ];
    i.daemon.data!.features = ['status.upstream'];
    i.daemon.data!.repo_waiting = { tool: 2 };
    i.sessions = fx<StatusReply>('status_all');
    for (const s of i.sessions.sessions)
      s.status.upstream = {
        clean: false,
        repos: {
          tool: {
            behind: 1,
            auto: { source: 'main', state: 'pending', since: incident.now, attempts: 0 },
          },
        },
      };
    expect(allChips(i).filter((c) => c.sev === 'blue')).toHaveLength(1);
    const chip = allChips(i).find((c) => c.sev === 'blue')!;
    expect(chip.text).toBe(
      'Auto-rebase work · tool · s-eqyyt2k7 · 2 min · 2 waiting · 2 pending · 1 deferred'
    );
    expect(chip.tooltip).toContain('waits 30 seconds before writing gets Busy');
    i.daemon.data!.warnings.reverse();
    expect(allChips(i).find((c) => c.sev === 'blue')).toEqual(chip);
    i.sessionsUpdatedAt = NOW - 301_000;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).not.toContain('pending');
    i.sessionsUpdatedAt = undefined;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).not.toContain('pending');
    i.sessionsUpdatedAt = NOW - 91_000;
    i.daemon.data!.api = 1;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).not.toContain('pending');
  });

  test('catch-up wins by kind even when another background hold is older', () => {
    const i = input(daemon([hold('daemon', 5 * MINUTE), hold('catch_up')]));
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).toContain('Auto-rebase work');
  });

  test('a deferred row without a LongHold has no lock counts or lock tooltip', () => {
    const i = input(daemon([stuck('changed_during_move', 10 * MINUTE)]));
    i.daemon.data!.repo_waiting = { tool: 2 };
    expect(allChips(i)[0]).toMatchObject({
      text: 'Auto-rebase waiting · tool · s-stuck (changed during move)',
      tooltip: undefined,
    });
  });

  test('sub-three-second activity does not flash', () => {
    expect(allChips(input(daemon([hold('catch_up', 2999)])))).toEqual([]);
    expect(allChips(input(daemon([hold('catch_up', 3000)])))[0].sev).toBe('blue');
  });
});

describe('health, waiting and honest facts', () => {
  const err = (why: string, detail = {}) => ({
    code: 'Busy',
    message: 'probe failed',
    detail: { reason: why, ...detail },
  });
  test('connecting and not running are distinct, without duplicate trigger chips', () => {
    const i = input();
    i.daemon = { pending: true, data: undefined, error: null, updatedAt: 0 };
    expect(statusBarModel(i, NOW).daemon).toEqual({
      state: 'pending',
      label: 'Connecting to daemon',
      detail: null,
    });
    expect(allChips(i)).toEqual([]);
    i.daemon.pending = false;
    i.daemon.error = err('daemon_down');
    expect(statusBarModel(i, NOW).daemon.label).toBe("Daemon isn't running");
    expect(allChips(i)).toEqual([]);
  });

  test('not responding requires two health timeouts AND 20 seconds since the last Ok', () => {
    const i = input();
    i.daemon.error = err('daemon_timeout', { seconds: 8 });
    i.health = { timeouts: 1, lastOkAt: NOW };
    expect(statusBarModel(i, NOW + 25_000).daemon.state).toBe('pending');
    i.health.timeouts = 2;
    expect(statusBarModel(i, NOW + 19_999).daemon.state).toBe('pending');
    expect(statusBarModel(i, NOW + 20_000).daemon).toEqual({
      state: 'down',
      label: "Daemon isn't responding",
      detail: copy.daemon.stuck(8),
    });
    i.daemon.error = null;
    i.daemon.updatedAt = NOW + 21_000;
    i.health = { timeouts: 0, lastOkAt: NOW + 21_000 };
    expect(statusBarModel(i, NOW + 21_000).daemon.state).toBe('up');
    i.daemon.error = err('daemon_timeout');
    i.health.timeouts = 1;
    expect(statusBarModel(i, NOW + 45_000).daemon.state).toBe('pending');
  });

  test('timeouts without an earlier Ok still require two observations; omitted health fails neutral', () => {
    const i = input();
    i.daemon.data = undefined;
    i.daemon.updatedAt = 0;
    i.daemon.error = err('daemon_timeout');
    expect(statusBarModel(i, NOW).daemon.state).toBe('pending');
    i.health = { timeouts: 1, lastOkAt: 0 };
    expect(statusBarModel(i, NOW).daemon.state).toBe('pending');
    i.health.timeouts = 2;
    expect(statusBarModel(i, NOW).daemon.state).toBe('down');
  });

  test('an escalated joined read describes its unanswered request; below threshold stays neutral', () => {
    const i = input();
    i.daemon.error = err('request_in_flight');
    i.health = { timeouts: 2, lastOkAt: NOW };
    expect(statusBarModel(i, NOW + 19_999).daemon).toEqual({
      state: 'pending',
      label: 'Checking the daemon…',
      detail: copy.daemon.busy,
    });
    expect(statusBarModel(i, NOW + 20_000).daemon).toEqual({
      state: 'down',
      label: "Daemon isn't responding",
      detail: 'An earlier request is still waiting for the daemon.',
    });
  });

  test.each(['request_in_flight', 'not_tauri', 'shed'])(
    '%s without timeout evidence is neutral',
    (why) => {
      const i = input(daemon([{ FetchFailed: { repo: 'tool' } }]));
      i.daemon.error = err(why);
      i.health = { timeouts: 0, lastOkAt: NOW };
      const model = statusBarModel(i, NOW + 5000);
      expect(model.daemon).toMatchObject({ state: 'pending', label: 'Checking the daemon…' });
      expect(allChips(i, NOW + 5000)).toHaveLength(1);
      expect(allChips(i, NOW + 5000)[0]).toMatchObject({
        text: 'Fetching tool failed',
        target: { kind: 'modal', section: 'daemon' },
      });
    }
  );

  test.each([1, 4, 13])('store permission errno %s uses Removable Volumes guidance', (errno) => {
    const i = input();
    i.daemon.error = err('store_unavailable', { last_error: { detail: { errno } } });
    expect(statusBarModel(i, NOW).daemon.label).toBe('Daemon waiting for its data volume');
    expect(statusBarModel(i, NOW).daemon.detail).toBeNull();
    expect(allChips(i)[0]).toMatchObject({ sev: 'red', rank: 1 });
    expect(allChips(i)[0].text).toContain('Waiting for its data volume: allow Removable Volumes');
  });
  test.each([2, 5, undefined])(
    'non-permission store errno %s means unavailable data volume',
    (errno) => {
      const i = input();
      i.daemon.error = err('store_unavailable', { last_error: { detail: { errno } } });
      expect(allChips(i)[0].text).toBe('Data volume not available');
    }
  );
  test('starting is muted recovery, not daemon failure', () => {
    const i = input();
    i.daemon.error = err('starting');
    expect(statusBarModel(i, NOW).daemon.state).toBe('pending');
    expect(statusBarModel(i, NOW).daemon.label).toBe('Daemon starting (recovering)');
    expect(allChips(i)).toEqual([]);
  });

  test('stale health is muted and does not present old warnings/activity as current', () => {
    const i = input(daemon(['StorePoisoned', hold()]));
    i.daemon.updatedAt = NOW - 120_000;
    expect(allChips(i)).toEqual([]);
    expect(statusBarModel(i, NOW).daemon.detail).toBe('Not updated for 2 min');
    expect(statusBarModel(i, NOW).daemon.state).toBe('pending');
    i.daemon.updatedAt = NOW - 60_000;
    expect(allChips(i).some((c) => c.sev === 'red')).toBe(true);
  });

  test.each(['request_in_flight', 'daemon_timeout'])(
    '%s below the Down threshold shows stale age after 60 s',
    (why) => {
      const i = input(daemon(['StorePoisoned', hold()]));
      i.daemon.error = err(why, { seconds: 8 });
      i.health = { timeouts: why === 'daemon_timeout' ? 1 : 0, lastOkAt: NOW };
      expect(statusBarModel(i, NOW + 60_000).daemon.detail).toBe(
        why === 'daemon_timeout' ? copy.daemon.stuck(8) : copy.daemon.busy
      );
      for (const [elapsed, detail] of [
        [60_001, 'Not updated for 1 min'],
        [179_999, 'Not updated for 2 min'],
        [3_600_000, 'Not updated for 1 h'],
      ] as const) {
        expect(statusBarModel(i, NOW + elapsed).daemon).toEqual({
          state: 'pending',
          label: 'Checking the daemon…',
          detail,
        });
        expect(allChips(i, NOW + elapsed)).toEqual([]);
      }
      i.daemon.data = undefined;
      i.daemon.updatedAt = 0;
      i.health.lastOkAt = 0;
      expect(statusBarModel(i, NOW + 3_600_000).daemon.detail).toBe(
        why === 'daemon_timeout' ? copy.daemon.stuck(8) : copy.daemon.busy
      );
    }
  );

  test('file handles match tray 90% threshold; historical accept_errors alone are not pressure', () => {
    const i = input();
    i.daemon.data!.fds = { open: 89, limit: 100, accept_errors: 15 };
    expect(allChips(i)).toEqual([]);
    i.daemon.data!.fds.open = 90;
    expect(allChips(i)[0]).toMatchObject({
      sev: 'red',
      text: 'Daemon is close to its file-handle limit',
    });
    i.daemon.data!.fds.limit = 0;
    expect(allChips(i)).toEqual([]);
    i.daemon.error = err('too_many_open_files');
    expect(statusBarModel(i, NOW).daemon).toMatchObject({
      state: 'down',
      label: 'Daemon is out of file handles',
    });
    expect(allChips(i)).toEqual([]);
  });

  test('increasing accept_errors is pressure below 90%; a reset is not', () => {
    const i = input();
    i.daemon.data!.fds = { open: 10, limit: 100, accept_errors: 15 };
    expect(allChips(i)).toEqual([]);
    i.health = { timeouts: 0, lastOkAt: NOW, prevAcceptErrors: 15 };
    expect(allChips(i)).toEqual([]);
    i.daemon.data!.fds.accept_errors = 16;
    expect(allChips(i)[0]).toMatchObject({
      id: 'fds',
      sev: 'red',
      target: { kind: 'modal', section: 'daemon' },
    });
    i.health.prevAcceptErrors = 16;
    i.daemon.data!.fds.accept_errors = 0;
    expect(allChips(i)).toEqual([]);
    i.health.prevAcceptErrors = 0;
    i.daemon.data!.fds.accept_errors = 1;
    expect(allChips(i)[0].id).toBe('fds');
  });

  test('FetchFailed and AnchorNotAdvanced deduplicate per repo', () => {
    const i = input(
      daemon([
        { FetchFailed: { repo: 'tool' } },
        { AnchorNotAdvanced: { repo: 'tool' } },
        { FetchFailed: { repo: 'web' } },
      ])
    );
    expect(allChips(i).map((c) => c.text)).toEqual(['Fetching tool failed', 'Fetching web failed']);
    expect(new Set(allChips(i).map((c) => c.id)).size).toBe(2);
  });

  test('a daemon reply without reconcile renders safely', () => {
    const data = daemon() as Partial<DaemonStatus>;
    delete data.reconcile;
    expect(allChips(input(data as DaemonStatus))).toEqual([]);
  });

  test('sessions_audit_error describes an audit-log write failure and has an existing target', () => {
    const i = input({ ...daemon(), sessions_audit_error: 'disk full' } as DaemonStatus);
    expect(allChips(i)[0]).toMatchObject({
      text: 'The sessions audit log could not be written',
      target: { kind: 'modal', section: 'daemon' },
    });
  });

  test('Error warnings aggregate into one red needs-attention chip', () => {
    const i = input(daemon(['StorePoisoned', { VerifyFailed: { message: 'bad' } }]));
    expect(allChips(i)).toEqual([
      expect.objectContaining({
        sev: 'red',
        rank: 1,
        text: 'Daemon needs attention: 2',
        target: { kind: 'modal', section: 'warnings' },
      }),
    ]);
  });

  test('waitingCount is reused, and conflicts count distinct repos', () => {
    const i = input(daemon([{ ConfigRelaxPending: { repo: 'tool' } }]));
    i.sessions = fx<StatusReply>('status_all');
    i.deps = fx<DepsList>('deps_list');
    for (const s of i.sessions.sessions) s.status.workspaces.tool.conflicts = [{ path: 'x' }];
    const chips = allChips(i);
    expect(chips.find((c) => c.rank === 2)).toMatchObject({
      sev: 'amber',
      text: `${waitingCount(i.sessions, i.deps, i.daemon.data, NOW)} waiting for you`,
      target: { kind: 'waiting' },
    });
    expect(chips.find((c) => c.rank === 3)).toMatchObject({
      sev: 'purple',
      text: copy.tray.conflicts(1),
      target: { kind: 'waiting' },
    });
  });

  test('config restart fallback and reconcile panic show amber once each', () => {
    const i = input();
    i.daemon.data!.config = {
      generation: 1,
      hash: 'x',
      loaded_at: null,
      last_attempt: null,
      restart_required: ['root'],
    };
    i.daemon.data!.reconcile.last = { panicked: true };
    expect(allChips(i).filter((c) => c.sev === 'amber')).toHaveLength(2);
    expect(
      allChips(i).every((c) => c.target?.kind === 'modal' && c.target.section === 'daemon')
    ).toBe(true);
    i.daemon.data!.warnings.push({ ConfigRestartRequired: { fields: ['root'] } });
    expect(allChips(i).filter((c) => c.text === 'Restart the daemon to apply: root')).toHaveLength(
      1
    );
  });

  test('an explicit needs_push reason yields one amber chip per repo, without guessing other states', () => {
    const i = input();
    i.daemon.data!.features = ['status.upstream'];
    i.sessions = fx<StatusReply>('status_all');
    for (const s of i.sessions.sessions)
      s.status.upstream = {
        clean: false,
        repos: {
          tool: {
            behind: 0,
            auto: {
              source: 'publish',
              state: 'waiting',
              reason: 'needs_push',
              since: incident.now,
              attempts: 0,
            },
          },
        },
      };
    expect(allChips(i).filter((c) => c.text === 'Published but not pushed · tool')).toHaveLength(1);
    expect(allChips(i)[0].target).toEqual({ kind: 'modal', section: 'daemon' });
    // publish_unfinished also covers failures after a successful push; it is not proof.
    for (const s of i.sessions.sessions)
      s.status.upstream!.repos.tool.auto!.reason = 'publish_unfinished';
    expect(allChips(i)).toEqual([]);
    for (const s of i.sessions.sessions)
      s.status.upstream!.repos.tool = { behind: 0, unpublished: true };
    expect(allChips(i)).toEqual([]);
  });

  test('optional version and recent started fields work without a new command', () => {
    const i = input();
    expect(allChips(i)).toEqual([]);
    i.daemon.data!.version = version;
    expect(allChips(i)).toEqual([]);
    i.daemon.data!.version = 'older-version';
    expect(allChips(i)[0]).toMatchObject({
      sev: 'amber',
      text: `App ${version} · daemon older-version`,
      target: { kind: 'modal', section: 'daemon' },
    });
    i.daemon.data!.started = new Date(NOW - 2 * MINUTE).toISOString();
    expect(allChips(i).find((c) => c.id === 'restarted')?.text).toBe('Daemon restarted 2 min ago');
    i.daemon.data!.started = new Date(NOW - 10 * MINUTE).toISOString();
    expect(allChips(i).some((c) => c.id === 'restarted')).toBe(false);
  });

  test('unavailableUpstream produces a muted details chip', () => {
    const i = input();
    i.daemon.data!.features = ['status.upstream'];
    i.sessions = fx<StatusReply>('status_all');
    expect(allChips(i)[0]).toMatchObject({
      sev: 'muted',
      text: 'Some session details unavailable',
    });
  });

  test('running trigger uses the existing live copy helper; savings are an upper bound', () => {
    const i = input();
    i.daemon.updatedAt = NOW - 12_000;
    i.sessions = fx<StatusReply>('status_all');
    i.du = fx<DuReply>('du');
    i.du.savings!.live = {
      logical: 4096,
      private: 1024,
      savings: 3072,
      complete: false,
      as_of: incident.now,
    };
    const model = statusBarModel(i, NOW);
    expect(model.daemon).toEqual({
      state: 'up',
      label: 'Daemon running',
      detail: 'Live · updated 12 s ago',
    });
    expect(model.right).toMatchObject({
      free: null,
      sessions: 2,
      saved: { text: 'up to 3.0 KB saved (measuring)' },
    });
    expect(model.right.saved!.tooltip).toContain('upper bound');
    expect(model.right.saved!.tooltip).toContain('2026-11-13');
    i.daemon.error = err('daemon_down');
    expect(statusBarModel(i, NOW).right.saved).toBeNull();
  });

  test.each(['total', 'sessions', 'session'])('inexact %s figures hide S1 savings', (where) => {
    const i = input();
    i.du = fx<DuReply>('du');
    i.du.savings!.live.savings = 3072;
    const figure =
      where === 'total'
        ? i.du.total
        : where === 'sessions'
          ? i.du.sessions!.all
          : i.du.sessions!.list[0];
    Object.assign(figure, { exact: false });
    expect(statusBarModel(i, NOW).right.saved).toBeNull();
    Object.assign(figure, { exact: true });
    expect(statusBarModel(i, NOW).right.saved?.text).toContain('up to');
  });
});

describe('scan gating and chip order', () => {
  test('scan appears only when running a full pass or strictly longer than 60 seconds', () => {
    const i = input();
    i.du = structuredClone(incident.du);
    const scan = i.du.scan!;
    scan.started = new Date(NOW - 59_000).toISOString();
    expect(allChips(i)).toEqual([]);
    scan.full = true;
    expect(allChips(i)[0]).toMatchObject({
      sev: 'muted',
      rank: 6,
      target: { kind: 'modal', section: 'scan' },
    });
    scan.full = false;
    scan.started = new Date(NOW - 60_000).toISOString();
    expect(allChips(i)).toEqual([]);
    scan.started = new Date(NOW - 60_001).toISOString();
    expect(allChips(i)[0].text).toBe('Measuring storage · 412 done, 140 queued');
    scan.running = false;
    expect(allChips(i)).toEqual([]);
    scan.running = true;
    scan.started = null;
    expect(allChips(i)).toEqual([]);
  });

  test('five chips sort rank, severity, then input order; source is not mutated', () => {
    const chips: Chip[] = [
      { id: 'scan', sev: 'muted', rank: 6, text: 'scan' },
      { id: 'blue', sev: 'blue', rank: 4, text: 'blue' },
      { id: 'first-amber', sev: 'amber', rank: 4, text: 'amber' },
      { id: 'error', sev: 'red', rank: 1, text: 'error' },
      { id: 'second-amber', sev: 'amber', rank: 4, text: 'amber' },
    ];
    const before = structuredClone(chips);
    const model = capChips(chips);
    expect(model.chips).toHaveLength(MAX_CHIPS);
    expect(model.overflow).toHaveLength(2);
    expect(model.chips.map((c) => c.id)).toEqual(['error', 'first-amber', 'second-amber']);
    expect(model.overflow.map((c) => c.id)).toEqual(['blue', 'scan']);
    expect(chips).toEqual(before);
  });
});

describe('P-b contract sources', () => {
  const pbi = () => input(structuredClone(pbFixture.daemon) as DaemonStatus);
  const work = (fields: Record<string, unknown>) =>
    ({ ...structuredClone(pbFixture.daemon.activity[0]), ...fields }) as DaemonActivity;
  const activity = (i: StatusBarInput) => allChips(i).filter((c) => c.id.startsWith('activity:'));
  const busy = (i: StatusBarInput) => allChips(i).filter((c) => c.id.startsWith('busy:'));

  test('contract-shaped catch-up move names the known ticket, phase, elapsed and wait counts', () => {
    const i = pbi();
    i.sessions = fx<StatusReply>('status_all');
    i.sessions.sessions[0].status.manifest.id = 's-pb9492';
    i.sessions.sessions[0].status.manifest.ticket = 'gwd-9492';
    for (const s of i.sessions.sessions)
      s.status.upstream = {
        clean: false,
        repos: {
          tool: {
            behind: 1,
            auto: { state: 'pending', source: 'main', since: incident.now, attempts: 0 },
          },
        },
      };
    const chip = activity(i)[0];
    expect(chip).toMatchObject({
      text: 'Moving gwd-9492 to new main · tool · 2 min · 2 waiting · 2 pending',
      sev: 'blue',
      rank: 5,
      target: { kind: 'modal', section: 'activity' },
    });
    expect(chip.tooltip).toContain('Phase: prepare.');
    expect(chip.tooltip).toContain('waits 30 seconds before writing gets Busy');
    i.sessionsUpdatedAt = NOW - 300_001;
    expect(activity(i)[0].text).not.toContain('pending');
    expect('api' in i.daemon.data!).toBe(false);
  });

  test.each([
    [{ kind: 'move', cause: 'publish' }, 'Rebasing after publish · tool'],
    [{ kind: 'move', cause: 'sync' }, 'Sync moving s-pb9492 · tool'],
    [{ kind: 'move', cause: 'deps' }, 'Moving s-pb9492 to new dependencies · tool'],
    [{ kind: 'move', cause: 'replay' }, 'Replaying s-pb9492 · tool'],
    [{ kind: 'deps_rollout', sessions: 5, done: 2 }, 'Rolling out deps 2/5 · tool'],
    [{ kind: 'base_build', tree: null }, 'Building base · tool'],
    [
      { kind: 'session_create', repo: null, repos: ['tool', 'web', 'api'], step: 'cloning' },
      'Creating session s-pb9492 · 3 repos',
    ],
    [{ kind: 'deps_install', proposal: null }, 'Installing dependencies · tool'],
    [{ kind: 'publish' }, 'Publishing tool'],
    [{ kind: 'push' }, 'Pushing tool'],
  ])('activity kind copy %j', (fields, text) => {
    const i = pbi();
    i.daemon.data!.activity = [work(fields)];
    expect(activity(i)[0].text).toStartWith(`${text} · 2 min`);
    expect(activity(i)[0].sev).toBe('blue');
  });

  test('unminted session creation needs neither a repo nor a session', () => {
    const i = pbi();
    i.daemon.data!.activity = [
      work({ kind: 'session_create', repo: null, session: null, repos: ['tool'], step: null }),
    ];
    expect(activity(i)[0].text).toStartWith('Creating session · 1 repo · 2 min');
  });

  test('registry debounce, monotonic elapsed, and the 30 minute boundary', () => {
    const i = pbi();
    i.daemon.data!.activity![0].started = new Date(NOW - DAY).toISOString();
    i.daemon.data!.activity![0].elapsed_ms = 2999;
    expect(activity(i)).toEqual([]);
    i.daemon.data!.activity![0].elapsed_ms = 3000;
    expect(activity(i)[0]).toMatchObject({ sev: 'blue', rank: 5 });
    i.daemon.data!.activity![0].elapsed_ms = BACKGROUND_HOLD_MAX_MS;
    expect(activity(i)[0]).toMatchObject({ sev: 'amber', rank: 4 });
    expect(activity(i)[0].text).toContain('30 min');
  });

  test('oldest parent work covers child activities and transient deferrals in the same repo', () => {
    const i = pbi();
    i.daemon.data!.activity = [
      work({ kind: 'move', id: 2 }),
      work({ kind: 'deps_rollout', id: 1, elapsed_ms: 180_000, sessions: 5, done: 2 }),
      work({ kind: 'base_build', id: 3, elapsed_ms: 60_000, tree: null }),
    ];
    i.daemon.data!.warnings = [stuck('recent_writes')];
    expect(activity(i)).toHaveLength(1);
    expect(activity(i)[0].text).toStartWith('Rolling out deps 2/5 · tool · 3 min');
    i.daemon.data!.activity = [
      work({
        kind: 'session_create',
        repo: null,
        repos: ['tool', 'web'],
        id: 1,
        elapsed_ms: 180_000,
        step: null,
      }),
      work({ id: 2 }),
    ];
    expect(activity(i)).toHaveLength(1);
  });

  test('plain background holds need 10 s, no matching activity, and explicit background origin', () => {
    const i = pbi();
    i.daemon.data!.activity = [];
    expect(activity(i)[0].text).toBe('tool busy with daemon work · 2 waiting');
    const h = i.daemon.data!.holds[0] as Record<string, unknown>;
    h.elapsed_ms = 10_000;
    expect(activity(i)).toEqual([]);
    h.elapsed_ms = 10_001;
    expect(activity(i)).toHaveLength(1);
    h.origin = 'request';
    expect(activity(i)).toEqual([]);
    h.origin = 'background';
    i.daemon.data!.warnings = [hold('daemon', 31 * MINUTE, { origin: 'background' })];
    expect(activity(i)).toEqual([]);
  });

  test.each(['catch_up', 'publish', 'push'])(
    'P-b %s LongHold is abnormal amber, with origin/session detail',
    (method) => {
      const i = pbi();
      i.daemon.data!.warnings = [
        hold(method, MINUTE, {
          origin: method === 'catch_up' ? 'background' : 'request',
          session: 's-pb9492',
        }),
      ];
      const chip = allChips(i).find((c) => c.id.startsWith('warning:LongHold'))!;
      expect(chip).toMatchObject({ sev: 'amber', rank: 4 });
      expect(chip.text).toContain('(s-pb9492)');
      expect(chip.text).toContain(method === 'catch_up' ? 'background' : 'request');
    }
  );

  test('Busy is grouped by scope after the hold/activity is gone; total is not the headline', () => {
    const i = pbi();
    i.daemon.data!.holds = [];
    i.daemon.data!.activity = [];
    expect(busy(i)).toHaveLength(1);
    expect(busy(i)[0]).toMatchObject({
      sev: 'amber',
      text: '3 requests got Busy · tool (auto-rebase of s-pb9492)',
    });
    i.daemon.data!.busy!.recent.push({
      ...i.daemon.data!.busy!.recent[0],
      scope: { repo: 'web' },
      holder: null,
    });
    expect(busy(i).map((c) => c.text)).toEqual([
      '3 requests got Busy · tool (auto-rebase of s-pb9492)',
      '1 request got Busy · web',
    ]);
    i.daemon.data!.busy!.recent[3].scope = {};
    expect(busy(i)[1].text).toBe('Some requests got Busy');
    i.daemon.data!.busy!.truncated = true;
    expect(busy(i)).toHaveLength(1);
    expect(busy(i)[0].text).toBe('50+ requests got Busy');
    i.daemon.data!.busy!.recent.forEach((e) => (e.at = new Date(NOW - 600_001).toISOString()));
    expect(busy(i)).toEqual([]);
  });

  test('free-space thresholds use decimal GB caps and keep the previous severity until 110%', () => {
    const i = pbi();
    const v = i.daemon.data!.volume!;
    expect(statusBarModel(i, NOW).right.free?.text).toBe('1.2 TB free');
    v.size = 1e12;
    const model = (free: number, previous: 'muted' | 'amber' | 'red' = 'muted') => {
      v.free = free;
      return statusBarModel(i, NOW, previous);
    };
    expect(model(100e9).spaceLevel).toBe('muted');
    expect(model(99e9).spaceLevel).toBe('amber');
    expect(model(20e9).spaceLevel).toBe('amber');
    const red = model(19e9);
    expect(red.right.free?.sev).toBe('red');
    expect(red.chips.find((c) => c.id === 'low-space')).toMatchObject({
      rank: 1,
      sev: 'red',
      target: { kind: 'modal', section: 'trash' },
    });
    expect(model(22e9, 'red').spaceLevel).toBe('red');
    expect(model(22e9 + 1, 'red').spaceLevel).toBe('amber');
    expect(model(110e9, 'amber').spaceLevel).toBe('amber');
    expect(model(110e9 + 1, 'amber').spaceLevel).toBe('muted');
    v.size = 100e9;
    expect(model(14e9).spaceLevel).toBe('amber');
    expect(model(4e9).spaceLevel).toBe('red');
  });

  test('Busy copy does not attribute several holders in a scope to just the most recent one', () => {
    const i = pbi();
    i.daemon.data!.busy!.recent[0].holder = 'publish';
    expect(busy(i)[0].text).toBe('3 requests got Busy · tool');
  });

  test('stale volume is a dash without a low-space chip, preserving hysteresis until a good sample', () => {
    const i = pbi();
    i.daemon.data!.volume!.free = 1;
    i.daemon.data!.volume!.as_of = new Date(NOW - 60_000).toISOString();
    expect(statusBarModel(i, NOW).right.free?.sev).toBe('red');
    i.daemon.data!.volume!.as_of = new Date(NOW - 60_001).toISOString();
    expect(statusBarModel(i, NOW, 'red')).toMatchObject({
      spaceLevel: 'red',
      right: { free: { text: '—', sev: 'muted' } },
    });
    expect(allChips(i).some((c) => c.id === 'low-space')).toBe(false);
    i.daemon.data!.volume = null;
    expect(statusBarModel(i, NOW, 'red')).toMatchObject({
      spaceLevel: 'red',
      right: { free: null },
    });
  });

  test('atomic scan counts work without du; unresolved failures do not come from failed history', () => {
    const i = pbi();
    expect(allChips(i).find((c) => c.id === 'scan')?.text).toBe(
      'Measuring storage · 412 done · 4 in progress · 136 queued'
    );
    expect(allChips(i).find((c) => c.id === 'scan-unresolved')?.text).toBe(
      "Couldn't measure 1 item"
    );
    i.daemon.data!.scan!.unresolved_n = 0;
    i.daemon.data!.scan!.full = false;
    // There is no start time in daemon_status.scan: show running progress until du supplies it.
    expect(allChips(i).some((c) => c.id === 'scan')).toBe(true);
    i.du = fx<DuReply>('du');
    i.du.scan = { ...pbFixture.duScan, started: new Date(NOW - 60_000).toISOString() };
    expect(allChips(i).some((c) => c.id === 'scan')).toBe(false);
    expect(allChips(i).some((c) => c.id === 'scan-unresolved')).toBe(false);
    i.du.scan.started = new Date(NOW - 60_001).toISOString();
    expect(allChips(i).some((c) => c.id === 'scan')).toBe(true);
    i.daemon.data!.scan!.running = false;
    expect(allChips(i).some((c) => c.id === 'scan')).toBe(false);
    delete i.daemon.data!.scan;
    expect(allChips(i).find((c) => c.id === 'scan-unresolved')?.text).toBe(
      "Couldn't measure 1 item"
    );
  });

  test('version mismatch and restart use existing identity source and RFC 3339 started', () => {
    const i = pbi();
    i.daemon.data!.version = '0.2.3';
    expect(allChips(i).find((c) => c.id === 'version')).toMatchObject({
      sev: 'amber',
      text: `App ${version} · daemon 0.2.3`,
    });
    expect(allChips(i).find((c) => c.id === 'restarted')).toMatchObject({
      sev: 'muted',
      text: 'Daemon restarted 2 min ago',
    });
  });

  test('without the capability new data fields add no chips or free-space facts', () => {
    const i = pbi();
    i.daemon.data!.features = ['status.upstream'];
    expect(activity(i)).toEqual([]);
    expect(busy(i)).toEqual([]);
    expect(statusBarModel(i, NOW).right.free).toBeNull();
    expect(allChips(i).filter((c) => c.id.startsWith('scan'))).toEqual([]);
    expect('api' in i.daemon.data!).toBe(false);
  });
});
