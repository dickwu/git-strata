import { create } from 'zustand';

/** localStorage key for the sidebar state; same pattern as `git-strata.theme`. */
export const SIDEBAR_KEY = 'git-strata.sidebar';

interface LayoutState {
  sidebarCollapsed: boolean;
  setSidebarCollapsed: (collapsed: boolean) => void;
  toggleSidebar: () => void;
}

export const useLayout = create<LayoutState>((set, get) => ({
  sidebarCollapsed: false,
  setSidebarCollapsed: (sidebarCollapsed) => {
    try {
      localStorage.setItem(SIDEBAR_KEY, sidebarCollapsed ? 'collapsed' : 'expanded');
    } catch {
      /* storage unavailable: keep the state for this run only */
    }
    set({ sidebarCollapsed });
  },
  toggleSidebar: () => get().setSidebarCollapsed(!get().sidebarCollapsed),
}));

/**
 * Read the saved sidebar state. Called from an effect, not at module load, so the static
 * export hydrates with the default and then applies the saved state (no hydration mismatch).
 */
export function initLayout(): void {
  try {
    const v = localStorage.getItem(SIDEBAR_KEY);
    if (v === 'collapsed' || v === 'expanded') {
      useLayout.setState({ sidebarCollapsed: v === 'collapsed' });
    }
  } catch {
    /* ignore */
  }
}

/** ⌘\ (Ctrl+\ off macOS) toggles the sidebar. R2 has no sidebar shortcut; this follows VS Code. */
export function isSidebarShortcut(e: {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}): boolean {
  return (e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key === '\\';
}
