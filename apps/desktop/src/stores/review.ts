import { create } from 'zustand';
import type { WaitingItem } from '@/lib/view';

/** What the review modal shows: a deps proposal or one workspace's conflicts (D6, read-only). */
export type ReviewTarget =
  { kind: 'deps'; id: number; repo: string } | { kind: 'conflict'; session: string; repo: string };

interface ReviewState {
  target: ReviewTarget | null;
  /**
   * The proposal whose deps_approve call has not come back yet, or null. Survives the modal
   * (Stop waiting closes the view, not the call), so a reopened modal keeps Approve off
   * instead of sending a second call into the back end's single-flight refusal.
   */
  approving: number | null;
  open: (t: ReviewTarget) => void;
  close: () => void;
  setApproving: (id: number | null) => void;
}

export function targetOf(it: WaitingItem): ReviewTarget | null {
  if (it.kind === 'deps') return { kind: 'deps', id: it.number, repo: it.repo };
  return it.session ? { kind: 'conflict', session: it.session, repo: it.repo } : null;
}

export function targetKey(t: ReviewTarget): string {
  return t.kind === 'deps' ? `deps:${t.id}` : `conflict:${t.session}:${t.repo}`;
}

export const useReview = create<ReviewState>((set) => ({
  target: null,
  approving: null,
  open: (target) => set({ target }),
  close: () => set({ target: null }),
  setApproving: (approving) => set({ approving }),
}));
