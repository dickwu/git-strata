'use client';

import { useStore } from 'zustand';
import { createStore } from 'zustand/vanilla';
import { asGuiError } from './tauri';
import type { GuiError } from './types';

type SettingsReply = { Ok: boolean } | { Err: GuiError };

interface SettingsEvents {
  listen: (receive: (reply: SettingsReply) => void) => Promise<() => void>;
  read: () => Promise<void>;
  write: (enabled: boolean) => Promise<void>;
}

// T04 owns the scheduler and persists this preference in the native check cache.
const nativeEvents: SettingsEvents = {
  listen: async (receive) => {
    const { listen } = await import('@tauri-apps/api/event');
    return listen<SettingsReply>('strata://update-settings', ({ payload }) => receive(payload));
  },
  read: async () => (await import('@tauri-apps/api/event')).emit('strata://update-settings-get'),
  write: async (enabled) =>
    (await import('@tauri-apps/api/event')).emit('strata://update-settings-set', enabled),
};

interface UpdateSettings {
  automatic?: boolean;
  saving: boolean;
  error?: string;
  connect: () => () => void;
  setAutomatic: (enabled: boolean) => Promise<void>;
}

export function createUpdateSettingsStore(events: SettingsEvents = nativeEvents) {
  return createStore<UpdateSettings>((set, get) => {
    let connection: object | undefined;
    function fail(error: unknown) {
      set({ saving: false, error: asGuiError(error).message });
    }

    return {
      saving: false,
      connect: () => {
        const current = {};
        connection = current;
        let unlisten: (() => void) | undefined;
        set({ automatic: undefined, saving: false, error: undefined });
        void events
          .listen((reply) => {
            if (connection !== current) return;
            if ('Ok' in reply) set({ automatic: reply.Ok, saving: false, error: undefined });
            else fail(reply.Err);
          })
          .then((dispose) => {
            if (connection !== current) return dispose();
            unlisten = dispose;
            return events.read();
          })
          .catch((error) => {
            if (connection === current) fail(error);
          });
        return () => {
          if (connection === current) connection = undefined;
          unlisten?.();
        };
      },
      setAutomatic: async (enabled) => {
        const current = connection;
        const { automatic, saving } = get();
        if (!current || automatic === undefined || automatic === enabled || saving) return;
        set({ saving: true, error: undefined });
        try {
          await events.write(enabled);
        } catch (error) {
          if (connection === current) fail(error);
        }
      },
    };
  });
}

const store = createUpdateSettingsStore();

export const useUpdateSettings = () => useStore(store);
