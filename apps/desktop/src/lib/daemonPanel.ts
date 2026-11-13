/**
 * The status-bar daemon modal and the Daemon screen: daemon facts, savings against plain
 * `git worktree` checkouts, storage, the size scan and lock activity. Pure; every number
 * comes from `daemon_status` or `du`.
 *
 * Space is measured: the daemon sizes each session with APFS private sizes, and
 * `savings` = logical − private is what full checkouts of the same files would have
 * written (daemon.md §5.12). Time is measured only on the git-strata side (`create_ms`,
 * newer daemons). The plain-worktree side is an estimate: logical bytes ÷ ASSUMED_COPY_RATE,
 * with no per-file cost and no dependency install, so it is a lower bound.
 *
 * Storage (status-bar design §2.3, §11.4, §11.7): a `private` sum is a lower bound ("at
 * least"), because blocks a base shares with its sessions count nowhere. `allocated` counts a
 * cloned block once for every file holding it, so over clones it reaches terabytes that were
 * never written; it stays out of user copy except inside a Details disclosure.
 *
 * Accuracy: a `du` figure with `exact: false` had files without an APFS private size, whose
 * allocated size stood in (strata-apfs usage.rs). Its `private` can then overstate what is held
 * alone and logical − private understate the savings, so neither bound holds: such figures are
 * null here and show as unavailable.
 */
import { daemonCopy as t } from './daemonCopy';
import type { Severity, WarningView } from './statusBar';
import type { DaemonStatus, DuReply, DuScan } from './types';

/** Optimistic sustained copy rate for the estimate (bytes per second, decimal GB). */
export const ASSUMED_COPY_RATE = 2_000_000_000;

export function copyEstimateMs(logical: number): number {
  return (Math.max(0, logical) / ASSUMED_COPY_RATE) * 1000;
}

export interface SavingsRow {
  session: string;
  logical: number;
  /** Held only by this session, at least (`private`); null when the row is inexact. */
  onDisk: number | null;
  /** logical − private, an upper bound; null when the row is inexact. */
  saved: number | null;
  files: number | null;
  createMs: number | null;
  copyEstimateMs: number;
  /** Estimate ÷ measured; null without a measured, non-zero create time. */
  speedup: number | null;
}

export interface SavingsView {
  live: {
    /** Upper bound; null when the live sessions' figures are inexact (`sessions.all.exact`). */
    saved: number | null;
    logical: number;
    /** Lower bound; null when inexact. */
    onDisk: number | null;
    files: number | null;
    sessions: number;
    complete: boolean;
    asOf: string | null;
    copyEstimateMs: number;
  };
  cumulative: {
    /**
     * Upper bound; null when this data root reports inexact figures. The reply carries no
     * accuracy for the cumulative sum (du.rs `cumulative`), so `total.exact` stands in.
     */
    saved: number | null;
    sessions: number;
    sized: number;
    unsized: number;
    removed: number;
    since: string | null;
  };
  /** Over sessions with a measured `new`; null when there are none. */
  timing: {
    timed: number;
    avgCreateMs: number;
    copyEstimateMs: number;
    speedup: number | null;
  } | null;
  /** Why `timing` is null. */
  timingMissing: 'old-daemon' | 'none-timed' | null;
  rows: SavingsRow[];
  /** Rows with no recorded create time (made before the daemon timed sessions). */
  untimed: number;
  /** Some bound above is null because its figures are inexact: say why. */
  inexact: boolean;
}

const ratio = (est: number, measured: number | null) =>
  measured !== null && measured > 0 ? est / measured : null;

/** A `du` figure with `exact: false` (absent on a daemon that does not say: no evidence). */
const inexact = (f: unknown): boolean =>
  f !== null && typeof f === 'object' && (f as { exact?: unknown }).exact === false;

