import { beforeEach, describe, expect, test } from 'bun:test';
import { useNav } from './nav';

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
