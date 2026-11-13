/**
 * The in-window status bar (S1) model: which state chips to show, in what order, and the
 * right-hand facts. Pure; every input is a daemon reply the app already polls. Design:
 * git-strata-review-notes/status-bar-states/design.md (r3.1) §2, §3.1, §3.3, §3.5, §11.3, §12;
 * gui.md "Status bar states".
 *
 * Step 1 (P-a) runs against a 0.2.2 daemon: no `activity[]`, `busy`, `volume`, `version`,
 * `pid` or `started`, so background work is recognised from the LongHold's own `method`
 * (§11.3) and no free-space or recorded-Busy chip exists yet.
 *
 * SHARED INTERFACE (landed first on feat/status-bar-states): slices implement against these
 * types. Slice A owns the bodies below and their tests; slice B renders `StatusBarModel`;
 * slice C renders `WarningView` in the modal and the Daemon screen.
 */
import appPackage from '../../package.json';
import { ago, bytes } from './copy';
import { statusBarCopy as t } from './statusBarCopy';
import { asGuiError, isDaemonDown, reason } from './tauri';
import type { DaemonStatus, DepsList, DuReply, StatusReply, SystemTimeJson } from './types';
import {
  hasUpstream,
  repoUpstream,
  STUCK_AFTER,
  unavailableUpstream,
  waitingCount,
  waitingGroups,
} from './view';

/** Severity tokens (design §2): red = a human must act now; never red for any LongHold. */
export type Severity = 'red' | 'amber' | 'purple' | 'blue' | 'muted';

/** Severity tie-break order after a chip's rank (design §3.5). */
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
   * Display priority (design §3.5): 1 red, 2 amber waiting-for-you, 3 purple
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
  /** Tray error/waiting classes, plus method-based LongHold and transient rebase activity. */
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
  /** Local observations from existing daemonStatus completions, never a new daemon field. */
  health?: {
    timeouts: number;
    /** Last successful health completion; 0 when none has been observed. */
    lastOkAt: number;
    /** Counter from the successful reply preceding daemon.data, absent on the first reply. */
    prevAcceptErrors?: number;
  };
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

// Keep this literal list equal to Rust's warning_class Error arm (tested against its source).
const ERROR_WARNING_KINDS: readonly string[] = [
  'StorePoisoned',
  'VerifyFailed',
  'UnknownPublish',
  'UnknownPush',
  'Quarantine',
  'AttachOutside',
];
const HEALTH_FRESH_MS = 60_000;
const UNRESPONSIVE_AFTER_MS = 20_000;
const LONG_WAIT_MAX_MS = 10 * 60_000;
const ACTIVITY_MIN_MS = 3000;
const TRANSIENT_REBASE_REASONS = new Set([
  'recent_writes',
  'changed_during_move',
  'busy',
  'installing',
  'waiting_install',
]);

function record(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}
function string(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}
function finite(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}
function systemTime(value: unknown): SystemTimeJson | undefined {
  const r = record(value);
  const secs = finite(r.secs_since_epoch);
  const nanos = finite(r.nanos_since_epoch);
  return secs !== undefined &&
    Number.isInteger(secs) &&
    secs >= 0 &&
    nanos !== undefined &&
    Number.isInteger(nanos) &&
    nanos >= 0 &&
    nanos < 1e9
    ? { secs_since_epoch: secs, nanos_since_epoch: nanos }
    : undefined;
}
function timestamp(value: unknown): number | undefined {
  const time = systemTime(value);
  if (time) return time.secs_since_epoch * 1000 + time.nanos_since_epoch / 1e6;
  const parsed = typeof value === 'string' ? Date.parse(value) : NaN;
  return Number.isFinite(parsed) ? parsed : undefined;
}
const age = (since: number, now: number) => Math.max(0, now - since);

function warningParts(w: unknown): { kind: string; payload: Record<string, unknown> } {
  const object = record(w);
  const keys = Object.keys(object);
  const kind = typeof w === 'string' ? w : keys.length === 1 ? keys[0] : 'Warning';
  return { kind, payload: record(object[kind]) };
}