/** null: this daemon does not report savings (before the du savings report). */
export function savingsView(du: DuReply | undefined): SavingsView | null {
  if (!du?.savings || !du.sessions) return null;
  const { live, cumulative: c } = du.savings;
  // `savings.live` sums the same units as `sessions.all`, which carries their accuracy.
  const liveExact = !inexact(du.sessions.all);
  const cumulativeExact = liveExact && !inexact(du.total);
  const rows: SavingsRow[] = du.sessions.list
    .map((s) => {
      const exact = !inexact(s);
      const createMs = typeof s.create_ms === 'number' ? s.create_ms : null;
      const est = copyEstimateMs(s.logical);
      return {
        session: s.session,
        logical: s.logical,
        onDisk: exact ? s.private : null,
        saved: exact ? s.savings : null,
        files: typeof s.files === 'number' ? s.files : null,
        createMs,
        copyEstimateMs: est,
        speedup: ratio(est, createMs),
      };
    })
    // Inexact rows (no saving to rank by) last, in reply order.
    .sort((a, b) => (b.saved ?? -1) - (a.saved ?? -1));
  const files = du.sessions.all.files;
  let timing: SavingsView['timing'] = null;
  let timingMissing: SavingsView['timingMissing'] = null;
  if (typeof c.timed !== 'number' || typeof c.create_ms !== 'number') {
    timingMissing = 'old-daemon';
  } else if (c.timed === 0) {
    timingMissing = 'none-timed';
  } else {
    const est = copyEstimateMs(c.timed_logical ?? 0);
    timing = {
      timed: c.timed,
      avgCreateMs: c.create_ms / c.timed,
      copyEstimateMs: est / c.timed,
      speedup: ratio(est, c.create_ms),
    };
  }
  return {
    live: {
      saved: liveExact ? live.savings : null,
      logical: live.logical,
      onDisk: liveExact ? live.private : null,
      files: typeof files === 'number' ? files : null,
      sessions: du.sessions.list.length,
      complete: live.complete,
      asOf: live.as_of,
      copyEstimateMs: copyEstimateMs(live.logical),
    },
    cumulative: {
      saved: cumulativeExact ? c.savings : null,
      sessions: c.sessions,
      sized: c.sized,
      unsized: c.unsized,
      removed: c.removed,
      since: c.since,
    },
    timing,
    timingMissing,
    rows,
    untimed: rows.filter((r) => r.createMs === null).length,
    inexact: !cumulativeExact || rows.some((r) => r.saved === null),
  };
}

/**
 * The `daemon_status` query answered last time (react-query keeps a reply across a failed
 * poll). Holds, queues and warnings are shown only from a live answer: a cached hold would
 * keep aging, and a LongHold turn amber, while the daemon is not answering.
 */
export function isLive(q: { isSuccess: boolean; isError: boolean; data: unknown }): boolean {
  return q.isSuccess || (q.data !== undefined && !q.isError);
}

export interface StatusFacts {
  version: string | null;
  api: number | null;
  pid: number | null;
  /** ms since `started`, from the reply's own clock reading. */
  uptimeMs: number | null;
  fds: { open: number; limit: number; acceptErrors: number; share: number } | null;
  reconcileRuns: number;
  holds: number;
  waiting: number;
  warnings: number;
}

export function statusFacts(s: DaemonStatus, now: number): StatusFacts {
  const started = s.started ? Date.parse(s.started) : NaN;
  const fds = s.fds;
  return {
    version: s.version ?? null,
    api: typeof s.api === 'number' ? s.api : null,
    pid: typeof s.pid === 'number' ? s.pid : null,
    uptimeMs: Number.isFinite(started) ? Math.max(0, now - started) : null,
    fds:
      fds && typeof fds.open === 'number' && typeof fds.limit === 'number'
        ? {
            open: fds.open,
            limit: fds.limit,
            acceptErrors: fds.accept_errors ?? 0,
            share: fds.limit > 0 ? fds.open / fds.limit : 0,
          }
        : null,
    reconcileRuns: s.reconcile?.runs ?? 0,
    holds: s.holds?.length ?? 0,
    waiting: Object.values(s.repo_waiting ?? {}).reduce((a, n) => a + n, 0),
    warnings: s.warnings?.length ?? 0,
  };
}

const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);

// ---- Storage (design §2.3 #21-#23, §11.4, Q5) ----

/**
 * The `du` fields the storage block reads beyond types.ts (du.rs `Agg::json` and `report`).
 * Local and optional on purpose: a daemon that lacks one shows less instead of breaking.
 */
interface FigJson {
  private?: number;
  allocated?: number;
  logical?: number;
  /** Every file reported a private size (APFS). */
  exact?: boolean;
  complete?: boolean;
  pending?: number;
  stale?: number;
  as_of?: string | null;
}

interface TrashJson extends FigJson {
  entries?: number;
  eligible?: FigJson & { entries?: number };
  young?: FigJson & { entries?: number };
  /** gc's age floor for an entry (store.md §5.11). */
  floor_secs?: number;
  /** The human command that empties eligible trash. */
  gc?: string;
}

