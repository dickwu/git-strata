import type { GuiError } from './types';

/** The six read commands the back end registers in P1 (src-tauri/src/lib.rs). */
export type Command =
  'daemon_status' | 'daemon_socket' | 'sessions_list' | 'session_status' | 'du' | 'deps_list';

export const OPEN_EVENT = 'strata://open';

export function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export function asGuiError(e: unknown): GuiError {
  if (e && typeof e === 'object' && 'code' in e && 'message' in e) {
    const o = e as { code: unknown; message: unknown; detail?: unknown };
    return { code: String(o.code), message: String(o.message), detail: o.detail ?? null };
  }
  return { code: 'Unknown', message: e instanceof Error ? e.message : String(e), detail: null };
}

export function reason(e: GuiError): string | null {
  const d = e.detail;
  if (d && typeof d === 'object' && 'reason' in d) {
    const r = (d as { reason: unknown }).reason;
    return typeof r === 'string' ? r : null;
  }
  return null;
}

export function isDaemonDown(e: unknown): boolean {
  return reason(asGuiError(e)) === 'daemon_down';
}

export async function call<T>(cmd: Command, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri()) {
    throw {
      code: 'Unsupported',
      message: 'not running inside Tauri',
      detail: { reason: 'not_tauri' },
    };
  }
  const { invoke } = await import('@tauri-apps/api/core');
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw asGuiError(e);
  }
}
