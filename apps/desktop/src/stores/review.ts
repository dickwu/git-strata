import { create } from 'zustand';
import type { QueryClient } from '@tanstack/react-query';
import { REFRESH_AFTER_APPROVE, type Outcome } from '@/lib/approve';
import { refreshAfter } from '@/lib/refresh';
import type { WaitingItem } from '@/lib/view';

/** What the review modal shows: a deps proposal or one workspace's conflicts (D6, read-only). */
export type ReviewTarget =
  { kind: 'deps'; id: number; repo: string } | { kind: 'conflict'; session: string; repo: string };

/**
 * Outcomes no review has shown yet, by proposal id. Each stays until a review of its own
 * proposal shows it, so approving another proposal never drops one.
 */
export type Unseen = Readonly<Record<number, Outcome>>;

interface ReviewState {
  target: ReviewTarget | null;
  /**
   * The proposal whose deps_approve call has not come back yet, or null. Lives here, not in
   * the modal: Stop waiting and Close end the view, not the call, so a reopened modal keeps
   * Approve off instead of sending a second call into the back end's single-flight refusal.
   */
  approving: number | null;
  /** Outcomes not shown yet. A review of that proposal shows its own, even one opened later. */
  unseen: Unseen;
  /**
   * Approve stays off until a read with `dataUpdatedAt >= readyAt` has landed: a read that
   * began after the last call ended. Infinity while that refresh is being asked for.
   */
  readyAt: number;
  open: (t: ReviewTarget) => void;
  close: () => void;
  /**
   * Send one approval. `call` performs it and never rejects; the outcome is stored and the
   * lists refreshed whether or not any modal is still open. False when one is already out.
   */
  start: (id: number, call: () => Promise<Outcome>, qc: QueryClient) => boolean;
  /** Drop proposal `id`'s unseen outcome once a review has shown it. */
  markSeen: (id: number) => void;
}

export function targetOf(it: WaitingItem): ReviewTarget | null {
  if (it.kind === 'deps') return { kind: 'deps', id: it.number, repo: it.repo };
  return it.session ? { kind: 'conflict', session: it.session, repo: it.repo } : null;
}

export function targetKey(t: ReviewTarget): string {
  return t.kind === 'deps' ? `deps:${t.id}` : `conflict:${t.session}:${t.repo}`;
}

export const useReview = create<ReviewState>((set, get) => ({
  target: null,
  approving: null,
  unseen: {},
  readyAt: 0,
  open: (target) => set({ target }),
  close: () => set({ target: null }),
  start: (id, call, qc) => {
    if (get().approving !== null) return false;
    set({ approving: id });
    const unknown = (e: unknown): Outcome => ({
      kind: 'error',
      error: { code: 'Unknown', message: String(e), detail: null },
    });
    void call()
      .catch(unknown)
      .then((outcome) => {
        set((st) => ({
          approving: null,
          unseen: { ...st.unseen, [id]: outcome },
          readyAt: Infinity,
        }));
        return refreshAfter(qc, REFRESH_AFTER_APPROVE).then((readyAt) => set({ readyAt }));
      });
    return true;
  },
  markSeen: (id) => {
    if (!(id in get().unseen)) return;
    const { [id]: _shown, ...rest } = get().unseen;
    set({ unseen: rest });
  },
}));
