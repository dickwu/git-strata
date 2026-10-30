import { copy } from './copy';
import { asGuiError, reason } from './tauri';
import type { GuiError, LegacyAgent, StartProgress } from './types';

const s = copy.start;

/**
 * What the down notice offers: a Start button, the setup-needed variant (no plist), or the
 * foreign variant (cli.md D38: not the login home, or another install's agent), which has no
 * button.
 */
export function downVariant(agent: LegacyAgent | undefined): 'start' | 'setup' | 'foreign' {
  if (agent?.state === 'foreign') return 'foreign';
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
  /** foreign_home / foreign_agent: show the foreign variant, no Start button. */
  foreign?: boolean;
}

const str = (v: unknown): string | undefined =>
  typeof v === 'string' && v.trim() !== '' ? v.trim() : undefined;

export function startErrorText(e: unknown): StartError {
  const g = asGuiError(e);
  const d = (g.detail && typeof g.detail === 'object' ? g.detail : {}) as Record<string, unknown>;
  switch (reason(g)) {
    case 'foreign_home': {
      const home = str(d.home) ?? '$HOME';
      const login = str(d.login_home);
      return {
        title: login ? s.foreign.home(home, login) : s.foreign.homeUnknown(home),
        foreign: true,
      };
    }
    case 'foreign_agent':
      return {
        title: s.foreign.agent(
          str(d.loaded_path) ?? 'another plist',
          str(d.expected) ?? 'this home'
        ),
        hint: s.foreign.agentHint,
        foreign: true,
      };
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

/** The foreign variant's text: from a failed start, else from legacy_agent's refusal. */
export function foreignText(
  agent: LegacyAgent | undefined,
  err: StartError | null
): StartError | null {
  if (err?.foreign) return err;
  const r: GuiError | undefined = agent?.state === 'foreign' ? agent.refusal : undefined;
  return r
    ? startErrorText(r)
    : agent?.state === 'foreign'
      ? { title: s.foreign.title, foreign: true }
      : null;
}
