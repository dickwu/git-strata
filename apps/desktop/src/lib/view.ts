/** Daemon replies → what the screens show. Pure; tested against the P1 fixtures. */
import { copy } from './copy';
import { classifyWarning, type WarningView } from './statusBar';
import type {
  Checkpoint,
  DaemonStatus,
  DepsList,
  StatusReply,
  SessionEntry,
  UpstreamRepo,
} from './types';

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
  mark: 'active' | 'done';
  note: string | null;
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
  behind?: number;
  behindCapped?: boolean;
}

export interface RepoView {
  upstream?: UpstreamRepo;
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
  /**
   * `warnings[]` classified for display (statusBar.ts `classifyWarning`, shared with the status
   * bar): one line of text and a severity each; the raw entry is kept for a Details disclosure.
   */
  warnings: WarningView[];
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
  marks?: string[];
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
  const marks = f.marks ?? [];
  return rows.filter((r) => {
    const used = new Set(r.repoList.filter((x) => x.used).map((x) => x.name));
    if (!f.repos.every((x) => used.has(x))) return false;
    if (agents.length > 0 && !(r.agent && agents.includes(r.agent))) return false;
    if (marks.length > 0 && !marks.includes(r.mark)) return false;
    if (words.length === 0) return true;
    const hay = [
      r.ticket ?? '',
      r.id,
      r.agent ?? '',
      r.note ?? '',
      ...r.repoList.map((x) => x.name),
    ]
      .join(' ')
      .toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/** Stable options keep a selected state visible even when its count reaches zero. */
export function sessionMarks(rows: SessionRow[]) {
  return [
    { name: 'active', label: 'Active', sessions: rows.filter((r) => r.mark === 'active').length },
    { name: 'done', label: 'Done', sessions: rows.filter((r) => r.mark === 'done').length },
  ];
}

/** One repo's proposal or workspace that needs attention. */
export interface WaitingItem {
  key: string;
  kind: 'deps' | 'conflict' | 'deferred' | 'relax';
  number: number;
  repo: string;
  session: string | null;
  since: string | null;
  summary?: string;
  summarySince?: 'session_base';
  affected?: Array<{ id: string; ticket: string | null }>;
  mode?: 'apply' | 'confirm';
  reason?: string;
  command?: string;
  progress?: string;
}
export interface WaitingGroup {
  repo: string;
  items: WaitingItem[];
  applying: WaitingItem[];
}
export const STUCK_AFTER = 24 * 60 * 60 * 1000;
export function hasUpstream(daemon: DaemonStatus | undefined): boolean {
  return daemon?.features?.includes('status.upstream') === true;
}
/** Workspace-level and session-level upstream are the same view, at different RPC depths. */
export function repoUpstream(s: SessionEntry, repo: string): UpstreamRepo | undefined {
  if (s.status.manifest.repos[repo]?.tracked === false) return undefined;
  return s.status.workspaces?.[repo]?.upstream ?? s.status.upstream?.repos?.[repo];
}
export function syncCommand(session: string, repo: string): string {
  // Real ids/names are validated by strata-types. Quoting also keeps imported fixtures safe.
  const quote = (v: string) => (/^[a-zA-Z0-9_.-]+$/.test(v) ? v : `'${v.replace(/'/g, "'\\''")}'`);
  return `cd ~ && git-strata sync --session ${quote(session)} --repo ${quote(repo)}`;
}
function pendingIncludes(s: SessionEntry, repo: string, id: number): boolean {
  return (s.status.workspaces?.[repo]?.pending ?? []).some(
    (p) => p === id || (typeof p === 'object' && p !== null && 'id' in p && p.id === id)
  );
}
export function longDeferral(auto: UpstreamRepo['auto'], now: number): boolean {
  const since = Date.parse(auto?.since ?? '');
  return (
    auto?.state === 'deferred' &&
    auto.reason !== 'conflicts' &&
    Number.isFinite(since) &&
    now - since > STUCK_AFTER
  );
}
/** A missing session-level enrichment is unknown; per-workspace omission is legitimate. */
export function unavailableUpstream(
  sessions: StatusReply | undefined,
  daemon?: DaemonStatus
): string[] {
  return hasUpstream(daemon)
    ? (sessions?.sessions ?? [])
        .filter((s) => !s.status.upstream)
        .map((s) => s.status.manifest.id)
        .sort()
    : [];
}
function knownUpstream(s: SessionEntry, repo: string, enabled: boolean): UpstreamRepo | undefined {
  return enabled && s.status.upstream ? repoUpstream(s, repo) : undefined;
}
function stuckReason(
  s: SessionEntry,
  repo: string,
  now: number,
  enabled: boolean
): string | undefined {
  const w = s.status.workspaces?.[repo];
  const u = knownUpstream(s, repo, enabled);
  const state = u?.deps?.state;
  if (state === 'install_failed') return 'install failed';
  if (state === 'own_change') return 'session has its own dependency changes';
  if (state === 'drift') return 'dependency drift';
  if (w && w.health !== 'Ok') return 'needs the human: workspace unhealthy';
  if (w?.scene || s.status.scene) return 'needs the human: scene';
  if (u?.following === false) return 'needs the human: not following main';
  if (u?.diverged) return 'needs the human: diverged';
  if (state === 'waiting_human') return 'needs the human';
  if (longDeferral(u?.auto, now)) return 'newer changes not applied for over a day';
  return undefined;
}
/** Group by repo; apply-mode progress stays out of the human waiting count (O6). */
export function waitingGroups(
  sessions: StatusReply | undefined,
  deps: DepsList | undefined,
  daemon?: DaemonStatus,
  now = Date.now()
): WaitingGroup[] {
  const upstream = hasUpstream(daemon);
  const ss = sessions?.sessions ?? [];
  const groups = new Map<string, WaitingGroup>();
  const covered = new Set<string>();
  const group = (repo: string) => {
    if (!groups.has(repo)) groups.set(repo, { repo, items: [], applying: [] });
    return groups.get(repo)!;
  };
  for (const p of [...(deps?.proposals ?? [])].sort((a, b) => a.id - b.id)) {
    if (p.state?.kind !== 'Pending') continue;
    const affected = ss.filter((s) => pendingIncludes(s, p.repo, p.id));
    const states = upstream ? affected.map((s) => knownUpstream(s, p.repo, upstream)) : [];
    // The proposal has the effective repo policy even when status lags or omits upstream.
    // Older daemons fall back to affected workspaces, then conservatively to confirm.
    const mode =
      p.mode ??
      (states.some((u) => u?.deps?.proposal === p.id && u.deps.mode === 'apply') &&
      !states.some((u) => u?.deps?.proposal === p.id && u.deps.mode === 'confirm')
        ? 'apply'
        : 'confirm');
    const reason =
      mode === 'apply'
        ? affected.map((s) => stuckReason(s, p.repo, now, upstream)).find(Boolean)
        : undefined;
    const moved = ss.filter(
      (s) =>
        s.status.manifest.repos[p.repo]?.tracked &&
        s.status.manifest.repos[p.repo]?.base.deps === p.to
    ).length;
    const deferred = states.filter((u) => u?.auto?.state === 'deferred');
    const reasons = [
      ...new Set(deferred.map((u) => u?.auto?.reason?.replace(/_/g, ' ')).filter(Boolean)),
    ];
    const it: WaitingItem = {
      key: `deps:${p.id}`,
      kind: 'deps',
      number: p.id,
      repo: p.repo,
      session: p.origin?.kind === 'Session' ? (p.origin.session ?? null) : null,
      since: p.created ?? null,
      summary: p.changes?.summary,
      summarySince: p.changes?.since,
      affected: affected.map((s) => ({
        id: s.status.manifest.id,
        ticket: s.status.manifest.ticket ?? null,
      })),
      mode,
      reason,
      progress: `${moved}/${moved + affected.length} moved${deferred.length ? `, ${deferred.length} deferred${reasons.length ? `: ${reasons.join(', ')}` : ''}` : ''}`,
    };
    (mode === 'apply' && !reason ? group(p.repo).applying : group(p.repo).items).push(it);
    if (mode === 'apply' && reason)
      for (const s of affected) covered.add(`${s.status.manifest.id}:${p.repo}`);
  }
  for (const s of ss) {
    for (const [repo, w] of Object.entries(s.status.workspaces ?? {})) {
      const u = knownUpstream(s, repo, upstream);
      const n = Math.max(w.conflicts?.length ?? 0, u?.conflicts?.length ?? 0);
      if (n === 0 && u?.auto?.reason !== 'conflicts') continue;
      group(repo).items.push({
        key: `conflict:${s.status.manifest.id}:${repo}`,
        kind: 'conflict',
        number: n,
        repo,
        session: s.status.manifest.id,
        since: null,
        affected: [{ id: s.status.manifest.id, ticket: s.status.manifest.ticket ?? null }],
        reason: "needs the session's agent",
        command: upstream ? syncCommand(s.status.manifest.id, repo) : undefined,
      });
    }
  }
  const deferred = new Map<string, Array<{ session: SessionEntry; since: string }>>();
  for (const session of ss)
    for (const repo of Object.keys(session.status.workspaces ?? {})) {
      const auto = knownUpstream(session, repo, upstream)?.auto;
      if (!longDeferral(auto, now) || covered.has(`${session.status.manifest.id}:${repo}`))
        continue;
      const rows = deferred.get(repo) ?? [];
      rows.push({ session, since: auto!.since });
      deferred.set(repo, rows);
    }
  for (const [repo, rows] of deferred) {
    rows.sort(
      (a, b) =>
        Date.parse(a.since) - Date.parse(b.since) ||
        a.session.status.manifest.id.localeCompare(b.session.status.manifest.id)
    );
    group(repo).items.push({
      key: `deferred:${repo}`,
      kind: 'deferred',
      repo,
      number: rows.length,
      session: rows[0].session.status.manifest.id,
      since: rows[0].since,
      affected: rows.map(({ session }) => ({
        id: session.status.manifest.id,
        ticket: session.status.manifest.ticket ?? null,
      })),
      reason: copy.tray.deferredReason,
    });
  }
  const relax = new Set<string>();
  for (const w of Array.isArray(daemon?.warnings) ? daemon.warnings : []) {
    if (w && typeof w === 'object' && Object.keys(w).length === 1 && 'ConfigRelaxPending' in w) {
      const payload = w.ConfigRelaxPending;
      if (
        payload &&
        typeof payload === 'object' &&
        'repo' in payload &&
        typeof payload.repo === 'string'
      )
        relax.add(payload.repo);
    }
  }
  for (const repo of [...relax].sort())
    group(repo).items.push({
      key: `relax:${repo}`,
      kind: 'relax',
      repo,
      number: 0,
      session: null,
      since: null,
      reason: copy.tray.relax,
    });
  return [...groups.values()].sort((a, b) => a.repo.localeCompare(b.repo));
}
/** Flat compatibility view for review targets and existing callers. */
export function waitingItems(
  sessions: StatusReply | undefined,
  deps: DepsList | undefined,
  daemon?: DaemonStatus,
  now = Date.now()
): WaitingItem[] {
  return waitingGroups(sessions, deps, daemon, now)
    .flatMap((g) => g.items)
    .sort((a, b) => {
      const rank = { deps: 0, deferred: 1, relax: 2, conflict: 3 };
      return (
        rank[a.kind] - rank[b.kind] ||
        (a.kind === 'deps' ? a.number - b.number : a.key.localeCompare(b.key))
      );
    });
}
export function waitingCount(
  sessions: StatusReply | undefined,
  deps: DepsList | undefined,
  daemon?: DaemonStatus,
  now = Date.now()
): number {
  return waitingGroups(sessions, deps, daemon, now).reduce(
    (n, g) => n + g.items.filter((i) => i.kind !== 'conflict').length,
    0
  );
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

function row(s: SessionEntry, daemon?: DaemonStatus): SessionRow {
  const ups = hasUpstream(daemon)
    ? Object.keys(s.status.manifest.repos)
        .map((repo) => repoUpstream(s, repo))
        .filter((u) => u && !u.unchecked && u.behind != null)
    : [];
  const ws = Object.values(s.status.workspaces ?? {});
  const bad = ws.map((w) => w.health).find((h) => h !== 'Ok');
  return {
    behind: ups.length ? ups.reduce((n, u) => n + (u?.behind ?? 0), 0) : undefined,
    behindCapped: ups.some((u) => u?.capped),
    id: s.status.manifest.id,
    ticket: s.status.manifest.ticket ?? null,
    state: s.status.state,
    mark: s.status.mark?.state === 'done' ? 'done' : 'active',
    note: s.status.mark?.note ?? null,
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

export function sessionRows(reply: StatusReply | undefined, daemon?: DaemonStatus): SessionRow[] {
  return (reply?.sessions ?? [])
    .map((s) => row(s, daemon))
    .sort((a, b) => b.created.localeCompare(a.created));
}

function reasonText(c: Checkpoint): string {
  return c.reason.message ? `${c.reason.kind}: ${c.reason.message}` : c.reason.kind;
}

export function sessionDetail(
  reply: StatusReply | undefined,
  id: string,
  daemon?: DaemonStatus
): SessionDetailView | null {
  const s = reply?.sessions.find((x) => x.status.manifest.id === id);
  if (!s) return null;
  const repos = Object.entries(s.status.manifest.repos).map(([repo, m]): RepoView => {
    const w = s.status.workspaces[repo];
    const c = reply?.chains?.[repo];
    return {
      repo,
      upstream: hasUpstream(daemon) ? repoUpstream(s, repo) : undefined,
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
  return { row: row(s, daemon), root: s.status.manifest.root, repos, checkpoints };
}

export function daemonView(s: DaemonStatus, now = Date.now()): DaemonView {
  return {
    legacy: !('api' in s),
    version: typeof s.version === 'string' ? s.version : null,
    reconcileRuns: s.reconcile?.runs ?? 0,
    holds: s.holds?.length ?? 0,
    waiting: Object.entries(s.repo_waiting ?? {})
      .filter(([, n]) => n > 0)
      .map(([repo, n]) => ({ repo, n }))
      .sort((a, b) => b.n - a.n || a.repo.localeCompare(b.repo)),
    warnings: (Array.isArray(s.warnings) ? s.warnings : []).map((w) => classifyWarning(w, now)),
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
  deps: DepsList | undefined,
  daemon?: DaemonStatus
): { session: string | null; screen?: 'daemon' } {
  const [, kind, arg] = id.split(':');
  if (kind === 'relax') return { session: null, screen: 'daemon' };
  if (kind === 'deferred') {
    const item = waitingGroups(sessions, deps, daemon)
      .find((g) => g.repo === arg)
      ?.items.find((i) => i.kind === 'deferred');
    return { session: item?.session ?? null };
  }
  if (kind === 'deps') {
    const p = deps?.proposals.find((x) => String(x.id) === arg);
    return {
      session:
        p?.origin.kind === 'Session'
          ? (p.origin.session ?? null)
          : p
            ? (sessions?.sessions.find((s) => pendingIncludes(s, p.repo, p.id))?.status.manifest
                .id ?? null)
            : null,
    };
  }
  if (kind === 'conflict') {
    const s = sessions?.sessions.find(
      (x) =>
        (x.status.workspaces[arg]?.conflicts?.length ?? 0) > 0 ||
        (hasUpstream(daemon) &&
          ((knownUpstream(x, arg, true)?.conflicts?.length ?? 0) > 0 ||
            knownUpstream(x, arg, true)?.auto?.reason === 'conflicts'))
    );
    return { session: s?.status.manifest.id ?? null };
  }
  return { session: null };
}
