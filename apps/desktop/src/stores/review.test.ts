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
  useReview.setState({ target: null, approving: null, finished: null, readyAt: 0 });
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
    expect(st.finished).toMatchObject({ id: 22, seen: false, outcome: reply });
    st.markSeen();
    expect(useReview.getState().finished?.seen).toBe(true);
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
    const f = useReview.getState().finished;
    expect(f?.outcome).toMatchObject({ kind: 'error', error: { code: 'Unknown' } });
  });
});
