import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

// A closed antd Modal keeps its content mounted unless destroyOnHidden is set,
// so a Tooltip that was open inside it (e.g. after Esc while hovering) stays on
// screen with nothing under it. Every Modal here must unmount its content.
// Props are prettier-formatted one per line, so the tag ends at a line holding
// only `>` (an arrow's `=>` in a prop never does).
describe('modals', () => {
  const dir = import.meta.dir;
  const files = readdirSync(dir).filter((f) => f.endsWith('.tsx'));

  test('every <Modal> sets destroyOnHidden', () => {
    const modals = files.flatMap((f) => {
      const src = readFileSync(join(dir, f), 'utf8');
      return [...src.matchAll(/<Modal\b[\s\S]*?\n\s*\/?>\n/g)].map((m) => ({ f, tag: m[0] }));
    });
    expect(modals.map((m) => m.f)).toContain('DaemonStatusModal.tsx');
    for (const m of modals)
      expect({ f: m.f, ok: /\bdestroyOnHidden\b/.test(m.tag) }).toEqual({ f: m.f, ok: true });
  });
});
