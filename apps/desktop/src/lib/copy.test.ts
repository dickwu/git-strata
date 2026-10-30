import { describe, expect, test } from 'bun:test';
import { compactTime, copy, duration, fullTime } from './copy';
import { sessionAge } from './view';

describe('copy', () => {
  // Display name only (tauri.conf.json productName, src-tauri APP_NAME); the CLI,
  // binaries and bundle identifier stay git-strata.
  test('app display name is GIT STRATA', () => {
    expect(copy.app).toBe('GIT STRATA');
    expect(copy.errors.notInTauri).toContain('GIT STRATA');
  });
});

describe('Time column', () => {
  const M = 60_000;
  const H = 60 * M;
  const D = 24 * H;
  test('duration: two units at most, compact', () => {
    expect(duration(0)).toBe('<1m');
    expect(duration(59_999)).toBe('<1m');
    expect(duration(M)).toBe('1m');
    expect(duration(59 * M)).toBe('59m');
    expect(duration(H)).toBe('1h');
    expect(duration(2 * H + 14 * M)).toBe('2h 14m');
    expect(duration(23 * H + 59 * M)).toBe('23h 59m');
    expect(duration(D)).toBe('1d');
    expect(duration(3 * D + 4 * H + 59 * M)).toBe('3d 4h');
    expect(duration(-5)).toBe('<1m');
  });
  test('compactTime: shorter the closer it is (local zone)', () => {
    const now = new Date(2026, 9, 27, 16, 0);
    expect(compactTime(new Date(2026, 9, 27, 14, 5), now)).toBe('2:05 PM');
    expect(compactTime(new Date(2026, 9, 26, 9, 30), now)).toBe('Oct 26, 9:30 AM');
    expect(compactTime(new Date(2025, 0, 3, 9, 30), now)).toBe('Jan 3, 2025');
  });
  test('fullTime carries seconds and a zone', () => {
    expect(fullTime(new Date(2026, 9, 27, 14, 5, 9))).toMatch(/^Oct 27, 2026, 2:05:09\sPM \S+/);
  });
  test('sessionAge runs from created; Removed has no final duration', () => {
    const created = '2026-10-27T17:31:14.575882Z';
    const t = Date.parse(created);
    expect(sessionAge(created, 'Active', t + 2 * H + 14 * M)).toBe(2 * H + 14 * M);
    expect(sessionAge(created, 'Active', t - 1000)).toBe(0);
    expect(sessionAge(created, 'Removed', t + H)).toBeNull();
    expect(sessionAge('nope', 'Active', t)).toBeNull();
  });
});
