import type { QueryClient, QueryKey } from '@tanstack/react-query';

/**
 * Refresh `keys` without cancelling anything. Native window reads wait out backend poller
 * flights, discard their result, then start their own read. Overlapping native window
 * reads are still refused, so cancelling and restarting a TanStack read would lose its
 * reply and return request_in_flight. Instead, join TanStack's own reads already in flight,
 * wait for them to end, then ask for reads that start after that.
 *
 * Resolves with a time T: every read started before the call to `refreshAfter` has ended by
 * T, so a successful read with `dataUpdatedAt >= T` began after the caller's event (an
 * approve outcome, a modal opening) and reflects it. Never rejects.
 */
export async function refreshAfter(
  qc: QueryClient,
  keys: ReadonlyArray<QueryKey>
): Promise<number> {
  await Promise.all(
    keys.map((queryKey) =>
      qc.refetchQueries({ queryKey, type: 'active' }, { cancelRefetch: false }).catch(() => {})
    )
  );
  // TanStack stamps replies in whole milliseconds. Exclude first-wave replies even when
  // they finish in this same tick, and let that boundary pass before starting fresh reads.
  const after = Date.now() + 1;
  await new Promise((resolve) => setTimeout(resolve, 1));
  await Promise.all(
    keys.map((queryKey) =>
      qc.invalidateQueries({ queryKey }, { cancelRefetch: false }).catch(() => {})
    )
  );
  return after;
}
