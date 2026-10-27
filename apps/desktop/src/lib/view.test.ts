import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { DaemonStatus, DepsList, StatusReply } from './types';
import { daemonView, openTarget, sessionDetail, sessionRows, short } from './view';
import { bytes, ago } from './copy';

const fx = <T>(name: string): T =>
  JSON.parse(
    readFileSync(join(import.meta.dir, '../../src-tauri/tests/fixtures/p1', `${name}.json`), 'utf8')
  ) as T;

const all = fx<StatusReply>('status_all');
const one = fx<StatusReply>('status_session');
const deps = fx<DepsList>('deps_list');
const daemon = fx<DaemonStatus>('daemon_status');

describe('sessionRows', () => {
  test('one row per session, newest first', () => {
    const rows = sessionRows(all);
    expect(rows.map((r) => r.id)).toEqual(['S2', 'S1']);
    expect(rows[1]).toMatchObject({
      ticket: 'T-101',
      state: 'Active',
      repos: 2,
      pending: 0,
      conflicts: 0,
      health: 'Ok',
    });
  });
  test('no reply → no rows', () => {
    expect(sessionRows(undefined)).toEqual([]);
  });
  test('worst health and conflict counts surface', () => {
    const copy: StatusReply = structuredClone(all);
    const s1 = copy.sessions.find((s) => s.status.manifest.id === 'S1')!;
    s1.status.workspaces.web.health = 'Degraded';
    s1.status.workspaces.web.conflicts = [{ path: 'a' }, { path: 'b' }];
    const r = sessionRows(copy).find((x) => x.id === 'S1')!;
    expect(r.health).toBe('Degraded');
    expect(r.conflicts).toBe(2);
  });
});

describe('sessionDetail', () => {
  test('repos sorted, short hashes available, checkpoints newest first', () => {
    const d = sessionDetail(one, one.sessions[0].status.manifest.id)!;
    expect(d).not.toBeNull();
    expect(d.repos.map((r) => r.repo)).toEqual(['tool', 'web']);
    expect(d.repos[0].chain).toMatchObject({ branch: 'main', state: 'Absent' });
    expect(short(d.repos[0].head)).toHaveLength(7);
    const ids = d.checkpoints.map((c) => c.id);
    expect(ids).toEqual([...ids].sort((a, b) => b - a));
    for (const c of d.checkpoints) expect(c.at.getUTCFullYear()).toBe(2026);
  });
  test('S1 from status_all has two checkpoints', () => {
    const d = sessionDetail(all, 'S1')!;
    expect(d.checkpoints.map((c) => c.id)).toEqual([2, 1]);
    expect(d.repos.find((r) => r.repo === 'web')!.checkpoint).toBe(2);
  });
  test('unknown id → null', () => {
    expect(sessionDetail(all, 'S99')).toBeNull();
  });
});

describe('daemonView', () => {
  test('fixture daemon has no api key → legacy, nothing waiting', () => {
    const v = daemonView(daemon);
    expect(v.legacy).toBe(true);
    expect(v.waiting).toEqual([]);
    expect(v.reconcileRuns).toBe(0);
    expect(v.holds).toBe(0);
  });
  test('api key present → not legacy; waiting sorted by count', () => {
    const v = daemonView({
      ...daemon,
      api: 1,
      repo_waiting: { a: 1, b: 3, c: 0 },
      warnings: [{ kind: 'Drift', message: 'x' }, 'plain'],
    });
    expect(v.legacy).toBe(false);
    expect(v.waiting).toEqual([
      { repo: 'b', n: 3 },
      { repo: 'a', n: 1 },
    ]);
    expect(v.warnings).toEqual(['Drift: x', 'plain']);
  });
});

describe('openTarget (tray strata://open)', () => {
  test('deps proposal → the session that made it', () => {
    expect(openTarget('waiting:deps:1', all, deps)).toEqual({ session: 'S2' });
  });
  test('unknown proposal → list', () => {
    expect(openTarget('waiting:deps:9', all, deps)).toEqual({ session: null });
  });
  test('conflict → first session with conflicts in that repo', () => {
    const c: StatusReply = structuredClone(all);
    c.sessions.find((s) => s.status.manifest.id === 'S2')!.status.workspaces.tool.conflicts = [{}];
    expect(openTarget('waiting:conflict:tool', c, deps)).toEqual({ session: 'S2' });
    expect(openTarget('waiting:conflict:web', c, deps)).toEqual({ session: null });
  });
});

describe('formatting', () => {
  test('bytes', () => {
    expect(bytes(0)).toBe('0 B');
    expect(bytes(512)).toBe('512 B');
    expect(bytes(937984)).toBe('916 KB');
    expect(bytes(1536)).toBe('1.5 KB');
  });
  test('ago', () => {
    expect(ago(3000)).toBe('just now');
    expect(ago(42000)).toBe('42 s ago');
    expect(ago(125000)).toBe('2 min ago');
  });
});
