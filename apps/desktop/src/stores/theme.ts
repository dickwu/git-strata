import { create } from 'zustand';
import type { ThemeName } from '@/lib/tokens';

const KEY = 'git-strata.theme';

type Choice = ThemeName | 'system';

interface ThemeState {
  choice: Choice;
  system: ThemeName;
  theme: ThemeName;
  setChoice: (c: Choice) => void;
  toggle: () => void;
  setSystem: (t: ThemeName) => void;
}

const resolve = (choice: Choice, system: ThemeName): ThemeName =>
  choice === 'system' ? system : choice;

export const useTheme = create<ThemeState>((set, get) => ({
  choice: 'system',
  system: 'light',
  theme: 'light',
  setChoice: (choice) => {
    try {
      localStorage.setItem(KEY, choice);
    } catch {
      /* storage unavailable: keep the choice for this run only */
    }
    set({ choice, theme: resolve(choice, get().system) });
  },
  toggle: () => get().setChoice(get().theme === 'dark' ? 'light' : 'dark'),
  setSystem: (system) => set({ system, theme: resolve(get().choice, system) }),
}));

/** Read the saved choice and follow the OS appearance. Returns the unsubscribe. */
export function initTheme(): () => void {
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const sys = (): ThemeName => (mq.matches ? 'dark' : 'light');
  let saved: Choice = 'system';
  try {
    const v = localStorage.getItem(KEY);
    if (v === 'light' || v === 'dark' || v === 'system') saved = v;
  } catch {
    /* ignore */
  }
  useTheme.setState({ choice: saved, system: sys(), theme: resolve(saved, sys()) });
  const on = () => useTheme.getState().setSystem(sys());
  mq.addEventListener('change', on);
  return () => mq.removeEventListener('change', on);
}
