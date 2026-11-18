/**
 * The shapes P1 reads, as the daemon sends them today (fixtures in
 * src-tauri/tests/fixtures/p1/). Only the fields the screens use are typed; everything
 * else stays `unknown` so a daemon that adds fields does not break the build.
 */

import type { Channel } from '@tauri-apps/api/core';

export interface GuiError {
  code: string;
  message: string;
  detail: unknown;
}

/** A Rust `SystemTime` as serde writes it (lock holds, queues, warnings). */
export interface SystemTimeJson {
  secs_since_epoch: number;
  nanos_since_epoch: number;
}

/** `holds[]` (daemon `LockHold`). `session` is null for `try_*` repo takes (catch-up moves on 0.2.2). */
export interface LockHoldJson {
  repo: string;
  method: string;
  session: string | null;
  since: SystemTimeJson;
  origin?: 'request' | 'background';
  elapsed_ms?: number;
}

export interface LongHoldJson {
  repo: string;
  method: string;
  since: SystemTimeJson;
  origin?: 'request' | 'background' | null;
  session?: string | null;
}

/** P-b display work, distinct from the admitted requests used by a future drain. */
interface ActivityBase {
  id: number;
  origin: 'request' | 'background';
  repo: string | null;
  session: string | null;
  started: string;
  elapsed_ms: number;
  step_elapsed_ms: number;
}

export type DaemonActivity = ActivityBase &
  (
    | {
        kind: 'move';
        cause: 'catch_up' | 'publish' | 'sync' | 'deps' | 'replay';
        phase: 'checkpoint' | 'prepare' | 'recheck' | 'swap' | 'pin' | 'record' | 'old_tree' | null;
      }
    | { kind: 'deps_rollout'; sessions: number; done: number }
    | { kind: 'base_build'; tree: string | null }
    | { kind: 'session_create'; repos: string[]; step: string | null }
    | { kind: 'deps_install'; proposal: string | null }
    | { kind: 'publish' | 'push' }
  );

export interface BusyEvent {
  at: string;
  method: string;
  session: string | null;
  reason: 'lock_wait' | 'du_cancel_timeout';
  scope: { repo?: string; session?: string };
  holder: string | null;
  holder_session: string | null;
}

export interface DaemonBusy {
  total: number;
  recent: BusyEvent[];
  truncated: boolean;
}

export interface VolumeSpace {
  free: number;
  size: number;
  as_of: string;
  /** Feature `daemon_status.volume_name`: the data root measured (`paths.root`). */
  path?: string;
  /** Feature `daemon_status.volume_name`: where the measured volume is mounted. */
  mount?: string;
  /** Feature `daemon_status.volume_name`: the measured volume's name (`GWD`). */
  name?: string;
}

export interface DaemonScan {
  running: boolean;
  active: number;
  queued: number;
  done: number;
  full: boolean;
  unresolved_n: number;
}

/** `queues[]` (locks.rs `queue_snapshot`): a session FIFO and who holds it. */
export interface QueueJson {
  session: string;
  holder: string | null;
  since: SystemTimeJson | null;
  /** Methods waiting in this session's FIFO. */
  queued: string[];
}

/**
 * One `warnings[]` entry: serde's external tagging of `DaemonWarning`, so a unit variant is a
 * bare string (`"StorePoisoned"`) and the rest are `{ Variant: { ...fields } }`.
 */
export type DaemonWarningJson = string | Record<string, Record<string, unknown> | null>;

export type DrainThen = 'restart' | 'stop' | 'hold';

export type DrainRequest =
  | { on: boolean; ttl_s?: number; lease_id?: string; seal?: never; then?: never }
  | { seal: true; lease_id: string; then: DrainThen; on?: never; ttl_s?: never };

export interface DrainLease {
  lease_id: string;
}

export interface Draining extends DrainLease {
  /** RFC 3339. A sealed hold has no expiry. */
  expires_at: string | null;
  sealed: boolean;
  then: DrainThen | null;
}

export interface Inflight {
  method: string;
  session?: string;
  repo?: string;
  phase: 'queued' | 'confirming' | 'locked' | 'running';
  /** RFC 3339. */
  started_at: string;
}

