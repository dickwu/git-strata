import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { version } from '../../package.json';
import incident from './fixtures/statusBar/incident-0927.json';
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
import type { DaemonStatus, DepsList, DuReply, StatusReply } from './types';
import { waitingCount } from './view';

const NOW = Date.parse(incident.now);
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
    expect(blue[0].target).toEqual({ kind: 'modal', section: 'activity' });
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

  test('one activity per repo, with waiting and only fresh upstream pending counts', () => {
    const i = input(daemon([hold(), stuck('recent_writes')]));
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
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).toContain('· 2 waiting · 2 pending');
    i.sessionsUpdatedAt = NOW - 301_000;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).not.toContain('pending');
    i.sessionsUpdatedAt = undefined;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).not.toContain('pending');
    i.sessionsUpdatedAt = NOW - 91_000;
    i.daemon.data!.api = 1;
    expect(allChips(i).find((c) => c.sev === 'blue')!.text).not.toContain('pending');
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
  test('connecting, not running and not responding are distinct', () => {
    const i = input();
    i.daemon = { pending: true, data: undefined, error: null, updatedAt: 0 };
    expect(statusBarModel(i, NOW).daemon).toEqual({
      state: 'pending',
      label: 'Connecting to daemon',
      detail: null,
    });
    expect(allChips(i)[0]).toMatchObject({
      sev: 'muted',
      target: { kind: 'modal', section: 'daemon' },
    });
    i.daemon.pending = false;
    i.daemon.error = err('daemon_down');
    expect(statusBarModel(i, NOW).daemon.label).toBe("Daemon isn't running");
    expect(allChips(i)[0]).toMatchObject({ sev: 'red', text: "Daemon isn't running" });
    i.daemon.error = err('daemon_timeout', { seconds: 8 });
    expect(statusBarModel(i, NOW).daemon.label).toBe("Daemon isn't responding");
    expect(allChips(i)[0].text).toBe("Daemon isn't responding");
    expect(statusBarModel(i, NOW).daemon.detail).toBe(copy.daemon.stuck(8));
    i.daemon.error = new Error('other transport error');
    expect(statusBarModel(i, NOW).daemon.detail).toBe('other transport error');
  });

  test.each([1, 4, 13])('store permission errno %s uses Removable Volumes guidance', (errno) => {
    const i = input();
    i.daemon.error = err('store_unavailable', { last_error: { detail: { errno } } });
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
    expect(allChips(i)[0]).toMatchObject({ sev: 'muted', text: 'Daemon starting (recovering)' });
  });

  test('stale health is muted and does not present old warnings/activity as current', () => {
    const i = input(daemon(['StorePoisoned', hold()]));
    i.daemon.updatedAt = NOW - 120_000;
    expect(allChips(i)).toEqual([
      expect.objectContaining({ sev: 'muted', text: 'Not updated for 2 min' }),
    ]);
    expect(statusBarModel(i, NOW).daemon.state).toBe('pending');
    i.daemon.updatedAt = NOW - 60_000;
    expect(allChips(i).some((c) => c.sev === 'red')).toBe(true);
  });

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
    expect(allChips(i)).toEqual([
      expect.objectContaining({ sev: 'red', text: 'Daemon is out of file handles' }),
    ]);
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
