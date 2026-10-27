import type { GuiError, StartProgress, Started } from './types';

/**
 * The commands the back end registers (src-tauri/src/lib.rs): the six P1 reads, plus
 * legacy_agent and daemon_start (S0, main window only).
 */
export type Command =
  | 'daemon_status'
  | 'daemon_socket'
  | 'sessions_list'
  | 'session_status'
  | 'du'
  | 'deps_list'
  | 'legacy_agent'
  | 'daemon_start';

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

/**
 * daemon_start: kickstart or bootstrap the launchd agent, then wait for its socket.
 * Progress arrives on a Channel while the call runs (src-tauri/src/commands/service.rs).
 */
export async function startDaemon(onProgress: (p: StartProgress) => void): Promise<Started> {
  if (!inTauri()) return call<Started>('daemon_start');
  const { Channel } = await import('@tauri-apps/api/core');
  const ch = new Channel<StartProgress>();
  ch.onmessage = onProgress;
  return call<Started>('daemon_start', { onProgress: ch });
}
