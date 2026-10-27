/**
 * Colour tokens that carry a contrast promise (gui.md §8.2). globals.css holds the same
 * values as CSS variables; src/lib/contrast.test.ts checks both the ratios and that the
 * two files agree.
 */
export type ThemeName = 'light' | 'dark';

export interface ThemeTokens {
  bgApp: string;
  bgPanel: string;
  text: string;
  textMuted: string;
  accent: string;
  onAccent: string;
}

export const TOKENS: Record<ThemeName, ThemeTokens> = {
  light: {
    bgApp: '#f6f5f2',
    bgPanel: '#ffffff',
    text: '#181613',
    textMuted: '#6b665e',
    accent: '#2E6F8E',
    onAccent: '#FFFFFF',
  },
  dark: {
    bgApp: '#0e0d0c',
    bgPanel: '#17171a',
    text: '#ecebe7',
    textMuted: '#9b958b',
    accent: '#6FB3D2',
    onAccent: '#0E0D0C',
  },
};

/** Layer fills (strata column, P2). Fill only; text on top uses --text. */
export const LAYERS = {
  base: '#B8AE9C',
  deps: '#8C9A7A',
  chain: '#C98A3D',
} as const;

/** CSS variable name for each contrast-bearing token. */
export const CSS_VAR: Record<keyof ThemeTokens, string> = {
  bgApp: '--bg-app',
  bgPanel: '--bg-panel',
  text: '--text',
  textMuted: '--text-muted',
  accent: '--accent',
  onAccent: '--on-accent',
};
