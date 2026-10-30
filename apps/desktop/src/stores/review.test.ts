import { beforeEach, describe, expect, test } from 'bun:test';
import { QueryClient } from '@tanstack/react-query';
import type { Outcome } from '@/lib/approve';
import { useReview } from './review';

const reply: Outcome = {
  kind: 'reply',
  reply: { results: [], proposal: { id: 22, repo: 'admin', state: { kind: 'Applied' } } as never },
};
const tick = () => new Promise((r) => setTimeout(r, 5));

function deferred() {
  let resolve!: (o: Outcome) => void;
  const promise = new Promise<Outcome>((r) => (resolve = r));
  return { promise, resolve };
}

beforeEach(() => {
  useReview.setState({ target: null, approving: null, unseen: {}, readyAt: 0 });
});

describe('review store: an approval outlives the modal that sent it (§4.4)', () => {
  test('Stop waiting, Close, reopen: the outcome is kept for the reopened view', async () => {
    const qc = new QueryClient();
    const call = deferred();
    const s = useReview.getState();
    s.open({ kind: 'deps', id: 22, repo: 'admin' });
    expect(s.start(22, () => call.promise, qc)).toBe(true);
    expect(useReview.getState().approving).toBe(22);
    // The modal is closed and reopened while the call is out: no second call.
    s.close();
    s.open({ kind: 'deps', id: 22, repo: 'admin' });
    expect(useReview.getState().start(22, () => call.promise, qc)).toBe(false);
    call.resolve(reply);
    await tick();
    const st = useReview.getState();
    expect(st.approving).toBeNull();
    expect(st.unseen[22]).toEqual(reply);
    st.markSeen(22);
    expect(useReview.getState().unseen[22]).toBeUndefined();
  });

  test('Approve stays off until the refresh after the outcome has been asked for', async () => {
    const qc = new QueryClient();
    const call = deferred();
    const before = Date.now();
    useReview.getState().start(22, () => call.promise, qc);
    call.resolve(reply);
    await Promise.resolve();
    await Promise.resolve();
    expect(useReview.getState().readyAt).toBe(Infinity);
    await tick();
    expect(useReview.getState().readyAt).toBeGreaterThanOrEqual(before);
    expect(useReview.getState().readyAt).toBeLessThan(Infinity);
  });

  test('a call that throws is a result unknown, never a lost outcome', async () => {
    const qc = new QueryClient();
    useReview.getState().start(22, () => Promise.reject(new Error('boom')), qc);
    await tick();
    expect(useReview.getState().unseen[22]).toMatchObject({
      kind: 'error',
      error: { code: 'Unknown' },
    });
  });

  test('approving another proposal keeps an unseen outcome (Stop waiting on A, then B)', async () => {
    const qc = new QueryClient();
    const a = deferred();
    const b = deferred();
    const s = useReview.getState();
    s.start(22, () => a.promise, qc);
    s.close();
    s.open({ kind: 'deps', id: 23, repo: 'tool' });
    // A's outcome lands while B is open: B's review must not consume it.
    a.resolve(reply);
    await tick();
    expect(useReview.getState().start(23, () => b.promise, qc)).toBe(true);
    useReview.getState().markSeen(23);
    b.resolve(reply);
    await tick();
    const st = useReview.getState();
    expect(st.unseen[22]).toEqual(reply);
    expect(st.unseen[23]).toEqual(reply);
    st.markSeen(23);
    expect(Object.keys(useReview.getState().unseen)).toEqual(['22']);
  });
});