export interface DaemonStatus {
  holds: unknown[];
  /** Session FIFOs. Present on every daemon the app talks to, typed late (status bar §2.2 #11). */
  queues?: QueueJson[];
  reconcile: { runs: number; last: unknown };
  repo_waiting: Record<string, number>;
  verify: unknown;
  warnings: unknown[];
  /** Added in P2 (gui.md §3.4). Absent on every daemon P1 can talk to. */
  api?: number;
  version?: string;
  features?: string[];
  config?: {
    generation: number;
    hash: string;
    loaded_at: unknown;
    last_attempt: unknown;
    restart_required: string[] | null;
  };
  /**
   * Descriptor use (daemons from ffe316b on). Optional: older daemons omit it. `limit` is null
   * when the daemon could not read its soft limit (startup.rs `fd_limit`).
   */
  fds?: { open: number; limit: number | null; accept_errors: number };
  /** P-b identity and display fields. Capability: `daemon_status.activity`; no new `api`. */
  pid?: number;
  started?: string;
  activity?: DaemonActivity[];
  busy?: DaemonBusy;
  volume?: VolumeSpace | null;
  /** Feature `daemon.drain`. Older daemons omit these fields. */
  draining?: Draining | null;
  inflight?: Inflight[];
  store_schema?: number;
  scan?: DaemonScan;
}

export interface BaseRef {
  repo: string;
  tree: string;
  deps: string;
}

export interface RepoManifest {
  base: BaseRef;
  head: string;
  path: string;
  tracked: boolean;
}

export interface Workspace {
  checkpoint: number | null;
  conflicts: unknown[];
  generation: number;
  health: string;
  pending: unknown[];
  seeds: unknown[];
  /** Counted only in P1; see `OldTreeKeptWarning` for the one with a tri-state field. */
  warnings: unknown[];
  scene?: unknown;
  upstream?: UpstreamRepo;
}

/** main-sync §5. Cold list-cache entries have null behind and unchecked=true. */
export interface UpstreamCommit {
  oid: string;
  subject: string;
  author: string;
  own: boolean;
}
export interface UpstreamRepo {
  behind: number | null;
  unchecked?: boolean;
  capped?: boolean;
  following?: boolean;
  diverged?: boolean;
  unpublished?: boolean;
  main?: { head: string; tree: string } | null;
  base_head?: string;
  commits?: UpstreamCommit[];
  /** Commit IDs in T..W.head (≤ 20); the daemon sends IDs only. */
  base_only?: string[];
  deps?: {
    proposal: number;
    mode: 'apply' | 'confirm';
    state: string;
    from: string;
    to: string;
    summary?: string;
    /** `install_failed` only: the worker's error; `detail.staging` is kept for inspection. */
    error?: { message?: string; detail?: { staging?: string; [key: string]: unknown } } | null;
  } | null;
  auto?: {
    source: string;
    state: 'waiting' | 'deferred' | 'pending';
    reason?: string | null;
    since: string;
    attempts: number;
    target?: { head: string; tree: string };
    detail?: { paths?: string[]; [key: string]: unknown };
  } | null;
  conflicts?: string[];
  fix?: string;
}
export interface Upstream {
  as_of?: string | null;
  clean: boolean;
  repos: Record<string, UpstreamRepo>;
}
export interface PackageChanges {
  summary: string;
  packages: Array<{
    name: string;
    from: string | null;
    to: string | null;
    kind: 'added' | 'removed' | 'changed';
    direct: boolean;
  }>;
  truncated: boolean;
  since?: 'session_base';
}

/**
 * `status` workspace warning `{kind: "OldTreeKept"}`. Since status-perf A1 `changed` is
 * nullable: `null` (with `unchecked: true`) means not judged yet, the old-tree check runs
 * in the background. Read `null` as unknown, never as `false`. P1 does not read it yet.
 */
export interface OldTreeKeptWarning {
  kind: 'OldTreeKept';
  path: string;
  n: number;
  holders: unknown[];
  changed: boolean | null;
  retained: boolean;
  unchecked?: boolean;
  checked_at?: string;
}

export interface Checkpoint {
  at: number;
  base: BaseRef;
  entries: number;
  id: number;
  reason: { kind: string; message?: string };
  repo: string;
}

export interface SessionEntry {
  checkpoints: Checkpoint[];
  in_flight: unknown;
  queued: number;
  status: {
    /** Absent on active sessions and daemons predating session.mark. */
    mark?: {
      state: 'active' | 'done';
      note?: string;
      by: { kind: 'agent' | 'cli' | 'gui' | 'daemon' };
      at: string;
    };
    created: string;
    manifest: {
      id: string;
      repos: Record<string, RepoManifest>;
      root: string;
      ticket?: string | null;
    };
    phase: string;
    state: string;
    workspaces: Record<string, Workspace>;
    scene?: unknown;
    upstream?: Upstream;
  };
}

export interface Chain {
  branch: string;
  origin: string;
  state: string;
}

export interface StatusReply {
  /** Only when asked (`chains: true`; the GUI asks on `status{session}`). */
  chains?: Record<string, Chain>;
  holds: unknown[];
  leftovers: unknown[];
  sessions: SessionEntry[];
  warnings: unknown[];
}

