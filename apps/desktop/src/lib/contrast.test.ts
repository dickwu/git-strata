import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { contrast, mix } from './contrast';
import { CSS_VAR, TOKENS, type ThemeName } from './tokens';

// gui.md §8.2: ui_accent_contrast.
describe('ui_accent_contrast', () => {
  for (const name of ['light', 'dark'] as ThemeName[]) {
    const t = TOKENS[name];
    test(`${name}: accent on app background >= 4.5:1`, () => {
      expect(contrast(t.accent, t.bgApp)).toBeGreaterThanOrEqual(4.5);
    });
    test(`${name}: accent on panel >= 4.5:1`, () => {
      expect(contrast(t.accent, t.bgPanel)).toBeGreaterThanOrEqual(4.5);
    });
    test(`${name}: on-accent on accent >= 4.5:1`, () => {
      expect(contrast(t.onAccent, t.accent)).toBeGreaterThanOrEqual(4.5);
    });
    // Repo tags and filter chips: accent-text on a 12% accent tint; selected nav row: text
    // on a 16% tint. Checked over both surfaces the list can sit on.
    test(`${name}: teal tag text and tinted selection >= 4.5:1`, () => {
      for (const bg of [t.bgApp, t.bgPanel]) {
        expect(contrast(t.accentText, mix(t.accent, 12, bg))).toBeGreaterThanOrEqual(4.5);
        expect(contrast(t.text, mix(t.accent, 16, bg))).toBeGreaterThanOrEqual(4.5);
        expect(contrast(t.textMuted, bg)).toBeGreaterThanOrEqual(4.5);
      }
    });
    test(`${name}: text and muted text on app background >= 4.5:1`, () => {
      expect(contrast(t.text, t.bgApp)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(t.textMuted, t.bgApp)).toBeGreaterThanOrEqual(4.5);
    });
  }

  test('matches the numbers recorded in gui.md', () => {
    expect(contrast('#2E6F8E', '#f6f5f2')).toBeCloseTo(5.09, 1);
    expect(contrast('#6FB3D2', '#0e0d0c')).toBeCloseTo(8.37, 1);
    expect(contrast('#FFFFFF', '#2E6F8E')).toBeCloseTo(5.55, 1);
  });

  test('globals.css carries the same values as tokens.ts', () => {
    const css = readFileSync(join(import.meta.dir, '../app/globals.css'), 'utf8');
    const block = (sel: string) => {
      const i = css.indexOf(sel);
      return css.slice(i, css.indexOf('}', i));
    };
    const blocks: Record<ThemeName, string> = {
      light: block(':root {'),
      dark: block(":root[data-theme='dark'] {"),
    };
    for (const name of ['light', 'dark'] as ThemeName[]) {
      for (const [k, v] of Object.entries(CSS_VAR)) {
        const m = new RegExp(`${v}:\\s*(#[0-9a-fA-F]{6})`).exec(blocks[name]);
        expect(m?.[1]?.toLowerCase(), `${name} ${v}`).toBe(
          TOKENS[name][k as keyof typeof CSS_VAR].toLowerCase()
        );
      }
    }
  });
});
