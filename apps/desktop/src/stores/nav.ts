import { create } from 'zustand';

export type Screen = 'sessions' | 'daemon';

interface NavState {
  screen: Screen;
  session: string | null;
  paletteOpen: boolean;
  sidebarCollapsed: boolean;
  go: (screen: Screen) => void;
  openSession: (id: string | null) => void;
  setPalette: (open: boolean) => void;
  toggleSidebar: () => void;
}

export const useNav = create<NavState>((set) => ({
  screen: 'sessions',
  session: null,
  paletteOpen: false,
  sidebarCollapsed: false,
  go: (screen) => set({ screen, session: null }),
  openSession: (session) => set({ screen: 'sessions', session }),
  setPalette: (paletteOpen) => set({ paletteOpen }),
  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
}));
