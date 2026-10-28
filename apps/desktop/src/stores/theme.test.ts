import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { THEME_KEY, initTheme, parseChoice, resolveTheme, useTheme } from './theme';

type Listener = () => void;

/** A stand-in for the OS appearance query; flip() fires 'change' like WebKit does. */
function fakeMedia(dark: boolean) {
  const listeners = new Set<Listener>();
  const mq = {
    matches: dark,
    addEventListener: (_: string, f: Listener) => listeners.add(f),
    removeEventListener: (_: string, f: Listener) => listeners.delete(f),
  };
  return {
    mq,
    listeners,
    flip(next: boolean) {
      mq.matches = next;
      listeners.forEach((f) => f());
    },
  };
}

function fakeStorage(init: Record<string, string> = {}) {
  const m = new Map(Object.entries(init));
  return {
    map: m,
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  };
}

const g = globalThis as unknown as { window?: unknown; localStorage?: unknown };
let saved: { window?: unknown; localStorage?: unknown };

function install(dark: boolean, store: Record<string, string> = {}) {
  const media = fakeMedia(dark);
  const storage = fakeStorage(store);
  g.window = { matchMedia: () => media.mq };
  g.localStorage = storage;
  return { media, storage };
}

beforeEach(() => {
  saved = { window: g.window, localStorage: g.localStorage };
  useTheme.setState({ choice: 'system', system: 'light', theme: 'light' });
});
afterEach(() => {
  g.window = saved.window;
  g.localStorage = saved.localStorage;
});

describe('theme choice', () => {
  test('resolve: system follows the OS, light and dark are fixed', () => {
    expect(resolveTheme('system', 'dark')).toBe('dark');
    expect(resolveTheme('system', 'light')).toBe('light');
    expect(resolveTheme('light', 'dark')).toBe('light');
    expect(resolveTheme('dark', 'light')).toBe('dark');
  });

  test('parseChoice falls back to system for missing or unknown values', () => {
    expect(parseChoice('dark')).toBe('dark');
    expect(parseChoice(null)).toBe('system');
    expect(parseChoice('sepia')).toBe('system');
  });

  test('setChoice persists and applies at once', () => {
    const { storage } = install(false);
    initTheme();
    useTheme.getState().setChoice('dark');
    expect(useTheme.getState().theme).toBe('dark');
    expect(storage.map.get(THEME_KEY)).toBe('dark');
    useTheme.getState().setChoice('system');
    expect(useTheme.getState().theme).toBe('light');
    expect(storage.map.get(THEME_KEY)).toBe('system');
  });

  test('initTheme restores the saved choice', () => {
    install(false, { [THEME_KEY]: 'dark' });
    initTheme();
    expect(useTheme.getState().choice).toBe('dark');
    expect(useTheme.getState().theme).toBe('dark');
  });

  test('initTheme ignores a corrupt saved value', () => {
    install(true, { [THEME_KEY]: '42' });
    initTheme();
    expect(useTheme.getState().choice).toBe('system');
    expect(useTheme.getState().theme).toBe('dark');
  });

  test('system follows the OS live; a fixed choice does not', () => {
    const { media } = install(false);
    const stop = initTheme();
    expect(useTheme.getState().theme).toBe('light');
    media.flip(true);
    expect(useTheme.getState().theme).toBe('dark');
    useTheme.getState().setChoice('light');
    media.flip(false);
    media.flip(true);
    expect(useTheme.getState().theme).toBe('light');
    // Back to system picks up the OS state recorded while fixed.
    useTheme.getState().setChoice('system');
    expect(useTheme.getState().theme).toBe('dark');
    stop();
    expect(media.listeners.size).toBe(0);
  });

  test('storage failures keep the choice for this run', () => {
    install(false);
    g.localStorage = {
      getItem: () => {
        throw new Error('denied');
      },
      setItem: () => {
        throw new Error('denied');
      },
    };
    initTheme();
    useTheme.getState().setChoice('dark');
    expect(useTheme.getState().theme).toBe('dark');
  });
});
