import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const here = import.meta.dir;
const src = join(here, '..');
const read = (p: string) => readFileSync(join(src, p), 'utf8');
const modal = read('components/ReviewModal.tsx');
const code = (s: string) => s.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '');

function files(dir: string): string[] {
  return readdirSync(join(src, dir), { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? files(join(dir, e.name)) : /\.tsx?$/.test(e.name) ? [join(dir, e.name)] : []
  );
}

describe('review modal: Approve can only come from a deliberate click (§4.7)', () => {
  test('no autoFocus, no form submit, no key handlers', () => {
    const c = code(modal);
    expect(c).not.toMatch(/autoFocus/);
    expect(c).not.toMatch(/htmlType="submit"/);
    expect(c).not.toMatch(/<form\b|<Form\b/);
    expect(c).not.toMatch(/onKey(Down|Up|Press)|addEventListener\(\s*['"]key/);
    expect(c).not.toMatch(/['"]Enter['"]/);
  });

  test('approveProposal is called once, inside onApprove, which only an onClick uses', () => {
    const c = code(modal);
    expect(c.match(/approveProposal\(/g)?.length).toBe(1);
    const body = c.slice(c.indexOf('const onApprove'), c.indexOf('const onStopWaiting'));
    expect(body).toContain('approveProposal(id)');
    expect(c.match(/onApprove\b/g)?.length).toBe(2);
    expect(c).toMatch(/onClick=\{onApprove\}/);
  });

  test('nothing in the front end names a confirmation bypass or sends a token', () => {
    for (const f of [...files('components'), ...files('lib'), ...files('stores')]) {
      if (f.endsWith('.test.ts')) continue;
      const c = code(read(f));
      expect([f, /\b(confirmed|approved|token|force)\s*:/.test(c)]).toEqual([f, false]);
    }
  });

  test('only tauri.ts calls deps_approve, and always with sessions: null (D2=b, D4)', () => {
    for (const f of [...files('components'), ...files('lib'), ...files('stores')]) {
      if (f.endsWith('.test.ts') || f === join('lib', 'tauri.ts')) continue;
      expect([f, read(f).includes("'deps_approve'")]).toEqual([f, false]);
    }
    const t = code(read('lib/tauri.ts'));
    expect(t.match(/'deps_approve'/g)?.length).toBe(2); // the Command union and the call
    expect(t).toContain("call<ApproveReply>('deps_approve', { proposal, sessions: null })");
    expect(t).not.toMatch(/sessions:\s*\[/);
  });

  test('no Reject (D3 = R1) and no Start daemon inside the review', () => {
    const c = code(modal);
    expect(c).not.toMatch(/reject/i);
    expect(c).not.toMatch(/DaemonDownAlert|ErrorNote error=/);
  });

  test('a busy modal cannot be dismissed by mask, Esc or the close icon', () => {
    expect(modal).toMatch(/maskClosable=\{!busy\}/);
    expect(modal).toMatch(/keyboard=\{!busy\}/);
    expect(modal).toMatch(/closable=\{!busy\}/);
    expect(modal).toMatch(/if \(!busy\) close\(\)/);
  });

  test('every outcome refreshes, in the store (not the modal), and so does Stop waiting', () => {
    const store = code(read('stores/review.ts'));
    const start = store.slice(
      store.indexOf('start: (id, call, qc)'),
      store.lastIndexOf('markSeen:')
    );
    expect(start).toContain('refreshAfter(qc, REFRESH_AFTER_APPROVE)');
    const c = code(modal);
    const stop = c.slice(c.indexOf('const onStopWaiting'), c.indexOf('const onAgain'));
    expect(stop).toContain('refreshAfter(qc, REFRESH_AFTER_APPROVE)');
  });

  test('review refreshes never cancel an in-flight read (all go through refreshAfter)', () => {
    for (const f of ['components/ReviewModal.tsx', 'stores/review.ts']) {
      const c = code(read(f));
      expect([f, /\.(invalidateQueries|refetchQueries)\(/.test(c)]).toEqual([f, false]);
    }
  });

  test('Approve waits for a read after the last approval ended, and for any unseen outcome', () => {
    const c = code(modal);
    expect(c).toContain('deps.dataUpdatedAt >= Math.max(since, readyAt)');
    expect(c).toMatch(/approving === null && !unseen/);
  });

  test('the modal is mounted once, in Shell', () => {
    expect(read('components/Shell.tsx')).toContain('<ReviewModal />');
  });
});
