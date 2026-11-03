import type { QueryClient, QueryKey } from '@tanstack/react-query';

/**
 * Refresh `keys` without cancelling anything. Window reads never join backend poller flights:
 * they wait, discard the poller result, then start a read of their own. Another window
 * flight still refuses overlapping window reads. Thus a
 * cancelled-and-restarted read comes back request_in_flight and the cancelled one's reply
 * is lost. Instead: join the reads already in flight, wait for them to end, then ask for
 * reads that start after that.
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
  const after = Date.now();
  await Promise.all(
    keys.map((queryKey) =>
      qc.invalidateQueries({ queryKey }, { cancelRefetch: false }).catch(() => {})
    )
  );
  return after;
}