/** Queues have no repo field; only an unambiguous holder predating this repo hold is usable. */
function catchUpSession(
  payload: Record<string, unknown>,
  daemon?: DaemonStatus
): string | undefined {
  const holdTime = systemTime(payload.since);
  if (!holdTime) return undefined;
  const candidates = (daemon?.queues ?? []).filter((q) => {
    const time = systemTime(q.since);
    return (
      q.holder === 'catch_up' &&
      string(q.session) &&
      time &&
      (time.secs_since_epoch < holdTime.secs_since_epoch ||
        (time.secs_since_epoch === holdTime.secs_since_epoch &&
          time.nanos_since_epoch <= holdTime.nanos_since_epoch))
    );
  });
  return candidates.length === 1 ? candidates[0].session : undefined;
}

/** Context enriches S1 only; the public two-argument classifier also works on isolated warnings. */
function classify(w: unknown, now: number, daemon?: DaemonStatus): WarningView {
  const { kind, payload: p } = warningParts(w);
  const repo = string(p.repo);
  const session = string(p.session);
  const since = timestamp(p.since);
  const view: WarningView = {
    kind,
    cls: 'notice',
    sev: 'muted',
    text: t.warning.unknown(kind),
    repo,
    session,
    since,
    raw: w,
  };
  if (ERROR_WARNING_KINDS.includes(kind)) {
    view.cls = 'error';
    view.sev = 'red';
  }
  switch (kind) {
    case 'StorePoisoned':
      view.text = t.warning.storePoisoned;
      break;
    case 'VerifyFailed':
      view.text = t.warning.verifyFailed(string(p.message) ?? '');
      break;
    case 'UnknownPublish':
      view.text = t.warning.unknownPublish(finite(p.id)?.toString() ?? '');
      break;
    case 'UnknownPush':
      view.text = t.warning.unknownPush(repo ?? '');
      break;
    case 'Quarantine':
      view.text = t.warning.quarantine(string(p.path) ?? '');
      break;
    case 'AttachOutside':
      view.text = t.warning.attachOutside(string(p.reason) ?? '');
      break;
    case 'ConfigRelaxPending':
      view.cls = 'waiting';
      view.sev = 'amber';
      view.text = t.warning.configRelax(repo ?? '');
      break;
    case 'ConfigReloadFailed':
      view.sev = 'amber';
      view.text = t.warning.configReload(string(p.reason) ?? '');
      break;
    case 'ConfigRestartRequired':
      view.sev = 'amber';
      view.text = t.warning.configRestart(
        Array.isArray(p.fields) ? p.fields.filter((v): v is string => typeof v === 'string') : []
      );
      break;
    case 'FetchFailed':
    case 'AnchorNotAdvanced':
      view.sev = 'amber';
      view.text = t.warning.fetch(repo ?? '');
      break;
    case 'CatalogGrowth':
      view.text = t.warning.catalogGrowth(finite(p.generations));
      break;
    case 'StaleRefLock':
      view.text = t.warning.staleRefLock(repo ?? '');
      break;
    case 'PublishRunning':
      view.text = t.warning.publishRunning;
      break;
    case 'LongWait': {
      const attempt = timestamp(p.attempt_since);
      view.since = attempt;
      view.sev = attempt !== undefined && age(attempt, now) < LONG_WAIT_MAX_MS ? 'amber' : 'muted';
      view.text = t.warning.longWait(
        repo,
        t.elapsed(attempt === undefined ? 0 : age(attempt, now))
      );
      break;
    }
    case 'AutoRebaseStuck': {
      const transient =
        TRANSIENT_REBASE_REASONS.has(string(p.reason) ?? '') &&
        since !== undefined &&
        age(since, now) < STUCK_AFTER;
      view.cls = transient ? 'activity' : 'notice';
      view.sev = transient ? 'blue' : 'amber';
      view.text = transient
        ? t.activity.autoRebaseWaiting(repo ?? '', session, string(p.reason) ?? '')
        : t.warning.autoRebaseStuck(repo ?? '', session, string(p.reason) ?? '');
      break;
    }
    case 'LongHold': {
      const method = string(p.method);
      if (!repo || !method || since === undefined) break;
      const elapsedMs = age(since, now);
      const elapsed = t.elapsed(elapsedMs);
      const newDaemon = p.origin != null || daemon?.features?.includes('daemon_status.activity');
      const background = ['catch_up', 'old_tree', 'periodic_fetch', 'daemon'].includes(method);
      const normal = !newDaemon && background && elapsedMs < BACKGROUND_HOLD_MAX_MS;
      view.cls = normal ? 'activity' : 'notice';
      view.sev = normal ? 'blue' : 'amber';
      view.session = session ?? (method === 'catch_up' ? catchUpSession(p, daemon) : undefined);
      view.text =
        newDaemon || !background
          ? t.warning.abnormalHold(repo, method, elapsed)
          : method === 'catch_up'
            ? t.activity.catchUp(repo, view.session, elapsed)
            : method === 'old_tree'
              ? t.activity.oldTree(repo, elapsed)
              : method === 'periodic_fetch'
                ? t.activity.fetch(repo)
                : t.activity.daemon(repo, elapsed);
      break;
    }
  }
  return view;
}

