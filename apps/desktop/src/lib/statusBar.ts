/**
 * The in-window status bar (S1) model: which state chips to show, in what order, and the
 * right-hand facts. Pure; every input is a daemon reply the app already polls. Design:
 * git-strata-review-notes/status-bar-states/design.md (r3.1) §2, §3.1, §3.3, §3.5, §11.3, §12;
 * gui.md "Status bar states".
 *
 * Step 1 (P-a) runs against a 0.2.2 daemon: no `activity[]`, `busy`, `volume`, `version`,
 * `pid` or `started`, so background work is recognised from the LongHold's own `method`
 * (§11.3). P-b display sources are gated by `daemon_status.activity`; legacy polling still
 * depends only on `api`. Volume hysteresis is passed in by the component, keeping this pure.
 */
import appPackage from '../../package.json';
import { ago, bytes } from './copy';
import { statusBarCopy as t } from './statusBarCopy';
import { asGuiError, isDaemonDown, reason } from './tauri';
import type {
  BusyEvent,
  DaemonActivity,
  DaemonStatus,
  DepsList,
  DuReply,
  QueueJson,
  StatusReply,
  SystemTimeJson,
} from './types';
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

/**
 * 0.2.2 fallback: a background LongHold turns amber after this long (Q3/Q4: a fixed 30-minute
 * cap, §11.3). A P-b daemon applies the cap itself and reports the result as a LongHold, so its
 * activity chips stay blue (§3.1, §12.1).
 */
export const BACKGROUND_HOLD_MAX_MS = 30 * 60_000;

/** Where a chip click lands. `modal` opens the daemon status modal at a section. */
export type ChipTarget =
  | { kind: 'modal'; section: 'daemon' | 'warnings' | 'activity' | 'storage' | 'trash' | 'scan' }
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
  /** P-b: epoch ms the App first saw the current scan pass running ({@link observeScanRun}). */
  scanSince?: number;
  /**
   * The App saw this daemon replace an earlier one: its pid or start time changed between
   * replies ({@link observeDaemonStart}). A daemon first seen just after it started (a cold
   * start or login) was not restarted as far as the App can tell.
   */
  daemonRestarted?: boolean;
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
  /** Retained by the caller across samples, including a stale or unavailable volume. */
  spaceLevel?: SpaceLevel;
  /** Always-on right side (§3.5). `free` is null until a daemon reports `volume` (step 2). */
  right: {
    free: { text: string; sev: Severity } | null;
    saved: { text: string; tooltip: string } | null;
    sessions: number | null;
  };
}

export type SpaceLevel = 'muted' | 'amber' | 'red';

export function hasActivityStatus(s: DaemonStatus | undefined): boolean {
  return s?.features?.includes('daemon_status.activity') === true;
}

/** Cached volume facts; a stale sample never supplies a current free-space figure. */
export function volumeView(
  s: DaemonStatus | undefined,
  now: number,
  previous: SpaceLevel = 'muted'
) {
  const v = hasActivityStatus(s) ? s?.volume : null;
  if (!v || !Number.isFinite(v.free) || !Number.isFinite(v.size) || v.free < 0 || v.size <= 0)
    return null;
  const sampled = timestamp(v.as_of);
  // A sample newer than `now` (taken after the caller's last clock tick) is fresh.
  const stale = sampled === undefined || age(sampled, now) > 60_000;
  const amber = Math.min(v.size * 0.15, 100_000_000_000);
  const red = Math.min(v.size * 0.05, 20_000_000_000);
  const level: SpaceLevel = stale
    ? previous
    : v.free < red || (previous === 'red' && v.free <= red * 1.1)
      ? 'red'
      : v.free < amber || (previous !== 'muted' && v.free <= amber * 1.1)
        ? 'amber'
        : 'muted';
  return { ...v, stale, level, text: stale ? '—' : t.free(bytes(v.free)) };
}

/**
 * The one rule for the live savings bound, in S1 ("up to X saved") and the modal: it holds
 * only when no live session figure reports `exact: false`, for then allocated bytes stood in
 * for private ones. `savings.live` sums the units of `sessions.all`, so an inexact base,
 * dependency or store figure (seen in `total`) does not affect it.
 */
