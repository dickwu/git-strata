/** Daemon replies → what the screens show. Pure; tested against the P1 fixtures. */
import type { Checkpoint, DaemonStatus, DepsList, StatusReply, SessionEntry } from './types';

export interface SessionRow {
  id: string;
  ticket: string | null;
  state: string;
  phase: string;
  repos: number;
  pending: number;
  conflicts: number;
  /** Worst workspace health: anything other than "Ok" wins. */
  health: string;
  created: string;
}

export interface RepoView {
  repo: string;
  head: string;
  baseTree: string;
  depsSet: string;
  path: string;
  health: string;
  pending: number;
  conflicts: number;
  warnings: number;
  checkpoint: number | null;
  chain: { branch: string; state: string; origin: string } | null;
}

export interface SessionDetailView {
  row: SessionRow;
  root: string;
  repos: RepoView[];
  checkpoints: Array<{ id: number; repo: string; at: Date; reason: string; entries: number }>;
}

export interface DaemonView {
  /** No `api` key: a daemon from before P2. The poll plan reads it gently (§8.1 P1 notes). */
  legacy: boolean;
  version: string | null;
  reconcileRuns: number;
  holds: number;
  waiting: Array<{ repo: string; n: number }>;
  warnings: string[];
}

export const short = (oid: string | null | undefined, n = 7): string =>
  oid ? oid.slice(0, n) : '—';

function row(s: SessionEntry): SessionRow {
  const ws = Object.values(s.status.workspaces ?? {});
  const bad = ws.map((w) => w.health).find((h) => h !== 'Ok');
  return {
    id: s.status.manifest.id,
    ticket: s.status.manifest.ticket ?? null,
    state: s.status.state,
    phase: s.status.phase,
    repos: Object.keys(s.status.manifest.repos ?? {}).length,
    pending: ws.reduce((n, w) => n + (w.pending?.length ?? 0), 0),
    conflicts: ws.reduce((n, w) => n + (w.conflicts?.length ?? 0), 0),
    health: bad ?? 'Ok',
    created: s.status.created,
  };
}

export function sessionRows(reply: StatusReply | undefined): SessionRow[] {
  return (reply?.sessions ?? []).map(row).sort((a, b) => b.created.localeCompare(a.created));
}

function reasonText(c: Checkpoint): string {
  return c.reason.message ? `${c.reason.kind}: ${c.reason.message}` : c.reason.kind;
}

export function sessionDetail(
  reply: StatusReply | undefined,
  id: string
): SessionDetailView | null {
  const s = reply?.sessions.find((x) => x.status.manifest.id === id);
  if (!s) return null;
  const repos = Object.entries(s.status.manifest.repos).map(([repo, m]): RepoView => {
    const w = s.status.workspaces[repo];
    const c = reply?.chains?.[repo];
    return {
      repo,
      head: m.head,
      baseTree: m.base.tree,
      depsSet: m.base.deps,
      path: m.path,
      health: w?.health ?? 'Unknown',
      pending: w?.pending?.length ?? 0,
      conflicts: w?.conflicts?.length ?? 0,
      warnings: w?.warnings?.length ?? 0,
      checkpoint: w?.checkpoint ?? null,
      chain: c ? { branch: c.branch, state: c.state, origin: c.origin } : null,
    };
  });
  repos.sort((a, b) => a.repo.localeCompare(b.repo));
  const checkpoints = [...s.checkpoints]
    .sort((a, b) => b.id - a.id)
    .map((c) => ({
      id: c.id,
      repo: c.repo,
      at: new Date(c.at * 1000),
      reason: reasonText(c),
      entries: c.entries,
    }));
  return { row: row(s), root: s.status.manifest.root, repos, checkpoints };
}

function warningText(w: unknown): string {
  if (typeof w === 'string') return w;
  if (w && typeof w === 'object') {
    const o = w as Record<string, unknown>;
    const kind = typeof o.kind === 'string' ? o.kind : 'Warning';
    const msg = typeof o.message === 'string' ? o.message : null;
    return msg ? `${kind}: ${msg}` : kind;
  }
  return String(w);
}

export function daemonView(s: DaemonStatus): DaemonView {
  return {
    legacy: !('api' in s),
    version: typeof s.version === 'string' ? s.version : null,
    reconcileRuns: s.reconcile?.runs ?? 0,
    holds: s.holds?.length ?? 0,
    waiting: Object.entries(s.repo_waiting ?? {})
      .filter(([, n]) => n > 0)
      .map(([repo, n]) => ({ repo, n }))
      .sort((a, b) => b.n - a.n || a.repo.localeCompare(b.repo)),
    warnings: (s.warnings ?? []).map(warningText),
  };
}

/**
 * Where a tray "Waiting for you" item (src-tauri/src/tray_model.rs ids) should land in P1:
 * `waiting:deps:<id>` → the session that proposed it; `waiting:conflict:<repo>` → the first
 * session with conflicts in that repo. Null → the sessions list.
 */
export function openTarget(
  id: string,
  sessions: StatusReply | undefined,
  deps: DepsList | undefined
): { session: string | null } {
  const [, kind, arg] = id.split(':');
  if (kind === 'deps') {
    const p = deps?.proposals.find((x) => String(x.id) === arg);
    return { session: p?.origin.kind === 'Session' ? (p.origin.session ?? null) : null };
  }
  if (kind === 'conflict') {
    const s = sessions?.sessions.find(
      (x) => (x.status.workspaces[arg]?.conflicts?.length ?? 0) > 0
    );
    return { session: s?.status.manifest.id ?? null };
  }
  return { session: null };
}
