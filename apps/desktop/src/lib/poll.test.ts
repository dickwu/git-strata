import { describe, expect, test } from 'bun:test';
import { pollPlan } from './poll';

describe('pollPlan', () => {
  test('api daemon, visible: gui.md §5.3.3 fallback row', () => {
    expect(pollPlan({ visible: true, up: true, legacy: false })).toEqual({
      daemonStatus: 2000,
      sessions: 5000,
      session: 10000,
      deps: 10000,
      du: 60000,
    });
  });
  test('api daemon, hidden', () => {
    expect(pollPlan({ visible: false, up: true, legacy: false })).toEqual({
      daemonStatus: 10000,
      sessions: 30000,
      session: false,
      deps: 30000,
      du: false,
    });
  });
  test('legacy daemon: status{all} no more than every 30 s visible, 120 s hidden', () => {
    const v = pollPlan({ visible: true, up: true, legacy: true });
    const h = pollPlan({ visible: false, up: true, legacy: true });
    expect(v.sessions).toBe(30000);
    expect(v.session).toBe(30000);
    expect(v.daemonStatus).toBe(5000);
    expect(h.sessions).toBe(120000);
    expect(h.du).toBe(false);
  });
  test('daemon down: only daemon_status polls', () => {
    for (const visible of [true, false]) {
      const p = pollPlan({ visible, up: false, legacy: true });
      expect(p.daemonStatus).toBe(visible ? 5000 : 30000);
      expect([p.sessions, p.session, p.deps, p.du]).toEqual([false, false, false, false]);
    }
  });
});
