/**
 * The in-window status bar (S1) model: which state chips to show, in what order, and the
 * right-hand facts. Pure; every input is a daemon reply the app already polls. Design:
 * git-strata-review-notes/status-bar-states/design.md (r3.1) §2, §3.1, §3.3, §3.5, §11.3, §12;
 * gui.md "Status bar states".
 *
 * Step 1 (P-a) runs against a 0.2.2 daemon: no `activity[]`, `busy`, `volume`, `version`,
 * `pid` or `started`, so background work is recognised from the LongHold's own `method`
 * (§11.3) and no free-space or Busy chip exists yet.
 *
 * SHARED INTERFACE (landed first on feat/status-bar-states): slices implement against these
 * types. Slice A owns the bodies below and their tests; slice B renders `StatusBarModel`;
 * slice C renders `WarningView` in the modal and the Daemon screen.
 */
import type { DaemonStatus, DepsList, DuReply, StatusReply } from './types';

/** Severity tokens (design §2): red = a human must act now; never red for any LongHold. */
export type Severity = 'red' | 'amber' | 'purple' | 'blue' | 'muted';

/** Display order of severities; also the chip sort's primary key (design §3.5). */
export const SEVERITY_ORDER: readonly Severity[] = ['red', 'amber', 'purple', 'blue', 'muted'];

/** At most this many chips show in the bar; the rest collapse into "+N" (design §3.5). */
export const MAX_CHIPS = 3;

/** Background holds turn amber after this long (Q3/Q4: a fixed 30-minute cap). */
export const BACKGROUND_HOLD_MAX_MS = 30 * 60_000;

/** Where a chip click lands. `modal` opens the daemon status modal at a section. */
export type ChipTarget =
  | { kind: 'modal'; section: 'daemon' | 'warnings' | 'activity' | 'storage' | 'scan' }
  | { kind: 'waiting' }
  | { kind: 'session'; session: string };

export interface Chip {
  /** Stable key, e.g. `down`, `warning:LongHold:tool`, `activity:catch_up:tool`, `scan`. */
  id: string;
  sev: Severity;
  /**
   * Order inside the severity band (design §3.5): 1 red, 2 amber waiting-for-you, 3 purple
   * conflicts, 4 amber degraded, 5 blue activity, 6 muted. Lower sorts first.
   */
  rank: 1 | 2 | 3 | 4 | 5 | 6;
  /** Plain text (G6: repo/agent text is never HTML). */
  text: string;
  tooltip?: string;
  target?: ChipTarget;
  /** Epoch ms the state began, when known (elapsed is rendered from it). */
  since?: number;
}

/** One `warnings[]` entry classified for display (S1 chips, S3 modal, S4 Daemon screen). */
export interface WarningView {
  /** Variant name, e.g. `LongHold`, `StorePoisoned`. */
  kind: string;
  /** Mirrors `tray_model.rs::warning_class`, extended with LongHold split by method (§3.1). */
  cls: 'error' | 'waiting' | 'notice' | 'activity';
  sev: Severity;
  /** One sentence for people; repo names and ids as plain text. */
  text: string;
  repo?: string;
  session?: string;
  since?: number;
  /** The raw entry, shown only behind "Details" (never in red). */
  raw: unknown;
}

export interface StatusBarInput {
  /** The `daemonStatus` query. */
  daemon: {
    pending: boolean;
    data: DaemonStatus | undefined;
    /** Last error (a GuiError from `call`), null when the last poll answered. */
    error: unknown | null;
    /** Epoch ms of the last successful answer (react-query `dataUpdatedAt`), 0 if none. */
    updatedAt: number;
  };
  sessions: StatusReply | undefined;
  /** Epoch ms of the last sessions answer; used to hide counts from a stale read. */
  sessionsUpdatedAt?: number;
  deps: DepsList | undefined;
  du: DuReply | undefined;
}

export interface StatusBarModel {
  /** The left-hand daemon trigger (dot + name + detail). */
  daemon: { state: 'pending' | 'up' | 'down'; label: string; detail: string | null };
  /** Chips to show, already sorted and capped at MAX_CHIPS. */
  chips: Chip[];
  /** Chips not shown; the bar renders "+{overflow.length}" opening the modal. */
  overflow: Chip[];
  /** Always-on right side (§3.5). `free` is null until a daemon reports `volume` (step 2). */
  right: {
    free: { text: string; sev: Severity } | null;
    saved: { text: string; tooltip: string } | null;
    sessions: number | null;
  };
}

/** Classify one warning. Slice A implements; the stub keeps every warning a plain notice. */
export function classifyWarning(w: unknown, now: number): WarningView {
  void now;
  const kind =
    typeof w === 'string'
      ? w
      : w && typeof w === 'object'
        ? (Object.keys(w)[0] ?? 'Warning')
        : 'Warning';
  return { kind, cls: 'notice', sev: 'amber', text: kind, raw: w };
}

/** Sort by rank then severity, keep order otherwise; split at MAX_CHIPS. Slice A owns it. */
export function capChips(all: Chip[]): { chips: Chip[]; overflow: Chip[] } {
  const sorted = [...all].sort(
    (a, b) => a.rank - b.rank || SEVERITY_ORDER.indexOf(a.sev) - SEVERITY_ORDER.indexOf(b.sev)
  );
  return { chips: sorted.slice(0, MAX_CHIPS), overflow: sorted.slice(MAX_CHIPS) };
}

/** Build the bar. Slice A implements; the stub shows the daemon trigger only. */
export function statusBarModel(input: StatusBarInput, now: number): StatusBarModel {
  void now;
  const up = input.daemon.error === null && input.daemon.data !== undefined;
  return {
    daemon: {
      state: input.daemon.pending ? 'pending' : up ? 'up' : 'down',
      label: input.daemon.pending
        ? 'Connecting to daemon'
        : up
          ? 'Daemon running'
          : 'Daemon unreachable',
      detail: null,
    },
    chips: [],
    overflow: [],
    right: { free: null, saved: null, sessions: input.sessions?.sessions.length ?? null },
  };
}