/**
 * Additive `closed` / `auto_close` fields on a publish or push reply (p1-autoclose §6).
 * The GUI does not drive publish yet; typed so a later screen can read them without
 * widening every `unknown`. `closed` is omitted on push when nothing was judged.
 */
export interface AutoCloseMarkResult {
  written: boolean;
  at?: string;
  skipped?: string;
  error?: unknown;
}

export interface AutoCloseSession {
  closed: boolean;
  in_group: boolean;
  reasons: string[];
  detail: unknown;
  rm?: unknown | null;
  mark?: AutoCloseMarkResult;
  /** Present when another request already removed a group session (`by: "other"`). */
  by?: string;
}

export interface AutoCloseReport {
  closed: boolean;
  reasons: string[];
  rejudged: string[];
  sessions: Record<string, AutoCloseSession>;
}

export interface AutoCloseReplyFields {
  closed?: boolean;
  auto_close?: AutoCloseReport;
}

export interface Proposal {
  id: number;
  repo: string;
  created: string;
  origin: { kind: string; session?: string };
  /** Pending, Applied, AwaitingPublish or `{kind: 'Superseded', by}`. */
  state: { kind: string; by?: number };
  from: string;
  to: string;
  /** Hashes of the install inputs; differing means the lockfile or manifests changed (deps.md). */
  from_inputs?: string;
  to_inputs?: string;
  /** Main's tree and commit the proposal was made from. */
  tree?: string | null;
  head?: string | null;
  from_head?: string | null;
  /** Effective dependency sync policy; older daemons omit this field. */
  mode?: 'apply' | 'confirm';
  changes?: PackageChanges;
}

/** A session's move report (strata-session MoveReport); only what the modal reads is typed. */
export interface MoveReport {
  conflicts: string[];
  dropped: string[];
  lww: string[];
  uncarried: string[];
  old_tree?: string;
  [k: string]: unknown;
}

/** One `deps_approve` result row (handlers.rs apply_step). */
export type ApproveRow =
  | { session: string; kind: 'moved'; report: MoveReport }
  | {
      session: string;
      kind: 'skipped';
      /** Present on failed moves and on OpenFiles / Drift / Error skips. */
      code?: string;
      /** A SkipReason (`{kind, ...}`), or a failed move's error detail (no `kind`). */
      detail: unknown;
    };

export interface ApproveReply {
  results: ApproveRow[];
  proposal: Proposal;
}

export interface DepsList {
  proposals: Proposal[];
}

export interface Usage {
  allocated: number;
  logical: number;
  private: number;
  shared: number;
  as_of: string | null;
  complete: boolean;
  pending: number;
  units: number;
}

/** One live session in `du` (daemon.md §5.12). */
export interface DuSession extends Usage {
  session: string;
  files?: number;
  /** logical − private: what a plain copy would have added. */
  savings: number;
  /** Wall time of the `new` that made it (ms). Absent on older daemons, null when not measured. */
  create_ms?: number | null;
}

/** `savings.cumulative`: every session recorded since `since`. */
export interface DuCumulative {
  since: string | null;
  sessions: number;
  sized: number;
  unsized: number;
  removed: number;
  logical: number;
  private: number;
  savings: number;
  /** Newer daemons: sessions with a measured `new`, their total time and size. */
  timed?: number;
  create_ms?: number;
  timed_logical?: number;
  timed_files?: number;
}

export interface DuReply {
  now: string;
  root: string;
  total: Usage;
  bases: { all: Usage; repos: Record<string, Usage> };
  deps: { all: Usage; repos: Record<string, Usage> };
  /** Optional: daemons before the du savings report (2026-10-27) send neither. */
  sessions?: { all: Usage & { files?: number }; list: DuSession[] };
  savings?: {
    live: {
      logical: number;
      private: number;
      savings: number;
      as_of: string | null;
      complete: boolean;
    };
    cumulative: DuCumulative;
  };
  scan?: DuScan;
}

/** `du.scan` (du.rs `scan_json`). `failed` is attempt history (last 20, duplicates, not cleared). */
export interface DuScan {
  running: boolean;
  queued: number;
  last_full: { started: string; ended: string } | null;
  started?: string | null;
  done?: number;
  full?: boolean;
  active?: number;
  unresolved?: Array<{ unit: string; error: string; at: string }>;
  failed?: Array<{ unit: string; error: string }>;
}

/** legacy_agent (src-tauri/src/service/mod.rs LegacyAgent). */
export type AgentState = 'loaded' | 'installed' | 'not_installed' | 'foreign';
export interface LegacyAgent {
  state: AgentState;
  label: string;
  plist: string;
  command: string;
  /** state 'foreign' (cli.md D38): the foreign_home / foreign_agent error. */
  refusal?: GuiError;
}

