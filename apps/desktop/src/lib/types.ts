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
  warnings: unknown[];
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
  chains: Record<string, Chain>;
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
  state: { kind: string };
  from: string;
  to: string;
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

export interface DuReply {
  now: string;
  root: string;
  total: Usage;
  bases: { all: Usage; repos: Record<string, Usage> };
  deps: { all: Usage; repos: Record<string, Usage> };
}
