import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { buildEntries, filterEntries, type Entry } from './palette';
import type { StatusReply } from './types';

const e = (key: string, label: string, keywords: string[] = [], hint?: string): Entry => ({
  key,
  label,
  hint,
  keywords,
  action: { kind: 'refresh' },
});

const entries = [
  e('a', 'Go to Sessions', ['list']),
  e('b', 'Go to Daemon', ['status', 'socket']),
  e('c', 'T-101', ['Active'], 'S1'),
  e('d', 'Switch light / dark theme', ['dark']),
  e('e', 'Session tools', []),
];

describe('filterEntries', () => {
  test('empty query keeps everything in order', () => {
    expect(filterEntries(entries, '  ').map((x) => x.key)).toEqual(['a', 'b', 'c', 'd', 'e']);
  });
  test('case-insensitive, matches keywords and hints', () => {
    expect(filterEntries(entries, 'SOCKET').map((x) => x.key)).toEqual(['b']);
    expect(filterEntries(entries, 's1').map((x) => x.key)).toEqual(['c']);
  });
  test('label prefix ranks first, then word prefix', () => {
    expect(filterEntries(entries, 'sess').map((x) => x.key)).toEqual(['e', 'a']);
  });
  test('all words must match', () => {
    expect(filterEntries(entries, 'go daemon').map((x) => x.key)).toEqual(['b']);
    expect(filterEntries(entries, 'go nothing')).toEqual([]);
  });
});

describe('buildEntries (⌘K)', () => {
  const all = JSON.parse(
    readFileSync(join(import.meta.dir, '../../src-tauri/tests/fixtures/p1/status_all.json'), 'utf8')
  ) as StatusReply;
  const list = buildEntries(all);
  test('sessions are searchable by repo name', () => {
    const hits = filterEntries(list, 'web').map((x) => x.key);
    expect(hits).toContain('session:S1');
    expect(hits).toContain('session:S2');
    expect(hits).toContain('repo:web');
  });
  test('repo filter entries only for used repos', () => {
    const r = structuredClone(all);
    r.sessions = r.sessions.filter((s) => s.status.manifest.id === 'S2');
    expect(
      buildEntries(r)
        .filter((x) => x.action.kind === 'repo')
        .map((x) => x.key)
    ).toEqual(['repo:tool']);
  });
  test('one "Sessions with <repo>" entry per distinct repo, filtering the list', () => {
    const repos = list.filter((x) => x.action.kind === 'repo');
    expect(repos.map((x) => x.label)).toEqual(['Sessions with tool', 'Sessions with web']);
    expect(repos[1].action).toEqual({ kind: 'repo', repo: 'web' });
  });
  test('"Toggle sidebar" is a command, found by collapse too', () => {
    expect(filterEntries(list, 'collapse').map((x) => x.key)).toEqual(['sidebar']);
    expect(list.find((x) => x.key === 'sidebar')!.action).toEqual({ kind: 'sidebar' });
  });
  test('no daemon reply → only screens and commands', () => {
    expect(buildEntries(undefined).every((x) => !x.key.startsWith('session:'))).toBe(true);
  });
});
