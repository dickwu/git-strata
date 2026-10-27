import { copy } from './copy';
import { asGuiError, reason } from './tauri';
import type { LegacyAgent, StartProgress } from './types';

const s = copy.start;

/** What the down notice offers: a Start button, or the setup-needed variant (no plist). */
export function downVariant(agent: LegacyAgent | undefined): 'start' | 'setup' {
  return agent?.state === 'not_installed' ? 'setup' : 'start';
}

/** The live line under the Start button while daemon_start runs. */
export function phaseText(p: StartProgress): string {
  switch (p.phase) {
    case 'launching':
      return p.action === 'bootstrap' ? s.phase.bootstrap : s.phase.kickstart;
    case 'waiting':
      return s.phase.waiting;
    case 'waiting_store':
      return p.tcc ? s.phase.tcc : s.phase.store;
    case 'recovering':
      return s.phase.recovering;
    case 'ready':
      return s.phase.ready;
  }
}

/**
 * A failed start, as copy: what happened (title), how to fix it (hint), and the raw text the
 * fix needs (output: launchctl stderr or the store error), shown verbatim.
 */
export interface StartError {
  title: string;
  hint?: string;
  output?: string;
  /** plist_missing: show the setup variant rather than a bare error. */
  setup?: string;
}

const str = (v: unknown): string | undefined =>
  typeof v === 'string' && v.trim() !== '' ? v.trim() : undefined;

export function startErrorText(e: unknown): StartError {
  const g = asGuiError(e);
  const d = (g.detail && typeof g.detail === 'object' ? g.detail : {}) as Record<string, unknown>;
  switch (reason(g)) {
    case 'plist_missing': {
      const plist = str(d.plist) ?? '~/Library/LaunchAgents/com.gwd.git-stratad.plist';
      return { title: s.errors.plistMissing(plist), hint: s.setup.hint(plist), setup: plist };
    }
    case 'launchctl_failed':
      return {
        title: s.errors.launchctl(
          str(d.step) ?? 'start',
          typeof d.status === 'number' ? d.status : null
        ),
        hint: s.errors.launchctlHint,
        output: str(d.stderr),
      };
    case 'tcc_pending':
      return { title: s.errors.tcc, hint: s.errors.tccHint, output: str(d.store_error) };
    case 'store_unavailable':
      return { title: s.errors.store, hint: s.errors.storeHint, output: str(d.store_error) };
    case 'start_timeout':
      return {
        title: s.errors.timeout(typeof d.seconds === 'number' ? d.seconds : 30),
        hint: s.errors.timeoutHint,
      };
    case 'start_in_flight':
      return { title: s.errors.inFlight };
    case 'not_main_window':
      return { title: s.errors.notMain };
    case 'not_tauri':
      return { title: copy.errors.notInTauri };
    default:
      return { title: g.message || copy.errors.unknown };
  }
}
