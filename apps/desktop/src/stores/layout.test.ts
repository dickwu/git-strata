import { beforeEach, describe, expect, test } from 'bun:test';
import { SIDEBAR_KEY, initLayout, isSidebarShortcut, useLayout } from './layout';

const mem = new Map<string, string>();
globalThis.localStorage = {
  getItem: (k: string) => mem.get(k) ?? null,
  setItem: (k: string, v: string) => void mem.set(k, v),
  removeItem: (k: string) => void mem.delete(k),
  clear: () => mem.clear(),
  key: () => null,
  length: 0,
} as Storage;

beforeEach(() => {
  mem.clear();
  useLayout.setState({ sidebarCollapsed: false });
});

describe('sidebar collapse', () => {
  test('toggle flips and persists', () => {
    useLayout.getState().toggleSidebar();
    expect(useLayout.getState().sidebarCollapsed).toBe(true);
    expect(mem.get(SIDEBAR_KEY)).toBe('collapsed');
    useLayout.getState().toggleSidebar();
    expect(mem.get(SIDEBAR_KEY)).toBe('expanded');
  });
  test('initLayout restores the saved state; junk is ignored', () => {
    mem.set(SIDEBAR_KEY, 'collapsed');
    initLayout();
    expect(useLayout.getState().sidebarCollapsed).toBe(true);
    mem.set(SIDEBAR_KEY, 'sideways');
    initLayout();
    expect(useLayout.getState().sidebarCollapsed).toBe(true);
  });
  test('⌘\\ and Ctrl+\\ are the shortcut; shifted or other keys are not', () => {
    const k = (key: string, m: Partial<KeyboardEvent> = {}) => ({
      key,
      metaKey: false,
      ctrlKey: false,
      shiftKey: false,
      altKey: false,
      ...m,
    });
    expect(isSidebarShortcut(k('\\', { metaKey: true }))).toBe(true);
    expect(isSidebarShortcut(k('\\', { ctrlKey: true }))).toBe(true);
    expect(isSidebarShortcut(k('\\'))).toBe(false);
    expect(isSidebarShortcut(k('\\', { metaKey: true, shiftKey: true }))).toBe(false);
    expect(isSidebarShortcut(k('b', { metaKey: true }))).toBe(false);
  });
});