/** Classify an externally tagged daemon warning, including the 0.2.2 method-based fallback. */
export function classifyWarning(w: unknown, now: number, daemon?: DaemonStatus): WarningView {
  return classify(w, now, daemon);
}

/** Sort by rank then severity, keep order otherwise; split at MAX_CHIPS. */
export function capChips(all: Chip[]): { chips: Chip[]; overflow: Chip[] } {
  const sorted = [...all].sort(
    (a, b) => a.rank - b.rank || SEVERITY_ORDER.indexOf(a.sev) - SEVERITY_ORDER.indexOf(b.sev)
  );
  return { chips: sorted.slice(0, MAX_CHIPS), overflow: sorted.slice(MAX_CHIPS) };
}

/** Minimal pure equivalent of ErrorNote's error title; no component/React import. */
function errorTitle(e: unknown): string {
  const g = asGuiError(e);
  switch (reason(g)) {
    case 'daemon_timeout':
      return t.daemon.timeout(finite(record(g.detail).seconds) ?? 0);
    case 'request_in_flight':
      return t.daemon.busy;
    case 'not_tauri':
      return t.daemon.notInTauri;
    default:
      return g.message || t.daemon.unknownError;
  }
}

function daemonState(
  input: StatusBarInput['daemon'],
  now: number,
  health: StatusBarInput['health']
): {
  trigger: StatusBarModel['daemon'];
  healthChip?: Chip;
  fresh: boolean;
} {
  const state = (
    state: 'pending' | 'down',
    label: string,
    detail: string | null = null,
    fresh = false
  ) => ({
    trigger: { state, label, detail },
    fresh,
  });
  if (input.pending) return state('pending', t.daemon.connecting);
  if (input.error != null) {
    const g = asGuiError(input.error);
    if (g.code === 'Busy') {
      switch (reason(g)) {
        case 'store_unavailable': {
          const errno = record(record(record(g.detail).last_error).detail).errno;
          const text = [1, 4, 13].includes(finite(errno) ?? -1)
            ? t.daemon.permission
            : t.daemon.volumeUnavailable;
          return {
            ...state('down', t.daemon.waitingForVolume),
            healthChip: {
              id: 'volume',
              rank: 1,
              sev: 'red',
              text,
              target: { kind: 'modal', section: 'daemon' },
            },
          };
        }
        case 'starting':
          return state('pending', t.daemon.starting);
        case 'too_many_open_files':
          return state('down', t.daemon.fileHandlesExhausted);
      }
    }
    if (isDaemonDown(input.error)) return state('down', t.daemon.notRunning);
    const lastOkAt = Math.max(health?.lastOkAt ?? 0, input.updatedAt);
    if (
      (health?.timeouts ?? 0) >= 2 &&
      (lastOkAt <= 0 || age(lastOkAt, now) >= UNRESPONSIVE_AFTER_MS)
    )
      return state('down', t.daemon.notResponding, errorTitle(input.error));
    // Below the Down threshold, a cached reply keeps its original freshness and age.
    const fresh =
      input.data !== undefined &&
      input.updatedAt > 0 &&
      age(input.updatedAt, now) <= HEALTH_FRESH_MS;
    const detail =
      lastOkAt > 0 && age(lastOkAt, now) > HEALTH_FRESH_MS
        ? t.daemon.stale(t.elapsed(age(lastOkAt, now)))
        : errorTitle(input.error);
    return state('pending', t.daemon.checking, detail, fresh);
  }
  if (input.data === undefined) return state('pending', t.daemon.connecting);
  if (input.updatedAt <= 0 || age(input.updatedAt, now) > HEALTH_FRESH_MS) {
    const text =
      input.updatedAt > 0
        ? t.daemon.stale(t.elapsed(age(input.updatedAt, now)))
        : t.daemon.checking;
    return state('pending', t.daemon.checking, text === t.daemon.checking ? null : text);
  }
  return {
    trigger: {
      state: 'up',
      label: t.daemon.running,
      detail: t.daemon.live(ago(age(input.updatedAt, now))),
    },
    fresh: true,
  };
}

