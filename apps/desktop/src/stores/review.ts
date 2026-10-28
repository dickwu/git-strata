import { create } from 'zustand';
import type { QueryClient } from '@tanstack/react-query';
import { REFRESH_AFTER_APPROVE, type Outcome } from '@/lib/approve';
import { refreshAfter } from '@/lib/refresh';
import type { WaitingItem } from '@/lib/view';

/** What the review modal shows: a deps proposal or one workspace's conflicts (D6, read-only). */
export type ReviewTarget =
  { kind: 'deps'; id: number; repo: string } | { kind: 'conflict'; session: string; repo: string };

/** The outcome of the last deps_approve call, kept until a review of that proposal shows it. */
export interface Finished {
  id: number;
  outcome: Outcome;
  seen: boolean;
}

interface ReviewState {
  target: ReviewTarget | null;
  /**
   * The proposal whose deps_approve call has not come back yet, or null. Lives here, not in
   * the modal: Stop waiting and Close end the view, not the call, so a reopened modal keeps
   * Approve off instead of sending a second call into the back end's single-flight refusal.
   */
  approving: number | null;
  /** The last call's outcome. A review of that proposal shows it, even one opened later. */
  finished: Finished | null;
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
  markSeen: () => void;
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
  finished: null,
  readyAt: 0,
  open: (target) => set({ target }),
  close: () => set({ target: null }),
  start: (id, call, qc) => {
    if (get().approving !== null) return false;
    set({ approving: id, finished: null });
    const unknown = (e: unknown): Outcome => ({
      kind: 'error',
      error: { code: 'Unknown', message: String(e), detail: null },
    });
    void call()
      .catch(unknown)
      .then((outcome) => {
        set({ approving: null, finished: { id, outcome, seen: false }, readyAt: Infinity });
        return refreshAfter(qc, REFRESH_AFTER_APPROVE).then((readyAt) => set({ readyAt }));
      });
    return true;
  },
  markSeen: () => {
    const f = get().finished;
    if (f && !f.seen) set({ finished: { ...f, seen: true } });
  },
}));
