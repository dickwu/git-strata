import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryObserver } from '@tanstack/react-query';
import type { Outcome } from './approve';
import type { GuiError } from './types';
import { useReview } from '../stores/review';
import { refreshAfter } from './refresh';

/** Hand-resolved TanStack reads; refreshAfter must join them without cancelling. */
function setup() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const reads: Array<{ started: number; resolve: (v: number) => void }> = [];
  let n = 0;
  const observer = new QueryObserver(qc, {
    queryKey: ['deps'],
    queryFn: ({ signal }) =>
      new Promise<number>((resolve, reject) => {
        const i = ++n;
        reads.push({ started: Date.now(), resolve: () => resolve(i) });
        signal.addEventListener('abort', () => reject(new Error('cancelled')));
      }),
  });
  const unsubscribe = observer.subscribe(() => {});
  return { qc, reads, unsubscribe };
}

const tick = () => new Promise((r) => setTimeout(r, 5));

// Poll until `done()` holds, bounded so a regression fails instead of hanging. Under load a
// single fixed tick may not cover refreshAfter's internal timer plus the query turns.
async function until(done: () => boolean, ms = 2000) {
  const end = Date.now() + ms;
  while (!done()) {
    if (Date.now() > end) throw new Error('condition not reached within ' + ms + ' ms');
    await new Promise((r) => setTimeout(r, 1));
  }
}

describe('refreshAfter: joins in-flight reads, never cancels them', () => {
  test('the outstanding read is kept, and a new one starts after it ends', async () => {
    const { qc, reads, unsubscribe } = setup();
    await tick();
    expect(reads.length).toBe(1); // the observer's first read, still out
    let after = 0;
    const done = refreshAfter(qc, [['deps']]).then((t) => (after = t));
    await tick();
    expect(reads.length).toBe(1); // joined, not cancelled and restarted
    reads[0].resolve(0);
    await tick();
    expect(qc.getQueryData(['deps'])).toBe(1); // the original reply landed
    expect(reads.length).toBe(2); // then a read that began after it
    reads[1].resolve(0);
    await done;
    expect(reads[1].started).toBeGreaterThanOrEqual(after);
    expect(qc.getQueryData(['deps'])).toBe(2);
    expect(qc.getQueryState(['deps'])!.dataUpdatedAt).toBeGreaterThanOrEqual(after);
    unsubscribe();
  });
});

for (const fails of [false, true]) {
  test(`second refresh wave waits out a backend poller: ${fails ? 'daemon_timeout keeps Approve off' : 'only its own fresh reply enables Approve'}`, async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    let calls = 0;
    let releasePoller!: () => void;
    let releaseFresh!: () => void;
    const timeout: GuiError = {
      code: 'Busy',
      message: 'Timed out',
      detail: { reason: 'daemon_timeout' },
    };
    const poller = new Promise<number>((resolve) => {
      releasePoller = () => resolve(-1); // Distinct old reply, which must be discarded.
    });
    const freshReply = new Promise<number>((resolve, reject) => {
      releaseFresh = () => (fails ? reject(timeout) : resolve(3));
    });
    const starts: number[] = [];
    const observer = new QueryObserver<number, GuiError>(qc, {
      queryKey: ['deps'],
      queryFn: async () => {
        const call = ++calls;
        if (call <= 2) return call; // Initial cache, then first refresh wave.
        // The native window caller waits out the poller before its own second-wave read.
        await poller;
        starts.push(Date.now());
        return freshReply;
      },
    });
    const unsubscribe = observer.subscribe(() => {});
    let stopReview = () => {};
    try {
      await tick();
      const since = Date.now();
      let boundary = 0;
      const invalidate = qc.invalidateQueries.bind(qc);
      qc.invalidateQueries = (...args: Parameters<typeof invalidate>) => {
        if (args[0]?.queryKey?.[0] === 'deps') boundary = Date.now();
        return invalidate(...args);
      };
      // Same freshness gate as ReviewModal, using the store's real approval refresh below.
      const fresh = (readyAt: number) => {
        const deps = observer.getCurrentResult();
        return deps.dataUpdatedAt >= Math.max(since, readyAt) && !deps.isError;
      };
      useReview.setState({ target: null, approving: null, unseen: {}, readyAt: 0 });
      const outcome: Outcome = {
        kind: 'error',
        error: { code: 'Busy', message: 'Try again', detail: null },
      };
      expect(useReview.getState().start(22, async () => outcome, qc)).toBe(true);
      let settled = false;
      const finished = new Promise<number>((resolve) => {
        stopReview = useReview.subscribe(({ readyAt }) => {
          if (readyAt > 0 && Number.isFinite(readyAt)) {
            settled = true;
            resolve(readyAt); // refreshAfter has resolved, including on a read rejection.
          }
        });
      });
      await until(() => calls === 3);
      expect(calls).toBe(3);
      expect(boundary).toBeGreaterThanOrEqual(since);
      expect(Number.isFinite(boundary)).toBe(true);
      expect(qc.getQueryData(['deps'])).toBe(2);
      expect(starts).toHaveLength(0);
      const previousUpdatedAt = observer.getCurrentResult().dataUpdatedAt;
      expect(observer.getCurrentResult().dataUpdatedAt).toBeLessThan(boundary);
      expect(fresh(boundary)).toBe(false); // Finite T, independent of readyAt's Infinity gate.
      expect(fresh(useReview.getState().readyAt)).toBe(false);
      expect(settled).toBe(false);

      releasePoller();
      await until(() => starts.length === 1);
      expect(starts).toHaveLength(1);
      expect(starts[0]).toBeGreaterThanOrEqual(boundary);
      expect(qc.getQueryData(['deps'])).toBe(2); // The fake's poller value (-1) is never a query reply, so the cache still holds the first wave.
      expect(observer.getCurrentResult().dataUpdatedAt).toBeLessThan(boundary);
      expect(fresh(boundary)).toBe(false);
      expect(settled).toBe(false);

      releaseFresh();
      const readyAt = await finished;
      expect(readyAt).toBeGreaterThanOrEqual(since);
      expect(readyAt).toBeLessThanOrEqual(boundary);
      expect(previousUpdatedAt).toBeLessThan(readyAt);
      expect(qc.getQueryData(['deps'])).toBe(fails ? 2 : 3);
      expect(fresh(boundary)).toBe(!fails);
      expect(fresh(readyAt)).toBe(!fails);
      expect(observer.getCurrentResult().error).toEqual(fails ? timeout : null);
      expect(useReview.getState().approving).toBeNull();
      expect(useReview.getState().unseen[22]).toEqual(outcome);
    } finally {
      stopReview();
      unsubscribe();
      qc.clear();
      useReview.setState({ target: null, approving: null, unseen: {}, readyAt: 0 });
    }
  });
}