interface StoreJson {
  db?: FigJson;
  blobs?: FigJson;
  tmp?: FigJson;
  other?: FigJson;
  trash?: TrashJson;
}

/**
 * `ok`: a lower bound over fresh measurements; `measuring`: some units are not sized yet or
 * are being re-sized; `unavailable`: some file reported no private size, so no bound holds.
 */
export type FigureState = 'ok' | 'measuring' | 'unavailable';

export interface Figure {
  /** Private bytes ("at least"); null when `unavailable`. */
  bytes: number | null;
  state: FigureState;
  /** The oldest measurement behind the figure. */
  asOf: string | null;
}

export type StorageGroup = 'sessions' | 'bases' | 'deps' | 'store' | 'trash';

export interface StorageView {
  /** `total.private`: what git-strata alone holds, at least. */
  headline: Figure;
  /** The same total by group; `store` leaves trash out, so the groups add up to the total. */
  groups: Array<{ key: StorageGroup; fig: Figure }>;
  /** Details disclosure only: per-file sums that count a cloned block again for every clone. */
  allocated: number | null;
  logical: number | null;
  /** What gc may empty; null from a daemon that does not report trash eligibility. */
  trash: { entries: number; eligible: number; young: number; floorSecs: number; gc: string } | null;
}

function figure(f: FigJson | undefined): Figure | null {
  const bytes = num(f?.private);
  if (!f || bytes === null) return null;
  const asOf = typeof f.as_of === 'string' ? f.as_of : null;
  if (f.exact === false) return { bytes: null, state: 'unavailable', asOf };
  const measuring = f.complete === false || (num(f.pending) ?? 0) > 0 || (num(f.stale) ?? 0) > 0;
  return { bytes, state: measuring ? 'measuring' : 'ok', asOf };
}

/** Units' figures summed, flags combined as du.rs `Agg::merge` does. */
function merge(parts: FigJson[]): FigJson | undefined {
  if (parts.length === 0) return undefined;
  const total = (k: 'private' | 'pending' | 'stale') =>
    parts.reduce((a, p) => a + (num(p[k]) ?? 0), 0);
  const asOf = parts.reduce<string | null>(
    (min, p) =>
      typeof p.as_of === 'string' && (min === null || Date.parse(p.as_of) < Date.parse(min))
        ? p.as_of
        : min,
    null
  );
  return {
    private: total('private'),
    pending: total('pending'),
    stale: total('stale'),
    exact: parts.every((p) => p.exact !== false),
    complete: parts.every((p) => p.complete !== false),
    as_of: asOf,
  };
}

function trashView(tr: TrashJson | undefined): StorageView['trash'] {
  const entries = num(tr?.entries);
  const eligible = num(tr?.eligible?.entries);
  const floorSecs = num(tr?.floor_secs);
  if (!tr || entries === null || eligible === null || floorSecs === null) return null;
  if (typeof tr.gc !== 'string' || tr.gc === '') return null;
  const young = num(tr.young?.entries) ?? Math.max(0, entries - eligible);
  return { entries, eligible, young, floorSecs, gc: tr.gc };
}

/** null: no `du` reply yet, or one without a total. */
export function storageView(du: DuReply | undefined): StorageView | null {
  const headline = figure(du?.total);
  if (!du || !headline) return null;
  const store = (du as DuReply & { store?: StoreJson }).store;
  const storeParts = [store?.db, store?.blobs, store?.tmp, store?.other].filter(
    (p): p is FigJson => p !== undefined && p !== null
  );
  const byGroup: Array<[StorageGroup, FigJson | undefined]> = [
    ['sessions', du.sessions?.all],
    ['bases', du.bases?.all],
    ['deps', du.deps?.all],
    ['store', merge(storeParts)],
    ['trash', store?.trash],
  ];
  return {
    headline,
    groups: byGroup.flatMap(([key, f]) => {
      const fig = figure(f);
      return fig ? [{ key, fig }] : [];
    }),
    allocated: num(du.total.allocated),
    logical: num(du.total.logical),
    trash: trashView(store?.trash),
  };
}

