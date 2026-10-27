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
