import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { copy } from '@/lib/copy';

// The result rows (one line per session) are one grid for the whole list, so the status
// column is as wide as the widest label in it. A fixed column let "· No move needed" run
// into the session id while "✓ Moved" fit.
const css = readFileSync(join(import.meta.dir, 'ReviewModal.module.css'), 'utf8');

/** Declarations of the rule whose selector is exactly `selector`, comments stripped. */
function rule(selector: string): string {
  const esc = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const m = css.match(new RegExp(`(?:^|\\n)${esc} \\{([^}]*)\\}`));
  if (!m) throw new Error(`rule ${selector} not found`);
  return m[1].replace(/\/\*[\s\S]*?\*\//g, '');
}

describe('review modal: result rows', () => {
  test('the status column is one track for the whole list, capped, sized by its widest label', () => {
    expect(rule('.rows')).toMatch(/display:\s*grid/);
    expect(rule('.rows')).toMatch(/grid-template-columns:\s*fit-content\(\d+px\) max-content 1fr/);
    expect(rule('.rows li')).toMatch(/grid-template-columns:\s*subgrid/);
    expect(rule('.rows li')).toMatch(/grid-column:\s*1 \/ -1/);
  });

  test('no row sizes its own status column, and a label past the cap wraps instead of overflowing', () => {
    expect(rule('.rows li')).not.toMatch(/\d+px\s+max-content/);
    expect(rule('.rowState')).not.toMatch(/white-space:\s*nowrap/);
  });

  test('detail text sits under the session id; the list still scrolls with separators', () => {
    expect(rule('.rowDetail')).toMatch(/grid-column:\s*2 \/ -1/);
    expect(rule('.rows')).toMatch(/max-height:\s*40vh/);
    expect(rule('.rows')).toMatch(/overflow-y:\s*auto/);
    expect(rule('.rows li')).toMatch(/border-top:\s*1px solid var\(--border\)/);
  });

  test('every row label is a glyph, a space and a few words, short enough to sit on one line', () => {
    const labels = Object.entries(copy.review.result)
      .filter(([k, v]) => /^row[A-Z]/.test(k) && typeof v === 'string')
      .map(([, v]) => v as string);
    expect(labels.length).toBe(5);
    for (const label of labels) {
      expect(label).toMatch(/^\S \S/);
      // Past ~20 characters at 13px the column hits its cap and the label wraps: still
      // legible, but not the intent.
      expect([label, label.length <= 20]).toEqual([label, true]);
    }
  });
});