/** gc's age floor in words: "1 h", "30 min", "45 s". */
export function age(secs: number): string {
  if (secs >= 3600 && secs % 3600 === 0) return `${secs / 3600} h`;
  if (secs >= 60 && secs % 60 === 0) return `${secs / 60} min`;
  return `${secs} s`;
}

// ---- Size scan (design §3.3 fallback, §11.4) ----

export interface ScanView {
  running: boolean;
  /** Units measured in this pass; null from a daemon that does not say. */
  done: number | null;
  /** Units being measured right now (newer daemons, `scan.active`); null on 0.2.2. */
  active: number | null;
  queued: number;
  /** When the last full pass ended. */
  lastFull: string | null;
  /**
   * `failed` is attempt history (the last 20, repeats included, kept after a later success):
   * the same unit and error once, with how often, newest first.
   */
  failed: Array<{ unit: string; error: string; times: number }>;
}

export function scanView(scan: DuScan | undefined): ScanView | null {
  if (!scan || typeof scan.running !== 'boolean') return null;
  const history = Array.isArray(scan.failed) ? scan.failed : [];
  const failed = new Map<string, ScanView['failed'][number]>();
  for (const f of [...history].reverse()) {
    if (!f || typeof f.unit !== 'string' || typeof f.error !== 'string') continue;
    const key = JSON.stringify([f.unit, f.error]);
    const prev = failed.get(key);
    failed.set(key, { unit: f.unit, error: f.error, times: (prev?.times ?? 0) + 1 });
  }
  return {
    running: scan.running,
    done: num(scan.done),
    active: num((scan as DuScan & { active?: unknown }).active),
    queued: num(scan.queued) ?? 0,
    lastFull: typeof scan.last_full?.ended === 'string' ? scan.last_full.ended : null,
    failed: [...failed.values()],
  };
}

/** "Measuring storage · 412 done, 140 queued": counts only (units are uneven: no %, no ETA). */
export function scanText(v: ScanView, fmt: (iso: string) => string = when): string {
  if (!v.running) return t.scan.idle(v.lastFull ? fmt(v.lastFull) : null);
  const opt = (n: number | null) => (n === null ? null : count(n));
  return t.scan.running(opt(v.done), count(v.queued), opt(v.active));
}

// ---- Locks and queues (design §2.2 #10-#11, §11.3) ----

/**
 * Holders that are the daemon's own work, never a client request: the 0.2.2 fallback list
 * (design §11.3) plus `reconcile`, which holds only session queues.
 */
export const DAEMON_WORK: ReadonlySet<string> = new Set([
  'catch_up',
  'old_tree',
  'periodic_fetch',
  'daemon',
  'reconcile',
]);

export interface HoldRow {
  repo: string;
  method: string;
  /** The daemon's own work (auto-rebase, old-tree cleanup, fetch, ...), not a request. */
  background: boolean;
  /** Null for the daemon's own repo takes (catch-up moves on 0.2.2) and session-less requests. */
  session: string | null;
  elapsedMs: number | null;
}

export interface QueueRow {
  session: string;
  holder: string | null;
  background: boolean;
  /** Methods waiting in the session's queue. */
  queued: string[];
  elapsedMs: number | null;
}

export interface ActivityView {
  holds: HoldRow[];
  queues: QueueRow[];
  /** `repo_waiting`, busiest first. */
  waiting: Array<{ repo: string; n: number }>;
}

/** A Rust `SystemTime` as serde writes it, as epoch ms; null for anything else. */
export function systemTimeMs(v: unknown): number | null {
  if (!v || typeof v !== 'object') return null;
  const { secs_since_epoch: s, nanos_since_epoch: n } = v as Record<string, unknown>;
  return typeof s === 'number' && typeof n === 'number' ? s * 1000 + n / 1e6 : null;
}

function elapsedMs(since: unknown, now: number, reported?: unknown): number | null {
  const r = num(reported);
  if (r !== null) return Math.max(0, r);
  const ms = systemTimeMs(since);
  return ms === null ? null : Math.max(0, now - ms);
}

function holdRow(h: unknown, now: number): HoldRow | null {
  if (!h || typeof h !== 'object') return null;
  const o = h as Record<string, unknown>;
  if (typeof o.repo !== 'string' || typeof o.method !== 'string') return null;
  // A newer daemon says where the holder came from (`origin`); 0.2.2 is judged by method.
  const background =
    o.origin === 'background' || (o.origin !== 'request' && DAEMON_WORK.has(o.method));
  return {
    repo: o.repo,
    method: o.method,
    background,
    session: typeof o.session === 'string' ? o.session : null,
    elapsedMs: elapsedMs(o.since, now, o.elapsed_ms),
  };
}

