import { beforeEach, describe, expect, test } from 'bun:test';
import { sessionFromUrl, sessionHash, useNav } from './nav';

beforeEach(() => useNav.getState().clearFilters());

describe('Sessions list filter state', () => {
  test('add dedupes, remove drops one, clear empties chips and text', () => {
    const s = () => useNav.getState();
    s().addRepoFilter('web');
    s().addRepoFilter('tool');
    s().addRepoFilter('web');
    expect(s().repoFilter).toEqual(['web', 'tool']);
    s().removeRepoFilter('web');
    expect(s().repoFilter).toEqual(['tool']);
    s().setTextFilter('t-1');
    s().clearFilters();
    expect(s().repoFilter).toEqual([]);
    expect(s().textFilter).toBe('');
  });
  test('toggle selects then deselects (bar tags and row tags share it)', () => {
    const s = () => useNav.getState();
    s().toggleRepoFilter('web');
    expect(s().repoFilter).toEqual(['web']);
    s().toggleRepoFilter('web');
    expect(s().repoFilter).toEqual([]);
    s().toggleAgentFilter('codex');
    s().clearFilters();
    expect(s().agentFilter).toEqual([]);
  });
  test('survives opening a session and going back to the list', () => {
    const s = () => useNav.getState();
    s().addRepoFilter('web');
    s().openSession('S1');
    s().openSession(null);
    s().go('sessions');
    expect(s().repoFilter).toEqual(['web']);
  });
});

describe('the session list is the top-level view (t76u: no Sessions nav item)', () => {
  test('the app opens on the list, not a session or the Daemon screen', () => {
    const init = useNav.getInitialState();
    expect(init.screen).toBe('sessions');
    expect(init.session).toBeNull();
  });
  test('back from Daemon and back from a session both land on the list', () => {
    const s = () => useNav.getState();
    s().go('daemon');
    s().go('sessions');
    expect([s().screen, s().session]).toEqual(['sessions', null]);
    s().openSession('s-1');
    s().openSession(null);
    expect([s().screen, s().session]).toEqual(['sessions', null]);
  });
});

describe('session detail is a modal over the list (t87u)', () => {
  test('opening a session keeps the list screen and its filter underneath', () => {
    const s = () => useNav.getState();
    s().addRepoFilter('web');
    s().setTextFilter('T-1');
    s().openSession('S1');
    expect([s().screen, s().session]).toEqual(['sessions', 'S1']);
    s().openSession(null);
    expect([s().screen, s().session, s().repoFilter, s().textFilter]).toEqual([
      'sessions',
      null,
      ['web'],
      'T-1',
    ]);
  });
  test('opening a session from the Daemon screen (palette, tray) lands on the list', () => {
    const s = () => useNav.getState();
    s().go('daemon');
    s().openSession('S2');
    expect([s().screen, s().session]).toEqual(['sessions', 'S2']);
    s().openSession(null);
  });
});

describe('deep links to a session', () => {
  const u = (pathname = '/', hash = '', search = '') => ({ pathname, hash, search });
  test('hash, path and query forms all resolve', () => {
    expect(sessionFromUrl(u('/', '#/session/s-abc'))).toBe('s-abc');
    expect(sessionFromUrl(u('/session/s-abc'))).toBe('s-abc');
    expect(sessionFromUrl(u('/session/s-abc/'))).toBe('s-abc');
    expect(sessionFromUrl(u('/', '', '?session=s-abc'))).toBe('s-abc');
  });
  test('anything else is no session', () => {
    expect(sessionFromUrl(u())).toBeNull();
    expect(sessionFromUrl(u('/', '#/daemon'))).toBeNull();
    expect(sessionFromUrl(u('/session/'))).toBeNull();
    expect(sessionFromUrl(u('/', '#/session/a/b'))).toBeNull();
    expect(sessionFromUrl(u('/', '#/session/%E0%A4%A'))).toBeNull();
  });
  test('the hash the app writes round-trips', () => {
    for (const id of ['s-6o2xyse', 'odd id/with#chars']) {
      expect(sessionFromUrl(u('/', sessionHash(id)))).toBe(id);
    }
  });
});
