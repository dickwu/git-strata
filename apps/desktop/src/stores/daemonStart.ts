import { create } from 'zustand';
import { startDaemon } from '@/lib/tauri';
import type { StartProgress, Started } from '@/lib/types';

/**
 * One start at a time across the window: the page notice and the Daemon modal can both show
 * the button, and they share this state (the back end also refuses a second start).
 */
interface StartState {
  running: boolean;
  progress: StartProgress | null;
  error: unknown;
  start: () => Promise<Started | null>;
  clear: () => void;
}

export const useDaemonStart = create<StartState>((set, get) => ({
  running: false,
  progress: null,
  error: null,
  start: async () => {
    if (get().running) return null;
    set({ running: true, progress: null, error: null });
    try {
      const done = await startDaemon((p) => set({ progress: p }));
      set({ running: false, progress: null });
      return done;
    } catch (e) {
      set({ running: false, progress: null, error: e });
      return null;
    }
  },
  clear: () => set({ error: null }),
}));
