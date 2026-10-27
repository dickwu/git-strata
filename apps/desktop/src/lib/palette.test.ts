import { describe, expect, test } from 'bun:test';
import { filterEntries, type Entry } from './palette';

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
