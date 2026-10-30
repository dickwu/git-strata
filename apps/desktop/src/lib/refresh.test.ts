import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { refreshAfter } from './refresh';

/** A query whose reads are resolved by hand, like the back end's single-flight reads. */
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
