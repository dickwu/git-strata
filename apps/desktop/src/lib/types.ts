/**
 * The shapes P1 reads, as the daemon sends them today (fixtures in
 * src-tauri/tests/fixtures/p1/). Only the fields the screens use are typed; everything
 * else stays `unknown` so a daemon that adds fields does not break the build.
 */

export interface GuiError {
  code: string;
  message: string;
  detail: unknown;
}

export interface DaemonStatus {
  holds: unknown[];
  reconcile: { runs: number; last: unknown };
  repo_waiting: Record<string, number>;
  verify: unknown;
  warnings: unknown[];
  /** Added in P2 (gui.md §3.4). Absent on every daemon P1 can talk to. */
  api?: number;
  version?: string;
  /** Descriptor use (daemons from ffe316b on). Optional: older daemons omit it. */
  fds?: { open: number; limit: number; accept_errors: number };
  /** Not sent by any daemon yet; typed so the status modal picks them up when they are. */
  pid?: number;
  started?: string;
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
  scan?: { running: boolean; queued: number; last_full: { started: string; ended: string } | null };
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
