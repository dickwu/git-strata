import type { QueryClient, QueryKey } from '@tanstack/react-query';

/**
 * Refresh `keys` without cancelling anything. The back end allows one read per kind, so a
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
