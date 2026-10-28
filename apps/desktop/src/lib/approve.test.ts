import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { inputsChanged, verdict, type Outcome } from './approve';
import { copy, promptText } from './copy';
import type { ApproveReply, GuiError } from './types';
import applied from './fixtures/approve/applied.json';
import skipped from './fixtures/approve/skipped.json';
import failed from './fixtures/approve/failed.json';
import unknownRow from './fixtures/approve/unknown_row.json';
import errors from './fixtures/approve/errors.json';

const reply = (r: unknown): Outcome => ({ kind: 'reply', reply: r as ApproveReply });
const err = (k: keyof typeof errors): Outcome => ({ kind: 'error', error: errors[k] as GuiError });
const r = copy.review.result;

describe('approve verdict: headline priority Unknown > errors > skipped > moved', () => {
  test('applied: success, moved count, conflicts per row, no way back to Approve', () => {
    const v = verdict(22, reply(applied));
    expect(v.tone).toBe('success');
    expect(v.title).toBe('Approved. Moved 2 sessions.');
    expect(v.again).toBe(false);
    expect(v.rows.map((x) => [x.session, x.state, x.conflicts])).toEqual([
      ['s-aaa', 'moved', 2],
      ['s-bbb', 'moved', 0],
    ]);
  });

  test('skipped: warning, proposal stays waiting, a reason per row', () => {
    const v = verdict(22, reply(skipped));
    expect(v.tone).toBe('warning');
    expect(v.title).toBe('Approved, but 4 sessions were skipped, so proposal 22 stays waiting.');
    expect(v.again).toBe(true);
    const by = Object.fromEntries(v.rows.map((x) => [x.session, x]));
    expect(by['s-ccc'].detail).toBe(
      'Files in the old tree are open: node_modules/.bin/vite (node, pid 4242)'
    );
    expect(by['s-ddd'].detail).toBe('Has its own dependency edits (proposal 21)');
    expect(by['s-eee'].detail).toBe('Has its own dependency edits');
    expect(by['s-fff'].detail).toBe('Not ready yet');
    // Skips sort before moves.
    expect(v.rows[v.rows.length - 1]?.state).toBe('moved');
  });

  test('failed moves outrank skips and show code + detail; lock_wait names its holder', () => {
    const v = verdict(22, reply(failed));
    expect(v.tone).toBe('error');
    expect(v.title).toBe('3 sessions failed to move, so proposal 22 stays waiting.');
    const by = Object.fromEntries(v.rows.map((x) => [x.session, x]));
    expect(by['s-ggg'].state).toBe('failed');
    expect(by['s-ggg'].label).toBe('✕ Failed');
    expect(by['s-ggg'].detail).toBe('OpenFiles: /v/s-ggg/admin/node_modules (node (pid 9))');
    expect(by['s-hhh'].detail).toBe(
      'Busy: Another change (publish, s-hhh) was holding session s-hhh. Nothing moved. Try again; it asks for confirmation again.'
    );
    expect(by['s-iii'].detail).toBe('IoError: disk full');
  });

  test('an Unknown row outranks everything and hides Approve again', () => {
    const v = verdict(22, reply(unknownRow));
    expect(v.tone).toBe('unknown');
    expect(v.title).toBe('Result unknown for s-jjj: the move may have happened.');
    expect(v.again).toBe(false);
    expect(v.rows[0]).toMatchObject({ session: 's-jjj', state: 'unknown', label: '? Unknown' });
    expect(v.rows[0].detail).toContain('move interrupted');
  });

  test('a lost reply, socket_closed and Stop waiting are all "Result unknown", no retry', () => {
    for (const o of [err('socket_closed'), { kind: 'detached' } as Outcome]) {
      const v = verdict(22, o);
      expect(v.tone).toBe('unknown');
      expect(v.title).toBe('Result unknown.');
      expect(v.again).toBe(false);
    }
    expect(verdict(22, { kind: 'detached' }).notes[0]).toBe(r.detachedNote);
    // Any Unknown code, whatever its reason.
    const bare = verdict(22, {
      kind: 'error',
      error: { code: 'Unknown', message: 'x', detail: null },
    });
    expect(bare.tone).toBe('unknown');
  });
});

describe('approve verdict: whole-call errors (nothing moved)', () => {
  const cases: Array<[keyof typeof errors, Partial<ReturnType<typeof verdict>>]> = [
    [
      'cancelled',
      { tone: 'neutral', title: 'Not approved.', notes: [r.notConfirmed.cancelled], again: true },
    ],
    [
      'timeout',
      { tone: 'neutral', title: 'Not approved.', notes: [r.notConfirmed.timeout], again: true },
    ],
    ['prompt_in_progress', { tone: 'warning', notes: [r.promptOpen], again: true }],
    [
      'superseded',
      {
        title: 'Proposal 22 was replaced by proposal 28. Nothing moved.',
        replacement: 28,
        again: false,
      },
    ],
    ['superseded_no_replacement', { replacement: 27, again: false }],
    ['already_applied', { title: 'Already approved.', again: false }],
    ['head_moved', { title: r.headMoved, again: false }],
    [
      'lock_wait',
      {
        title:
          'Another change (deps_approve) was holding repo admin. Nothing moved. Try again; it asks for confirmation again.',
        again: true,
      },
    ],
    ['lock_wait_no_holder', { title: r.lockWait('holder unknown', 'repo admin') }],
    ['shutting_down', { title: r.shuttingDown, again: true }],
    ['build_lock_busy', { title: r.buildLock, again: true }],
    ['daemon_down', { title: r.daemonDown }],
    ['in_flight', { title: r.inFlight, again: false }],
    [
      'unsupported',
      { title: "This Mac can't show the confirmation prompt (no_gui_session).", again: false },
    ],
    ['other', { tone: 'error', title: 'something odd (Internal, weird)' }],
  ];
  for (const [k, want] of cases)
    test(k, () => {
      expect(verdict(22, err(k))).toMatchObject(want);
    });
});

describe('review copy', () => {
  test('the prompt text matches the core reason (strata-confirm reason.rs)', () => {
    expect(promptText(22)).toBe('approve dependency proposal 22');
    expect(copy.review.prompt(22)).toContain('“approve dependency proposal 22”');
    const core = join(import.meta.dir, '../../../../crates/strata-confirm/src/reason.rs');
    expect(existsSync(core)).toBe(true);
    expect(readFileSync(core, 'utf8')).toContain('"approve dependency proposal {}"');
  });
  test('the scope note states the G2 waiver (D2=b): whole proposal, as the daemon sees it', () => {
    const s = copy.review.scope(22, 'admin');
    expect(s).toContain('every session on admin');
    expect(s).toContain('as the daemon sees it then');
  });
  test('terminal command', () => {
    expect(copy.review.command(22)).toBe('git-strata deps approve 22');
  });
  test('inputs changed', () => {
    expect(inputsChanged({ from_inputs: 'a', to_inputs: 'b' })).toBe(true);
    expect(inputsChanged({ from_inputs: 'a', to_inputs: 'a' })).toBe(false);
    expect(inputsChanged({})).toBeNull();
  });
  test('no "we" in review copy', () => {
    const all =
      JSON.stringify(copy.review) +
      Object.values(copy.review.result)
        .filter((f) => typeof f === 'function')
        .map((f) => String(f))
        .join(' ');
    expect(/\bwe\b/i.test(all)).toBe(false);
  });
});
