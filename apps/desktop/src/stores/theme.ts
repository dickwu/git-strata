import { create } from 'zustand';
import type { ThemeName } from '@/lib/tokens';

/** localStorage key for the saved choice. The one source of truth for the theme (t93u). */
export const THEME_KEY = 'git-strata.theme';

export type ThemeChoice = ThemeName | 'system';

/** Status bar and ⌘K order. */
export const THEME_CHOICES: readonly ThemeChoice[] = ['system', 'light', 'dark'];

interface ThemeState {
  choice: ThemeChoice;
  system: ThemeName;
  theme: ThemeName;
  setChoice: (c: ThemeChoice) => void;
  toggle: () => void;
  setSystem: (t: ThemeName) => void;
}

export const resolveTheme = (choice: ThemeChoice, system: ThemeName): ThemeName =>
  choice === 'system' ? system : choice;

/** A saved value, or 'system' for anything missing or unknown. */
export function parseChoice(v: unknown): ThemeChoice {
  return v === 'light' || v === 'dark' || v === 'system' ? v : 'system';
}

export const useTheme = create<ThemeState>((set, get) => ({
  choice: 'system',
  system: 'light',
  theme: 'light',
  setChoice: (choice) => {
    try {
      localStorage.setItem(THEME_KEY, choice);
    } catch {
      /* storage unavailable: keep the choice for this run only */
    }
    set({ choice, theme: resolveTheme(choice, get().system) });
  },
  toggle: () => get().setChoice(get().theme === 'dark' ? 'light' : 'dark'),
  setSystem: (system) => set({ system, theme: resolveTheme(get().choice, system) }),
}));

/** Read the saved choice and follow the OS appearance live. Returns the unsubscribe. */
export function initTheme(): () => void {
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const sys = (): ThemeName => (mq.matches ? 'dark' : 'light');
  let saved: ThemeChoice = 'system';
  try {
    saved = parseChoice(localStorage.getItem(THEME_KEY));
  } catch {
    /* ignore */
  }
  useTheme.setState({ choice: saved, system: sys(), theme: resolveTheme(saved, sys()) });
  const on = () => useTheme.getState().setSystem(sys());
  mq.addEventListener('change', on);
  return () => mq.removeEventListener('change', on);
}