function queueRow(q: unknown, now: number): QueueRow | null {
  if (!q || typeof q !== 'object') return null;
  const o = q as Record<string, unknown>;
  if (typeof o.session !== 'string') return null;
  const holder = typeof o.holder === 'string' ? o.holder : null;
  return {
    session: o.session,
    holder,
    background: holder !== null && DAEMON_WORK.has(holder),
    queued: Array.isArray(o.queued) ? o.queued.filter((m) => typeof m === 'string') : [],
    elapsedMs: holder === null ? null : elapsedMs(o.since, now),
  };
}

const longestFirst = (a: { elapsedMs: number | null }, b: { elapsedMs: number | null }) =>
  (b.elapsedMs ?? -1) - (a.elapsedMs ?? -1);

export function activityView(s: DaemonStatus, now: number): ActivityView {
  const list = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
  return {
    holds: list(s.holds)
      .map((h) => holdRow(h, now))
      .filter((r) => r !== null)
      .sort(longestFirst),
    queues: list(s.queues)
      .map((q) => queueRow(q, now))
      .filter((r) => r !== null)
      .sort(longestFirst),
    waiting: Object.entries(s.repo_waiting ?? {})
      .filter(([, n]) => typeof n === 'number' && n > 0)
      .map(([repo, n]) => ({ repo, n }))
      .sort((a, b) => b.n - a.n || a.repo.localeCompare(b.repo)),
  };
}

// ---- Warnings (design §1 S3, §3.1 last bullet) ----

/**
 * The colour a classified warning is drawn in (design §2, Q3): red only for the error class,
 * blue for daemon activity, never red otherwise. A LongHold is never red.
 */
export function warningSev(v: Pick<WarningView, 'kind' | 'cls' | 'sev'>): Severity {
  if (v.cls === 'error' && v.kind !== 'LongHold') return 'red';
  if (v.cls === 'activity') return 'blue';
  return v.sev === 'red' ? 'amber' : v.sev;
}

/** A warning's raw entry, for the Details disclosure only. */
export function rawJson(raw: unknown): string {
  try {
    return JSON.stringify(raw, null, 2) ?? String(raw);
  } catch {
    return String(raw);
  }
}

// ---- Opening the modal at a chip's section (design §3.5) ----

/** One child of the modal's scroll box as laid out: its `data-section` and offset from the top. */
export interface SectionBox {
  name: string | undefined;
  offsetTop: number;
}

/**
 * Where the modal body scrolls for a chip's `section`, given its children and how far it is
 * scrolled now. `top`: scroll there (null: leave the view as it is). `done`: stop trying.
 * A section that is not there (yet) leaves the view at the top, where the Now list shows every
 * chip, and is tried again on the next render; once the view has landed, or the reader has
 * scrolled, nothing moves it, so a section that comes and goes with later polls never pulls the
 * view back.
 */
export function sectionLanding(
  children: readonly SectionBox[],
  section: string | undefined,
  scrollTop: number
): { top: number | null; done: boolean } {
  if (section === undefined || scrollTop !== 0) return { top: null, done: true };
  const hit = children.find((c) => c.name === section);
  return hit ? { top: hit.offsetTop, done: true } : { top: null, done: false };
}

// ---- Format ----

/** "Nov 13, 09:44" in the viewer's locale and zone; "—" without a valid time. */
export function when(iso: string | null | undefined): string {
  const d = iso ? new Date(iso) : null;
  return d && Number.isFinite(d.getTime())
    ? d.toLocaleString(undefined, {
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      })
    : '—';
}

/** "0.4 s", "12 s", "4.2 min", "1.5 h". */
export function duration(ms: number): string {
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)} s`;
  if (s < 60) return `${Math.round(s)} s`;
  const m = s / 60;
  if (m < 60) return `${m < 10 ? m.toFixed(1) : Math.round(m)} min`;
  return `${(m / 60).toFixed(1)} h`;
}

/** "3.2×", "41×", "1,200×". */
export function times(x: number): string {
  return x < 10 ? `${x.toFixed(1)}×` : `${Math.round(x).toLocaleString('en-US')}×`;
}

export function count(n: number): string {
  return n.toLocaleString('en-US');
}