export function liveSavingsExact(du: DuReply | undefined): boolean {
  const inexact = (f: unknown) => record(f).exact === false;
  return !inexact(du?.sessions?.all) && !list(du?.sessions?.list).some(inexact);
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
/** A reply field that should be a list; anything else (a malformed reply) is empty. */
function list<T = unknown>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
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
  const candidates = list<QueueJson>(daemon?.queues).filter((entry) => {
    const q = record(entry);
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

/**
 * Context enriches S1 only; the public two-argument classifier also works on isolated warnings.
 * `sessions` turns a P-b LongHold's session id into its ticket.
 */
function classify(
  w: unknown,
  now: number,
  daemon?: DaemonStatus,
  sessions?: StatusReply
): WarningView {
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
      const label = sessionLabel(session, sessions);
      view.cls = transient ? 'activity' : 'notice';
      view.sev = transient ? 'blue' : 'amber';
      view.text = transient
        ? t.activity.autoRebaseWaiting(repo ?? '', label, string(p.reason) ?? '')
        : t.warning.autoRebaseStuck(repo ?? '', label, string(p.reason) ?? '');
      break;
    }
    case 'LongHold': {
      const method = string(p.method);
      if (!repo || !method || since === undefined) break;
      const elapsedMs = age(since, now);
      const elapsed = t.elapsed(elapsedMs);
      const pb = hasActivityStatus(daemon);
      const newDaemon = p.origin != null || pb;
      const background = ['catch_up', 'old_tree', 'periodic_fetch', 'daemon'].includes(method);
      // A start later than now (the wall clock stepped back) shows no age under 30 min, so,
      // as in the tray, the fallback never calls it normal work; like a missing start time,
      // it stays a modal notice.
      if (!newDaemon && background && since > now) {
        view.text = t.warning.holdAheadOfClock(repo, method);
        break;
      }
      const normal = !newDaemon && background && elapsedMs < BACKGROUND_HOLD_MAX_MS;
      view.cls = normal ? 'activity' : 'notice';
      view.sev = normal ? 'blue' : 'amber';
      view.session =
        session ?? (!pb && method === 'catch_up' ? catchUpSession(p, daemon) : undefined);
      view.text = pb
        ? t.warning.reportedHold(repo, method, elapsed, sessionLabel(view.session, sessions))
        : newDaemon || !background
          ? t.warning.abnormalHold(repo, method, elapsed)
          : method === 'catch_up'
            ? t.activity.catchUp(repo, sessionLabel(view.session, sessions), elapsed)
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

function sessionLabel(id: string | null | undefined, sessions: StatusReply | undefined) {
  if (!id) return undefined;
  return (
    sessions?.sessions.find((s) => s.status.manifest.id === id)?.status.manifest.ticket ||
    id.slice(0, 10)
  );
}

/** `activity[]` entries that can be shown: objects with a finite `elapsed_ms`. */
export function activityList(s: DaemonStatus): DaemonActivity[] {
  return (Array.isArray(s.activity) ? s.activity : []).filter(
    (a) => finite(record(a).elapsed_ms) !== undefined
  );
}

function activityRepos(a: DaemonActivity): string[] {
  if (a.kind === 'session_create')
    return Array.isArray(a.repos) ? a.repos.filter((r) => string(r) !== undefined) : [];
  const repo = string(a.repo);
  return repo ? [repo] : [];
}

function activityText(a: DaemonActivity, sessions: StatusReply | undefined): string | undefined {
  const session = sessionLabel(a.session, sessions);
  const repo = string(a.repo) ?? '';
  switch (a.kind) {
    case 'move':
      return `${t.activity.move(a.cause, session ?? 'session')} · ${repo}`;
    case 'deps_rollout':
      return `${t.activity.rollout(a.done, a.sessions)} · ${repo}`;
    case 'base_build':
      return `${t.activity.base} · ${repo}`;
    case 'session_create':
      return t.activity.create(session, activityRepos(a).length);
    case 'deps_install':
      return `${t.activity.install} · ${repo}`;
    case 'publish':
      return t.activity.publish(repo);
    case 'push':
      return t.activity.push(repo);
  }
}

/**
 * Display the outermost (oldest) work per repo; parent rollout/create entries cover children.
 * A dependency install holds no repo lock, so no request waits for it: lock-holding work in its
 * repo is shown first, however much younger, and the install's chip carries no waiting count.
 * Always blue: what is abnormal (a step past 30 min, a background hold past 30 min, a request
 * hold past 30 s) the daemon reports as a LongHold, which is the amber chip (§3.1, §12.1, Q3).
 */
function reportedActivityChips(
  input: StatusBarInput,
  now: number,
  pending: Map<string, number>,
  target: ChipTarget
): Chip[] {
  const daemon = input.daemon.data!;
  const work = activityList(daemon);
  const chips: Chip[] = [];
  const covered = new Set<string>();
  const lockFree = (a: DaemonActivity) => a.kind === 'deps_install';
  const oldest = [...work].sort((a, b) => b.elapsed_ms - a.elapsed_ms || a.id - b.id);
  for (const a of [...oldest.filter((a) => !lockFree(a)), ...oldest.filter(lockFree)]) {
    const elapsed = a.elapsed_ms + age(input.daemon.updatedAt, now);
    if (elapsed < ACTIVITY_MIN_MS) continue;
    const repos = activityRepos(a);
    if (repos.some((repo) => covered.has(repo))) continue;
    const text = activityText(a, input.sessions);
    if (!text) continue;
    repos.forEach((repo) => covered.add(repo));
    const phase = string(a.kind === 'move' ? a.phase : a.kind === 'session_create' ? a.step : null);
    const waiting = lockFree(a)
      ? 0
      : repos.reduce((n, repo) => n + (daemon.repo_waiting?.[repo] ?? 0), 0);
    const queued = repos.reduce((n, repo) => n + (pending.get(repo) ?? 0), 0);
    const locks = lockFree(a) ? [] : repos.map(t.activity.tooltip);
    chips.push({
      id: `activity:${string(a.repo) ?? a.id}`,
      sev: 'blue',
      rank: 5,
      text: t.activity.counts(`${text} · ${t.elapsed(elapsed)}`, waiting, queued),
      tooltip:
        [phase ? t.activity.phase(phase) : null, ...locks].filter(Boolean).join(' ') || undefined,
      since: timestamp(a.started),
      target,
    });
  }
  // A plain background lock has no registry entry (e.g. reconcile gc).
  const working = new Set(work.flatMap(activityRepos));
  const warned = new Set(
    list(daemon.warnings).flatMap((w) => {
      const { kind, payload } = warningParts(w);
      return kind === 'LongHold' && typeof payload.repo === 'string' ? [payload.repo] : [];
    })
  );
  for (const raw of list(daemon.holds)) {
    const h = record(raw);
    const repo = string(h.repo);
    const elapsed = finite(h.elapsed_ms);
    if (
      h.origin !== 'background' ||
      !repo ||
      working.has(repo) ||
      warned.has(repo) ||
      covered.has(repo) ||
      elapsed === undefined ||
      elapsed <= 10_000
    )
      continue;
    covered.add(repo);
    chips.push({
      id: `activity:${repo}`,
      rank: 5,
      sev: 'blue',
      text: t.activity.counts(
        t.activity.background(repo),
        daemon.repo_waiting?.[repo] ?? 0,
        pending.get(repo) ?? 0
      ),
      tooltip: t.activity.tooltip(repo),
      target,
    });
  }
  return chips;
}

export function recentBusy(s: DaemonStatus, now: number): BusyEvent[] {
  if (!hasActivityStatus(s)) return [];
  return list<BusyEvent>(s.busy?.recent).filter((e) => {
    const event = record(e);
    const at = timestamp(event.at);
    // A reply newer than `now` (after the caller's last clock tick) is recent.
    return (
      at !== undefined &&
      age(at, now) <= LONG_WAIT_MAX_MS &&
      typeof event.scope === 'object' &&
      event.scope !== null
    );
  });
}

function busyChips(
  s: DaemonStatus,
  sessions: StatusReply | undefined,
  now: number,
  target: ChipTarget
): Chip[] {
  const recent = recentBusy(s, now);
  if (recent.length === 0) return [];
  const groups = new Map<string, BusyEvent[]>();
  for (const e of recent) {
    const key = JSON.stringify([e.scope.repo ?? null, e.scope.session ?? null]);
    groups.set(key, [...(groups.get(key) ?? []), e]);
  }
  // Evicted events have unknown scope. Do not attribute the ring's lower bound to one repo.
  if (s.busy?.truncated)
    return [
      {
        id: 'busy:truncated',
        rank: 4,
        sev: 'amber',
        text: t.busyTruncated,
        target,
      },
    ];
  return [...groups].map(([key, events]) => {
    const e = events[events.length - 1];
    const scope =
      [e.scope.repo, sessionLabel(e.scope.session, sessions)].filter(Boolean).join(' · ') ||
      undefined;
    const holderSession = sessionLabel(e.holder_session, sessions);
    const sameHolder = events.every(
      (event) => event.holder === e.holder && event.holder_session === e.holder_session
    );
    const holder = !sameHolder
      ? undefined
      : e.holder === 'catch_up'
        ? `auto-rebase${holderSession ? ` of ${holderSession}` : ''}`
        : e.holder || undefined;
    return {
      id: `busy:${key}`,
      rank: 4,
      sev: 'amber',
      text: t.busy(String(events.length), scope, holder),
      target,
    };
  });
}

/** Classify an externally tagged daemon warning, including the 0.2.2 method-based fallback. */
export function classifyWarning(
  w: unknown,
  now: number,
  daemon?: DaemonStatus,
  sessions?: StatusReply
): WarningView {
  return classify(w, now, daemon, sessions);
}

/**
 * When the App first saw the current P-b scan pass running, kept across replies by the
 * component (§11.4): `daemon_status.scan` has no start time, and `du.scan.started` may belong
 * to an earlier pass. Cleared by a reply without a running pass; never before the daemon started.
 */
export function observeScanRun(
  previous: number | undefined,
  s: DaemonStatus | undefined,
  updatedAt: number
): number | undefined {
  if (!hasActivityStatus(s) || s?.scan?.running !== true) return undefined;
  return Math.max(previous ?? updatedAt, timestamp(s.started) ?? 0);
}

/** The daemon instance the App last saw, and whether it replaced one the App saw before. */
export interface DaemonStart {
  started: string;
  pid: number | undefined;
  /** This instance's pid or start time differs from an earlier reply's (§11.7). */
  restarted: boolean;
}

/**
 * Kept across replies by the component, like {@link observeScanRun}: a reply without a start
 * time (0.2.2, or no reply) changes nothing. The first instance seen is not a restart, so a
 * daemon that started at login, just before the App, shows no restart chip.
 */
export function observeDaemonStart(
  previous: DaemonStart | undefined,
  s: DaemonStatus | undefined
): DaemonStart | undefined {
  const started = string(s?.started);
  if (started === undefined) return previous;
  const pid = finite(s?.pid);
  if (previous === undefined) return { started, pid, restarted: false };
  return previous.started === started && previous.pid === pid
    ? previous
    : { started, pid, restarted: true };
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
      return state(
        'down',
        t.daemon.notResponding,
        reason(g) === 'request_in_flight' ? t.daemon.waitingForReply : errorTitle(input.error)
      );
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

/**
 * Whether a failed daemon_status poll keeps its cached reply on show, by S1's own rule: the
 * daemon is not stopped, starting, shedding or waiting for its volume, the reply is at most
 * 60 s old, and (with `health`) S1 has not escalated to not responding. One timeout or refusal
 * then blanks nothing, while a daemon that stopped answering shows nothing old as current.
 */
export function retainsReply(
  daemon: StatusBarInput['daemon'],
  now: number,
  health?: StatusBarInput['health']
): boolean {
  return daemon.error != null && daemonState(daemon, Math.max(now, daemon.updatedAt), health).fresh;
}

/**
 * Build S1 from already-polled replies. Historical warnings never become fresh health evidence.
 * `clock` is the caller's time, which ticks every 5 s: a reply that arrived after the last tick
 * proves later time has passed, so its own timestamps (volume sample, Busy replies, daemon start)
 * are never ahead of the model's `now`.
 */
export function statusBarModel(
  input: StatusBarInput,
  clock: number,
  prevSpaceLevel: SpaceLevel = 'muted'
): StatusBarModel {
  const now = Math.max(clock, input.daemon.updatedAt);
  const health = daemonState(input.daemon, now, input.health);
  const daemon = input.daemon.data;
  const all: Chip[] = health.healthChip ? [health.healthChip] : [];
  const pb = hasActivityStatus(daemon);
  const volume = health.fresh ? volumeView(daemon, now, prevSpaceLevel) : null;
  const modal = (section: 'daemon' | 'warnings' | 'activity' | 'scan' | 'trash'): ChipTarget => ({
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
    // The modal hides cached warning and activity sections while the current query is in
    // error; the Status section is always there.
    const warningTarget = input.daemon.error == null ? modal('warnings') : modal('daemon');
    const activityTarget = input.daemon.error == null ? modal('activity') : modal('daemon');
    const fds = daemon.fds;
    // `limit` is null when the daemon could not read it: occupancy is then unknown.
    const fdLimit = finite(fds?.limit);
    if (
      fds &&
      ((fdLimit !== undefined && fdLimit > 0 && fds.open >= fdLimit * 0.9) ||
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
    const warnings = list(daemon.warnings).map((w) => classify(w, now, daemon, input.sessions));
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
    const deferrals = new Map<string, WarningView[]>();
    const fetchRepos = new Set<string | undefined>();
    const activityRank = (w: WarningView) =>
      w.kind !== 'LongHold' ? 2 : warningParts(w.raw).payload.method === 'catch_up' ? 0 : 1;
    for (const w of warnings) {
      if (w.cls === 'activity') {
        if (w.since !== undefined && age(w.since, now) < ACTIVITY_MIN_MS) continue;
        const key = w.repo ?? w.kind;
        if (w.kind === 'AutoRebaseStuck') deferrals.set(key, [...(deferrals.get(key) ?? []), w]);
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
    const reported = pb ? reportedActivityChips(input, now, pending, activityTarget) : [];
    all.push(...reported, ...busyChips(daemon, input.sessions, now, activityTarget));
    const reportedRepos = new Set(activityList(daemon).flatMap(activityRepos));
    for (const [repo, w] of activities) {
      if (pb && (reportedRepos.has(repo) || reported.some((c) => c.id === `activity:${repo}`)))
        continue;
      const lock = w.kind === 'LongHold';
      const deferred = deferrals.get(repo) ?? [];
      // A deferral chip names one session; the repo's other deferrals are counted on it and
      // named in its tooltip, so a second one is never hidden.
      const others = deferred.filter((d) => d !== w);
      const text = lock
        ? t.activity.deferred(
            t.activity.counts(w.text, daemon.repo_waiting?.[repo] ?? 0, pending.get(repo) ?? 0),
            deferred.length
          )
        : t.activity.moreDeferred(w.text, others.length);
      all.push({
        id: `activity:${repo}`,
        rank: 5,
        sev: 'blue',
        text,
        tooltip: lock
          ? t.activity.tooltip(repo)
          : others.length > 0
            ? t.activity.alsoDeferred(
                others.map((d) =>
                  t.activity.deferral(
                    sessionLabel(d.session, input.sessions),
                    string(warningParts(d.raw).payload.reason) ?? ''
                  )
                )
              )
            : undefined,
        since: w.since,
        target: activityTarget,
      });
    }
    const scan = pb ? daemon.scan : input.du?.scan;
    // §11.4: only a full pass or one running over 60 s. P-b times the pass from the App's own
    // first sighting (status has no start); 0.2.2 from du's start of the same pass.
    const scanStarted = pb ? input.scanSince : timestamp(input.du?.scan?.started);
    if (
      scan?.running &&
      (scan.full || (scanStarted !== undefined && age(scanStarted, now) > 60_000))
    ) {
      all.push({
        id: 'scan',
        rank: 6,
        sev: 'muted',
        text:
          pb && daemon.scan
            ? t.scanProgress(daemon.scan.done, daemon.scan.active, daemon.scan.queued)
            : t.scan(scan.done, scan.queued),
        target: modal('scan'),
        since: scanStarted,
      });
    }
    const unresolved = pb
      ? (daemon.scan?.unresolved_n ?? input.du?.scan?.unresolved?.length ?? 0)
      : 0;
    if (unresolved > 0)
      all.push({
        id: 'scan-unresolved',
        rank: 4,
        sev: 'amber',
        text: t.scanUnresolved(unresolved),
        target: modal('scan'),
      });
    if (volume && !volume.stale && volume.level !== 'muted')
      all.push({
        id: 'low-space',
        rank: volume.level === 'red' ? 1 : 4,
        sev: volume.level,
        text: t.lowSpace(bytes(volume.free)),
        target: modal('trash'),
      });
    // Only a restart the App saw (pid or start time replaced), never a first start at login.
    const started = timestamp(daemon.started);
    if (
      input.daemonRestarted === true &&
      started !== undefined &&
      age(started, now) < LONG_WAIT_MAX_MS
    ) {
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
  const savings = health.fresh && liveSavingsExact(du) ? du?.savings?.live : undefined;
  return {
    daemon: health.trigger,
    ...capChips(all),
    spaceLevel: volume?.level ?? prevSpaceLevel,
    right: {
      free: volume ? { text: volume.text, sev: volume.stale ? 'muted' : volume.level } : null,
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