/** daemon_start progress, one per change (service::Progress, tagged by `phase`). */
export type StartProgress =
  | { phase: 'launching'; action: 'kickstart' | 'bootstrap' }
  | { phase: 'waiting' }
  | { phase: 'waiting_store'; tcc: boolean }
  | { phase: 'recovering' }
  | { phase: 'ready' };

export interface Started {
  action: 'kickstart' | 'bootstrap';
  waited_ms: number;
}

/** Manifest information shown before download. */
export interface UpdateTarget {
  version: string;
  notes?: string;
  notes_url?: string;
  /** Advisory until the downloaded bundle has been verified. */
  store_schema?: number;
  min_updatable_from?: string;
  manual_download_url?: string;
}

export type InstallEligibility = { version: string } & (
  | { kind: 'eligible' }
  | { kind: 'move' }
  | { kind: 'setup' }
  | { kind: 'refused'; message: string; manual_download_url?: string }
);

interface UpdatePresentation {
  transaction_id?: string;
  start_requested_ms?: number;
  permission_wait_elapsed_ms?: number | null;
  operation_error?: GuiError;
  install_eligibility?: InstallEligibility;
  latest_offer?: UpdateTarget;
  schema_consent?: { request_id: string; from_schema: number | null; to_schema: number; version: string };
  version_details?: {
    program?: string;
    cli?: { path: string; label: 'this app' | `Homebrew formula ${string}` | 'other' }[];
    move_mode?: 'formula' | 'program_missing' | 'resume';
    can_setup?: boolean;
    can_link_cli?: boolean;
  };
  recovery?: { start?: boolean; rollback_version?: string | null };
}

/** Version-upgrade design §4.3. */
export type UpdateState = UpdatePresentation & (
  | { phase: 'idle' }
  | { phase: 'checking' }
  | { phase: 'up_to_date' }
  | { phase: 'available'; target: UpdateTarget }
  | { phase: 'check_failed' }
  | { phase: 'confirming' }
  | { phase: 'downloading'; pct: number | null }
  | { phase: 'verifying_bundle' }
  | { phase: 'draining'; inflight: Inflight[]; background: LockHoldJson[] }
  | { phase: 'stopping' }
  | { phase: 'stop_detail'; slow: boolean; close_error?: unknown }
  | { phase: 'installing' }
  | { phase: 'starting_daemon' }
  | { phase: 'relaunching' }
  | { phase: 'reconciling' }
  | { phase: 'waiting_daemon'; since: string }
  | { phase: 'done' }
  | { phase: 'daemon_failed'; reason: string }
  | { phase: 'waiting_permission' }
  | { phase: 'cancelled' }
  | { phase: 'not_starting' }
  | { phase: 'install_failed_unchanged' }
  | { phase: 'restored'; from: string }
  | { phase: 'restore_failed' }
  | { phase: 'cancel_blocked' }
  | { phase: 'bootout_failed' }
  | { phase: 'fence_uncertain' }
);

/** Persisted update transaction (§4.2 step 4). */
export interface Txn {
  phase: string;
  from: string;
  to: string;
  from_schema: number;
  to_schema: number;
  to_floor: string;
  /** Required key. Null when the daemon was not serving at the start of the update. */
  from_daemon: { pid: number; started: string; version: string } | null;
  program: string;
  rollback_zip: string;
  was_enabled?: boolean;
}

interface UpdateProgressArgs {
  onProgress: Channel<UpdateState>;
}

interface UpdateSchemaArgs extends UpdateProgressArgs {
  /** Target store schema acknowledged in the app's migration warning. */
  schemaAcknowledgement?: number;
}

export interface UpdateCommandArgs {
  update_state: undefined;
  update_check: undefined;
  update_install: UpdateSchemaArgs & { target: string };
  update_cancel: undefined;
  daemon_restart: UpdateSchemaArgs;
  daemon_move_into_app: UpdateSchemaArgs;
  /**
   * Omit folder choices to keep the existing config. This path requires
   * schemaAcknowledgement for the target schema (§6.3 step 3).
   */
  daemon_setup: UpdateSchemaArgs & { setupReviewId?: string };
  cli_link_install: undefined;
  update_rollback: UpdateProgressArgs & { startOnly?: boolean };
}

export interface UpdateCommandResults {
  update_state: UpdateState;
  update_check: UpdateState;
  update_install: void;
  update_cancel: void;
  daemon_restart: void;
  daemon_move_into_app: void;
  daemon_setup: void;
  cli_link_install: void;
  update_rollback: void;
}