/** Build S1 from already-polled replies. Historical warnings never become fresh health evidence. */
export function statusBarModel(input: StatusBarInput, now: number): StatusBarModel {
  const health = daemonState(input.daemon, now, input.health);
  const daemon = input.daemon.data;
  const all: Chip[] = health.healthChip ? [health.healthChip] : [];
  const modal = (section: 'daemon' | 'warnings' | 'activity' | 'scan'): ChipTarget => ({
    kind: 'modal',
    section,
  });
  const waiting = waitingCount(input.sessions, input.deps, daemon, now);
  if (waiting > 0)
    all.push({
      id: 'waiting',
      rank: 2,
      sev: 'amber',
      text: t.waiting(waiting),
      target: { kind: 'waiting' },
    });
  const conflicts = waitingGroups(input.sessions, input.deps, daemon, now).filter((g) =>
    g.items.some((i) => i.kind === 'conflict')
  ).length;
  if (conflicts > 0)
    all.push({
      id: 'conflicts',
      rank: 3,
      sev: 'purple',
      text: t.conflicts(conflicts),
      target: { kind: 'waiting' },
    });

  if (health.fresh && daemon) {
    // The modal hides cached warning sections while the current query is in error.
    const warningTarget = input.daemon.error == null ? modal('warnings') : modal('daemon');
    const fds = daemon.fds;
    if (
      fds &&
      ((fds.limit > 0 && fds.open >= fds.limit * 0.9) ||
        (input.health?.prevAcceptErrors !== undefined &&
          fds.accept_errors > input.health.prevAcceptErrors))
    ) {
      all.push({
        id: 'fds',
        rank: 1,
        sev: 'red',
        text: t.daemon.fileHandlesLow,
        target: modal('daemon'),
      });
    }
    const warnings = (daemon.warnings ?? []).map((w) => classify(w, now, daemon));
    const errors = warnings.filter((w) => w.cls === 'error').length;
    if (errors > 0)
      all.push({
        id: 'warnings',
        rank: 1,
        sev: 'red',
        text: t.daemon.needsAttention(errors),
        target: warningTarget,
      });
    const activities = new Map<string, WarningView>();
    const deferred = new Map<string, number>();
    const fetchRepos = new Set<string | undefined>();
    const activityRank = (w: WarningView) =>
      w.kind !== 'LongHold' ? 2 : warningParts(w.raw).payload.method === 'catch_up' ? 0 : 1;
    for (const w of warnings) {
      if (w.cls === 'activity') {
        if (w.since !== undefined && age(w.since, now) < ACTIVITY_MIN_MS) continue;
        const key = w.repo ?? w.kind;
        if (w.kind === 'AutoRebaseStuck') deferred.set(key, (deferred.get(key) ?? 0) + 1);
        const previous = activities.get(key);
        if (
          !previous ||
          activityRank(w) < activityRank(previous) ||
          (activityRank(w) === activityRank(previous) && (w.since ?? now) < (previous.since ?? now))
        )
          activities.set(key, w);
      } else if (w.cls === 'notice' && w.sev === 'amber') {
        const fetching = w.kind === 'FetchFailed' || w.kind === 'AnchorNotAdvanced';
        if (fetching && fetchRepos.has(w.repo)) continue;
        if (fetching) fetchRepos.add(w.repo);
        const payload = warningParts(w.raw).payload;
        const id = [
          'warning',
          fetching ? 'fetch' : w.kind,
          w.repo,
          w.session,
          string(payload.method),
          string(payload.hash),
        ]
          .filter((part) => part !== undefined)
          .join(':');
        all.push({
          id,
          rank: 4,
          sev: 'amber',
          text: w.text,
          since: w.since,
          target: warningTarget,
        });
      }
    }
    const restart = daemon.config?.restart_required;
    if (restart?.length && !warnings.some((w) => w.kind === 'ConfigRestartRequired')) {
      all.push({
        id: 'config-restart',
        rank: 4,
        sev: 'amber',
        text: t.warning.configRestart(restart),
        target: modal('daemon'),
      });
    }
    if (record(daemon.reconcile?.last).panicked === true) {
      all.push({
        id: 'reconcile',
        rank: 4,
        sev: 'amber',
        text: t.daemon.reconcilePanicked,
        target: modal('daemon'),
      });
    }
    if (record(daemon).sessions_audit_error != null) {
      all.push({
        id: 'sessions-audit',
        rank: 4,
        sev: 'amber',
        text: t.daemon.auditFailed,
        target: modal('daemon'),
      });
    }
    if (daemon.version && daemon.version !== appPackage.version) {
      all.push({
        id: 'version',
        rank: 4,
        sev: 'amber',
        text: t.daemon.versionMismatch(appPackage.version, daemon.version),
        target: modal('daemon'),
      });
    }
    const sessionsFresh =
      input.sessionsUpdatedAt !== undefined &&
      input.sessionsUpdatedAt > 0 &&
      age(input.sessionsUpdatedAt, now) <= ('api' in daemon ? 90_000 : 300_000);
    const pending = new Map<string, number>();
    const needsPush = new Set<string>();
    if (hasUpstream(daemon))
      for (const s of input.sessions?.sessions ?? []) {
        // Match the tray: upstream-derived states require session-level enrichment.
        if (!s.status.upstream) continue;
        for (const repo of Object.keys(s.status.workspaces ?? {})) {
          const auto = repoUpstream(s, repo)?.auto;
          if (auto?.reason === 'needs_push') needsPush.add(repo);
          if (sessionsFresh && auto?.state === 'pending')
            pending.set(repo, (pending.get(repo) ?? 0) + 1);
        }
      }
    for (const repo of [...needsPush].sort()) {
      all.push({
        id: `needs-push:${repo}`,
        rank: 4,
        sev: 'amber',
        text: t.needsPush(repo),
        target: modal('daemon'),
      });
    }
    for (const [repo, w] of activities) {
      const lock = w.kind === 'LongHold';
      const text = lock
        ? t.activity.deferred(
            t.activity.counts(w.text, daemon.repo_waiting?.[repo] ?? 0, pending.get(repo) ?? 0),
            deferred.get(repo) ?? 0
          )
        : w.text;
      all.push({
        id: `activity:${repo}`,
        rank: 5,
        sev: 'blue',
        text,
        tooltip: lock ? t.activity.tooltip(repo) : undefined,
        since: w.since,
        target: modal('daemon'),
      });
    }
    const scan = input.du?.scan;
    const scanStarted = timestamp(scan?.started);
    if (
      scan?.running &&
      (scan.full || (scanStarted !== undefined && age(scanStarted, now) > 60_000))
    ) {
      all.push({
        id: 'scan',
        rank: 6,
        sev: 'muted',
        text: t.scan(scan.done, scan.queued),
        target: modal('scan'),
        since: scanStarted,
      });
    }
    const started = timestamp(daemon.started);
    if (started !== undefined && started <= now && age(started, now) < LONG_WAIT_MAX_MS) {
      all.push({
        id: 'restarted',
        rank: 6,
        sev: 'muted',
        text: t.daemon.restarted(ago(age(started, now))),
        target: { kind: 'modal', section: 'daemon' },
        since: started,
      });
    }
  }
  if (unavailableUpstream(input.sessions, daemon).length > 0) {
    all.push({
      id: 'unavailable',
      rank: 6,
      sev: 'muted',
      text: t.unavailable,
      target: modal('daemon'),
    });
  }
  const du = input.du;
  const inexact =
    record(du?.total).exact === false ||
    record(du?.sessions?.all).exact === false ||
    du?.sessions?.list.some((s) => record(s).exact === false);
  const savings = health.fresh && !inexact ? du?.savings?.live : undefined;
  return {
    daemon: health.trigger,
    ...capChips(all),
    right: {
      free: null,
      saved:
        savings && Number.isFinite(savings.savings) && savings.savings > 0
          ? {
              text: t.saved(bytes(savings.savings), !savings.complete),
              tooltip: t.savedTooltip(savings.as_of, !savings.complete),
            }
          : null,
      sessions: input.sessions?.sessions.length ?? null,
    },
  };
}
