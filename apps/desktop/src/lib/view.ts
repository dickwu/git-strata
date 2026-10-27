/** Daemon replies → what the screens show. Pure; tested against the P1 fixtures. */
import type { Checkpoint, DaemonStatus, DepsList, StatusReply, SessionEntry } from './types';

/** One repo in a session. Tracked repos checkpoint and publish; the rest are reference workspaces. */
export interface RepoChip {
  name: string;
  tracked: boolean;
  /**
   * The session uses this repo: it is tracked, or its workspace has pending changes,
   * conflicts or a checkpoint. Only used repos are filterable (peilin t73u).
   */
  used: boolean;
}

export interface SessionRow {
  id: string;
  ticket: string | null;
  state: string;
  phase: string;
  repos: number;
  /** Used first, then the rest; alphabetical within each. */
  repoList: RepoChip[];
  /**
   * The AI agent that created the session (Claude, Codex, …). The daemon does not record
   * one yet (see the t72u proposal); read defensively so the GUI lights up once it does.
   */
  agent: string | null;
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

function repoList(s: SessionEntry): RepoChip[] {
  const checkpointed = new Set((s.checkpoints ?? []).map((c) => c.repo));
  return Object.entries(s.status.manifest.repos ?? {})
    .map(([name, m]) => {
      const tracked = m.tracked !== false;
      const w = s.status.workspaces?.[name];
      const used =
        tracked ||
        (w?.pending?.length ?? 0) > 0 ||
        (w?.conflicts?.length ?? 0) > 0 ||
        w?.checkpoint != null ||
        checkpointed.has(name);
      return { name, tracked, used };
    })
    .sort((a, b) => Number(b.used) - Number(a.used) || a.name.localeCompare(b.name));
}

function agentOf(s: SessionEntry): string | null {
  const raw =
    (s.status as { agent?: unknown }).agent ?? (s.status.manifest as { agent?: unknown }).agent;
  return typeof raw === 'string' && raw.trim() ? raw.trim() : null;
}

/** Repos some current session uses, with how many sessions use each; busiest first. */
export function usedRepos(rows: SessionRow[]): Array<{ name: string; sessions: number }> {
  const n = new Map<string, number>();
  for (const r of rows)
    for (const x of r.repoList) if (x.used) n.set(x.name, (n.get(x.name) ?? 0) + 1);
  return [...n]
    .map(([name, sessions]) => ({ name, sessions }))
    .sort((a, b) => b.sessions - a.sessions || a.name.localeCompare(b.name));
}

/** Agents seen on current sessions, with counts. Empty until the daemon records agents. */
export function knownAgents(rows: SessionRow[]): Array<{ name: string; sessions: number }> {
  const n = new Map<string, number>();
  for (const r of rows) if (r.agent) n.set(r.agent, (n.get(r.agent) ?? 0) + 1);
  return [...n]
    .map(([name, sessions]) => ({ name, sessions }))
    .sort((a, b) => b.sessions - a.sessions || a.name.localeCompare(b.name));
}

export interface RowFilter {
  /** Selected repos: a session must use all of them. */
  repos: string[];
  /** Selected agents: a session must come from one of them (a session has one agent). */
  agents?: string[];
  text?: string;
}

/**
 * The Sessions list filter. A row passes when it uses every selected repo, comes from one
 * of the selected agents (if any), and every word of `text` appears in its ticket, id,
 * agent or one of its repo names.
 */
export function filterRows(rows: SessionRow[], f: RowFilter): SessionRow[] {
  const words = (f.text ?? '').trim().toLowerCase().split(/\s+/).filter(Boolean);
  const agents = f.agents ?? [];
  return rows.filter((r) => {
    const used = new Set(r.repoList.filter((x) => x.used).map((x) => x.name));
    if (!f.repos.every((x) => used.has(x))) return false;
    if (agents.length > 0 && !(r.agent && agents.includes(r.agent))) return false;
    if (words.length === 0) return true;
    const hay = [r.ticket ?? '', r.id, r.agent ?? '', ...r.repoList.map((x) => x.name)]
      .join(' ')
      .toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/**
 * How many things wait for a human: pending deps proposals plus workspaces with
 * conflicted paths. Same rule as the tray's "Waiting for you" (src-tauri/src/tray_model.rs
 * `waiting`), so the Sessions header note and the tray agree.
 */
export function waitingCount(
  sessions: StatusReply | undefined,
  deps: DepsList | undefined
): number {
  const proposals = (deps?.proposals ?? []).filter((p) => p.state?.kind === 'Pending').length;
  const conflicts = (sessions?.sessions ?? []).reduce(
    (n, s) =>
      n +
      Object.values(s.status.workspaces ?? {}).filter((w) => (w.conflicts?.length ?? 0) > 0).length,
    0
  );
  return proposals + conflicts;
}

/**
 * A session's age for the Time column. Running sessions count from `created` to `now`.
 * The status reply carries no end time, so a Removed session has no final duration (null)
 * rather than a clock that keeps running.
 */
export function sessionAge(created: string, state: string, now: number): number | null {
  if (state === 'Removed') return null;
  const t = Date.parse(created);
  return Number.isNaN(t) ? null : Math.max(0, now - t);
}

function row(s: SessionEntry): SessionRow {
  const ws = Object.values(s.status.workspaces ?? {});
  const bad = ws.map((w) => w.health).find((h) => h !== 'Ok');
  return {
    id: s.status.manifest.id,
    ticket: s.status.manifest.ticket ?? null,
    state: s.status.state,
    phase: s.status.phase,
    repos: Object.keys(s.status.manifest.repos ?? {}).length,
    repoList: repoList(s),
    agent: agentOf(s),
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
