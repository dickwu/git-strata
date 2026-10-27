import { create } from 'zustand';

export type Screen = 'sessions' | 'daemon';

interface NavState {
  screen: Screen;
  session: string | null;
  paletteOpen: boolean;
  /** Sessions list filter. Lives here, not in the screen, so it survives opening a session and coming back. */
  repoFilter: string[];
  textFilter: string;
  agentFilter: string[];
  toggleRepoFilter: (repo: string) => void;
  toggleAgentFilter: (agent: string) => void;
  addRepoFilter: (repo: string) => void;
  removeRepoFilter: (repo: string) => void;
  setTextFilter: (text: string) => void;
  clearFilters: () => void;
  go: (screen: Screen) => void;
  openSession: (id: string | null) => void;
  setPalette: (open: boolean) => void;
}

/**
 * The session a deep link points at, or null. Accepts `#/session/<id>` (what the app writes
 * while a session is open; works in the static export), `/session/<id>` and `?session=<id>`.
 * Any of them opens the list with that session's detail modal on top.
 */
export function sessionFromUrl(url: {
  pathname: string;
  hash: string;
  search: string;
}): string | null {
  const m =
    /^#?\/?session\/([^/?#]+)\/?$/.exec(url.hash) ?? /^\/session\/([^/?#]+)\/?$/.exec(url.pathname);
  const raw = m?.[1] ?? new URLSearchParams(url.search).get('session');
  if (!raw) return null;
  try {
    return decodeURIComponent(raw) || null;
  } catch {
    return null;
  }
}

/** The hash for an open session, so a reload or a copied link reopens it. */
export const sessionHash = (id: string) => `#/session/${encodeURIComponent(id)}`;

export const useNav = create<NavState>((set) => ({
  screen: 'sessions',
  session: null,
  paletteOpen: false,
  repoFilter: [],
  textFilter: '',
  agentFilter: [],
  toggleRepoFilter: (repo) =>
    set((s) => ({
      repoFilter: s.repoFilter.includes(repo)
        ? s.repoFilter.filter((r) => r !== repo)
        : [...s.repoFilter, repo],
    })),
  toggleAgentFilter: (agent) =>
    set((s) => ({
      agentFilter: s.agentFilter.includes(agent)
        ? s.agentFilter.filter((a) => a !== agent)
        : [...s.agentFilter, agent],
    })),
  addRepoFilter: (repo) =>
    set((s) => (s.repoFilter.includes(repo) ? s : { repoFilter: [...s.repoFilter, repo] })),
  removeRepoFilter: (repo) => set((s) => ({ repoFilter: s.repoFilter.filter((r) => r !== repo) })),
  setTextFilter: (textFilter) => set({ textFilter }),
  clearFilters: () => set({ repoFilter: [], agentFilter: [], textFilter: '' }),
  go: (screen) => set({ screen, session: null }),
  openSession: (session) => set({ screen: 'sessions', session }),
  setPalette: (paletteOpen) => set({ paletteOpen }),
}));
