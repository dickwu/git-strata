import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { DaemonStatus, DepsList, Proposal, StatusReply, Workspace } from './types';
import {
  daemonView,
  filterRows,
  openTarget,
  knownAgents,
  usedRepos,
  waitingCount,
  waitingItems,
  waitingGroups,
  STUCK_AFTER,
  sessionDetail,
  sessionRows,
  short,
} from './view';
import { bytes, ago } from './copy';

const fx = <T>(name: string): T =>
  JSON.parse(
    readFileSync(join(import.meta.dir, '../../src-tauri/tests/fixtures/p1', `${name}.json`), 'utf8')
  ) as T;

const all = fx<StatusReply>('status_all');
const one = fx<StatusReply>('status_session');
const deps = fx<DepsList>('deps_list');
const daemon = fx<DaemonStatus>('daemon_status');

describe('sessionRows', () => {
  test('one row per session, newest first', () => {
    const rows = sessionRows(all);
    expect(rows.map((r) => r.id)).toEqual(['S2', 'S1']);
    expect(rows[1]).toMatchObject({
      ticket: 'T-101',
      state: 'Active',
      repos: 2,
      pending: 0,
      conflicts: 0,
      health: 'Ok',
    });
  });
  test('no reply → no rows', () => {
    expect(sessionRows(undefined)).toEqual([]);
  });
  test('worst health and conflict counts surface', () => {
    const copy: StatusReply = structuredClone(all);
    const s1 = copy.sessions.find((s) => s.status.manifest.id === 'S1')!;
    s1.status.workspaces.web.health = 'Degraded';
    s1.status.workspaces.web.conflicts = [{ path: 'a' }, { path: 'b' }];
    const r = sessionRows(copy).find((x) => x.id === 'S1')!;
    expect(r.health).toBe('Degraded');
    expect(r.conflicts).toBe(2);
  });
});

describe('sessionDetail', () => {
  test('repos sorted, short hashes available, checkpoints newest first', () => {
    const d = sessionDetail(one, one.sessions[0].status.manifest.id)!;
    expect(d).not.toBeNull();
    expect(d.repos.map((r) => r.repo)).toEqual(['tool', 'web']);
    expect(d.repos[0].chain).toMatchObject({ branch: 'main', state: 'Absent' });
    expect(short(d.repos[0].head)).toHaveLength(7);
    const ids = d.checkpoints.map((c) => c.id);
    expect(ids).toEqual([...ids].sort((a, b) => b - a));
    for (const c of d.checkpoints) expect(c.at.getUTCFullYear()).toBe(2026);
  });
  test('S1 from status_all has two checkpoints', () => {
    const d = sessionDetail(all, 'S1')!;
    expect(d.checkpoints.map((c) => c.id)).toEqual([2, 1]);
    expect(d.repos.find((r) => r.repo === 'web')!.checkpoint).toBe(2);
    // status{all} has no `chains` key (a status-perf daemon computes them only on request)
    expect(all.chains).toBeUndefined();
    for (const r of d.repos) expect(r.chain).toBeNull();
  });
  test('unknown id → null', () => {
    expect(sessionDetail(all, 'S99')).toBeNull();
  });
});

describe('daemonView', () => {
  test('fixture daemon has no api key → legacy, nothing waiting', () => {
    const v = daemonView(daemon);
    expect(v.legacy).toBe(true);
    expect(v.waiting).toEqual([]);
    expect(v.reconcileRuns).toBe(0);
    expect(v.holds).toBe(0);
  });
  test('api key present → not legacy; waiting sorted by count', () => {
    const v = daemonView({
      ...daemon,
      api: 1,
      repo_waiting: { a: 1, b: 3, c: 0 },
      warnings: [{ kind: 'Drift', message: 'x' }, 'plain'],
    });
    expect(v.legacy).toBe(false);
    expect(v.waiting).toEqual([
      { repo: 'b', n: 3 },
      { repo: 'a', n: 1 },
    ]);
    expect(v.warnings).toEqual(['Drift: x', 'plain']);
  });
});

describe('openTarget (tray strata://open)', () => {
  test('deps proposal → the session that made it', () => {
    expect(openTarget('waiting:deps:1', all, deps)).toEqual({ session: 'S2' });
  });
  test('unknown proposal → list', () => {
    expect(openTarget('waiting:deps:9', all, deps)).toEqual({ session: null });
  });
  test('conflict → first session with conflicts in that repo', () => {
    const c: StatusReply = structuredClone(all);
    c.sessions.find((s) => s.status.manifest.id === 'S2')!.status.workspaces.tool.conflicts = [{}];
    expect(openTarget('waiting:conflict:tool', c, deps)).toEqual({ session: 'S2' });
    expect(openTarget('waiting:conflict:web', c, deps)).toEqual({ session: null });
  });
});

describe('formatting', () => {
  test('bytes', () => {
    expect(bytes(0)).toBe('0 B');
    expect(bytes(512)).toBe('512 B');
    expect(bytes(937984)).toBe('916 KB');
    expect(bytes(1536)).toBe('1.5 KB');
  });
  test('ago', () => {
    expect(ago(3000)).toBe('just now');
    expect(ago(42000)).toBe('42 s ago');
    expect(ago(125000)).toBe('2 min ago');
  });
});

/** S1 with `n` repos: the fixture's tracked `tool` cloned; every third one is reference. */
function bigSession(n: number): StatusReply {
  const r: StatusReply = structuredClone(all);
  const s1 = r.sessions.find((s) => s.status.manifest.id === 'S1')!;
  const tmpl = s1.status.manifest.repos.tool;
  const ws = s1.status.workspaces.tool;
  s1.status.manifest.repos = {};
  s1.status.workspaces = {};
  for (let i = 0; i < n; i++) {
    const name = `repo-${String(i).padStart(2, '0')}`;
    s1.status.manifest.repos[name] = { ...structuredClone(tmpl), tracked: i % 3 !== 2 };
    s1.status.workspaces[name] = {
      ...structuredClone(ws),
      checkpoint: null,
      pending: [],
      conflicts: [],
    };
  }
  s1.checkpoints = [];
  return r;
}

describe('session repos (Sessions list tags)', () => {
  test('fixture repos map with tracked and used flags, used first', () => {
    const rows = sessionRows(all);
    expect(rows.find((r) => r.id === 'S1')!.repoList).toEqual([
      { name: 'tool', tracked: true, used: true },
      { name: 'web', tracked: true, used: true },
    ]);
    // S2: web is a reference workspace (tracked: false) with no changes → unused, last.
    expect(rows.find((r) => r.id === 'S2')!.repoList).toEqual([
      { name: 'tool', tracked: true, used: true },
      { name: 'web', tracked: false, used: false },
    ]);
  });
  test('an untracked repo becomes used once it has changes, conflicts or a checkpoint', () => {
    for (const touch of [
      (w: Workspace) => (w.pending = [{ path: 'a' }]),
      (w: Workspace) => (w.conflicts = [{ path: 'a' }]),
      (w: Workspace) => (w.checkpoint = 1),
    ]) {
      const r: StatusReply = structuredClone(all);
      const s2 = r.sessions.find((s) => s.status.manifest.id === 'S2')!;
      touch(s2.status.workspaces.web);
      const web = sessionRows(r)
        .find((x) => x.id === 'S2')!
        .repoList.find((x) => x.name === 'web')!;
      expect(web).toEqual({ name: 'web', tracked: false, used: true });
    }
  });
  test('23 repos: every one listed (rows wrap, nothing hidden), used before unused', () => {
    const r = sessionRows(bigSession(23)).find((x) => x.id === 'S1')!;
    expect(r.repos).toBe(23);
    expect(r.repoList).toHaveLength(23);
    const firstUnused = r.repoList.findIndex((x) => !x.used);
    expect(r.repoList.slice(0, firstUnused).every((x) => x.used)).toBe(true);
    expect(r.repoList.slice(firstUnused).every((x) => !x.used)).toBe(true);
    expect(r.repoList.filter((x) => x.used)).toHaveLength(16);
  });
  test('no agent recorded today → null', () => {
    expect(sessionRows(all).map((r) => r.agent)).toEqual([null, null]);
  });
});

describe('usedRepos / knownAgents (the tag bar)', () => {
  test('only used repos, with session counts, busiest first', () => {
    // web is used by S1 only (unused reference in S2).
    expect(usedRepos(sessionRows(all))).toEqual([
      { name: 'tool', sessions: 2 },
      { name: 'web', sessions: 1 },
    ]);
  });
  test('a repo drops out when no session uses it', () => {
    const r: StatusReply = structuredClone(all);
    r.sessions = r.sessions.filter((s) => s.status.manifest.id === 'S2');
    expect(usedRepos(sessionRows(r)).map((x) => x.name)).toEqual(['tool']);
  });
  test('agents are read from status or manifest once the daemon records them', () => {
    const r: StatusReply = structuredClone(all);
    (r.sessions[0].status as Record<string, unknown>).agent = 'claude-code';
    (r.sessions[1].status.manifest as Record<string, unknown>).agent = 'codex';
    const rows = sessionRows(r);
    expect(
      knownAgents(rows)
        .map((x) => x.name)
        .sort()
    ).toEqual(['claude-code', 'codex']);
    expect(knownAgents(sessionRows(all))).toEqual([]);
  });
});

describe('filterRows (repo AND, agent OR, text)', () => {
  const rows = sessionRows(all);
  const ids = (r: ReturnType<typeof sessionRows>) => r.map((x) => x.id);
  test('no filter keeps every row', () => {
    expect(ids(filterRows(rows, { repos: [] }))).toEqual(['S2', 'S1']);
  });
  test('repos combine as AND and match only used repos', () => {
    expect(ids(filterRows(rows, { repos: ['tool'] }))).toEqual(['S2', 'S1']);
    // S2 contains web but does not use it.
    expect(ids(filterRows(rows, { repos: ['web'] }))).toEqual(['S1']);
    expect(ids(filterRows(rows, { repos: ['web', 'tool'] }))).toEqual(['S1']);
    expect(ids(filterRows(rows, { repos: ['web', 'nope'] }))).toEqual([]);
  });
  test('repo selection matches exact names, not substrings', () => {
    expect(ids(filterRows(rows, { repos: ['we'] }))).toEqual([]);
  });
  test('text matches ticket, id or any repo name, every word', () => {
    expect(ids(filterRows(rows, { repos: [], text: 't-101' }))).toEqual(['S1']);
    expect(ids(filterRows(rows, { repos: [], text: 's2' }))).toEqual(['S2']);
    expect(ids(filterRows(rows, { repos: [], text: 'web' }))).toEqual(['S2', 'S1']);
    expect(ids(filterRows(rows, { repos: [], text: 'web t-102' }))).toEqual(['S2']);
  });
  test('agents: any of the selected; sessions without an agent drop out', () => {
    const r: StatusReply = structuredClone(all);
    (
      r.sessions.find((s) => s.status.manifest.id === 'S1')!.status as Record<string, unknown>
    ).agent = 'codex';
    const rr = sessionRows(r);
    expect(ids(filterRows(rr, { repos: [], agents: ['codex'] }))).toEqual(['S1']);
    expect(ids(filterRows(rr, { repos: [], agents: ['codex', 'claude-code'] }))).toEqual(['S1']);
    expect(ids(filterRows(rr, { repos: [], text: 'codex' }))).toEqual(['S1']);
  });
  test('23-repo session found by a repo deep in the list', () => {
    const big = sessionRows(bigSession(23));
    expect(ids(filterRows(big, { repos: ['repo-22'] }))).toEqual(['S1']);
  });
});

describe('waitingCount (Sessions header note, same rule as the tray)', () => {
  test('fixtures: nothing pending, no conflicts', () => {
    expect(waitingCount(all, deps)).toBe(0);
    expect(waitingCount(undefined, undefined)).toBe(0);
  });
  test('human waiting count excludes agent conflicts', () => {
    const d: DepsList = structuredClone(deps);
    d.proposals[0].state = { kind: 'Pending' };
    const s: StatusReply = structuredClone(all);
    s.sessions[0].status.workspaces.web.conflicts = [{ path: 'a' }, { path: 'b' }];
    expect(waitingCount(s, d)).toBe(1);
  });
  test('items: pending proposals by id, then conflicted workspaces, with where they belong', () => {
    const d: DepsList = structuredClone(deps);
    const p = d.proposals[0];
    d.proposals = [
      { ...p, id: 27, repo: 'app', state: { kind: 'Pending' }, origin: { kind: 'Main' } },
      {
        ...p,
        id: 25,
        repo: 'app',
        state: { kind: 'AwaitingPublish' },
        origin: { kind: 'Session', session: 's-a' },
      },
      {
        ...p,
        id: 22,
        repo: 'admin',
        state: { kind: 'Pending' },
        origin: { kind: 'Session', session: 's-b' },
      },
    ];
    const s: StatusReply = structuredClone(all);
    s.sessions[0].status.workspaces.web.conflicts = [{ path: 'a' }, { path: 'b' }];
    const items = waitingItems(s, d);
    expect(items.map((i) => i.key)).toEqual([
      'deps:22',
      'deps:27',
      `conflict:${s.sessions[0].status.manifest.id}:web`,
    ]);
    expect(items[0].session).toBe('s-b');
    expect(items[1].session).toBeNull();
    expect(items[1].since).toBe(p.created);
    expect(items[2]).toMatchObject({ kind: 'conflict', number: 2, repo: 'web', since: null });
  });
  test('the list follows each refresh: an approved proposal drops out', () => {
    const d: DepsList = structuredClone(deps);
    d.proposals[0].state = { kind: 'Pending' };
    expect(waitingItems(undefined, d)).toHaveLength(1);
    d.proposals[0].state = { kind: 'Applied' };
    expect(waitingItems(undefined, d)).toHaveLength(0);
  });
});

describe('main-sync waiting groups', () => {
  const now = Date.parse('2026-11-02T12:00:00Z');
  const capable = { ...daemon, features: ['status.upstream'] };
  function pending() {
    const s = structuredClone(all);
    const p: Proposal = {
      ...deps.proposals[0],
      id: 75,
      repo: 'web',
      state: { kind: 'Pending' },
      origin: { kind: 'Main' },
    };
    for (const entry of s.sessions) {
      entry.status.upstream = { clean: true, repos: {} };
      entry.status.manifest.repos.web.tracked = true;
      entry.status.workspaces.web.health = 'Ok';
      entry.status.workspaces.web.pending = [p];
      entry.status.workspaces.web.upstream = {
        behind: 3,
        deps: { proposal: 75, mode: 'apply', state: 'installing', from: p.from, to: p.to },
        auto: {
          source: 'main',
          state: 'deferred',
          reason: 'open_files',
          since: new Date(now - 3600000).toISOString(),
          attempts: 2,
        },
      };
    }
    return { s, d: { proposals: [p] } };
  }
  test('grouping and affected sessions follow pending, not proposal origin', () => {
    const { s, d } = pending();
    s.sessions[1].status.workspaces.web.pending = [];
    d.proposals.push({ ...d.proposals[0], id: 76, repo: 'tool' });
    const groups = waitingGroups(s, d);
    expect(groups.map((g) => g.repo)).toEqual(['tool', 'web']);
    expect(groups[1].items[0].affected?.map((x) => x.id)).toEqual([
      s.sessions[0].status.manifest.id,
    ]);
    expect(groups[1].items[0].affected?.[0].ticket).toBe(s.sessions[0].status.manifest.ticket);
  });
  test('apply-repo collapsing and feature gate', () => {
    const { s, d } = pending();
    expect(waitingGroups(s, d, capable, now)[0].items).toHaveLength(0);
    expect(waitingGroups(s, d, capable, now)[0].applying).toHaveLength(1);
    expect(waitingCount(s, d, capable, now)).toBe(0);
    expect(waitingGroups(s, d, daemon, now)[0].items).toHaveLength(1);
  });
  test('proposal mode works before the sessions and capability polls arrive', () => {
    const { d } = pending();
    d.proposals[0].mode = 'apply';
    for (const observedDaemon of [undefined, daemon, capable]) {
      expect(waitingCount(undefined, d, observedDaemon, now)).toBe(0);
      expect(waitingGroups(undefined, d, observedDaemon, now)[0].applying).toHaveLength(1);
    }
  });
  test('proposal mode works when affected workspaces omit upstream', () => {
    const { s, d } = pending();
    d.proposals[0].mode = 'apply';
    for (const e of s.sessions) delete e.status.workspaces.web.upstream;
    expect(waitingCount(s, d, capable, now)).toBe(0);
    expect(waitingGroups(s, d, capable, now)[0].applying[0].affected).toHaveLength(2);
  });
  test('proposal mode wins over an older workspace mode in either direction', () => {
    const { s, d } = pending();
    d.proposals[0].mode = 'confirm';
    expect(waitingGroups(s, d, capable, now)[0].items[0].mode).toBe('confirm');
    for (const e of s.sessions) e.status.workspaces.web.upstream!.deps!.mode = 'confirm';
    d.proposals[0].mode = 'apply';
    expect(waitingCount(s, d, capable, now)).toBe(0);
    expect(waitingGroups(s, d, capable, now)[0].applying[0].mode).toBe('apply');
  });
  test('apply proposals stay quiet after the last workspace moves and before settlement', () => {
    const { s, d } = pending();
    d.proposals[0].mode = 'apply';
    for (const e of s.sessions) {
      e.status.workspaces.web.pending = [];
      e.status.manifest.repos.web.base.deps = d.proposals[0].to;
      delete e.status.workspaces.web.upstream;
    }
    const g = waitingGroups(s, d, capable, now)[0];
    expect(g.items).toHaveLength(0);
    expect(g.applying[0].progress).toBe('2/2 moved');
    expect(waitingCount(s, d, capable, now)).toBe(0);
  });
  test('older rows still fall back to workspace mode, then conservative confirm', () => {
    const { s, d } = pending();
    expect(waitingCount(s, d, capable, now)).toBe(0);
    s.sessions[0].status.workspaces.web.upstream!.deps!.mode = 'confirm';
    expect(waitingGroups(s, d, capable, now)[0].items[0].mode).toBe('confirm');
    for (const e of s.sessions) delete e.status.workspaces.web.upstream;
    expect(waitingGroups(s, d, capable, now)[0].items[0].mode).toBe('confirm');
    expect(waitingCount(undefined, d, capable, now)).toBe(1);
  });
  test.each(['unhealthy', 'workspace scene', 'session scene'])(
    'all affected workspaces without upstream retain their %s reason',
    (condition) => {
      const { s, d } = pending();
      d.proposals[0].mode = 'apply';
      for (const e of s.sessions) {
        const w = e.status.workspaces.web;
        delete w.upstream;
        if (condition === 'unhealthy') w.health = 'Missing';
        if (condition === 'workspace scene') w.scene = {};
        if (condition === 'session scene') e.status.scene = {};
      }
      const g = waitingGroups(s, d, capable, now)[0];
      expect(g.applying).toHaveLength(0);
      expect(g.items).toHaveLength(1);
      expect(g.items[0].mode).toBe('apply');
      expect(g.items[0].reason).toBe(
        condition === 'unhealthy'
          ? 'needs the human: workspace unhealthy'
          : 'needs the human: scene'
      );
      expect(waitingCount(s, d, capable, now)).toBe(1);
    }
  );
  test('stuck threshold is strictly more than 24 hours', () => {
    const { s, d } = pending();
    for (const e of s.sessions)
      e.status.workspaces.web.upstream!.auto!.since = new Date(now - STUCK_AFTER).toISOString();
    expect(waitingCount(s, d, capable, now)).toBe(0);
    expect(waitingCount(s, d, capable, now + 1)).toBe(1);
  });
  test.each(['install_failed', 'own_change', 'drift', 'waiting_human'])(
    '%s needs attention immediately',
    (state) => {
      const { s, d } = pending();
      s.sessions[0].status.workspaces.web.upstream!.deps!.state = state;
      expect(waitingGroups(s, d, capable, now)[0].items[0].number).toBe(75);
    }
  );
  test('unhealthy, scene, not following and diverged cannot hide in applying', () => {
    for (const condition of ['unhealthy', 'scene', 'following', 'diverged']) {
      const { s, d } = pending();
      const w = s.sessions[0].status.workspaces.web;
      if (condition === 'unhealthy') w.health = 'Missing';
      if (condition === 'scene') w.scene = {};
      if (condition === 'following') w.upstream!.following = false;
      if (condition === 'diverged') w.upstream!.diverged = true;
      expect(waitingCount(s, d, capable, now)).toBe(1);
    }
  });
  test('an unhealthy workspace without upstream still needs the human in an apply repo', () => {
    const { s, d } = pending();
    s.sessions[0].status.workspaces.web.health = 'Missing';
    delete s.sessions[0].status.workspaces.web.upstream;
    const item = waitingGroups(s, d, capable, now)[0].items[0];
    expect(item.mode).toBe('apply');
    expect(item.reason).toBe('needs the human: workspace unhealthy');
  });
  test('auto conflicts point to the session agent and do not create a deps approval', () => {
    const { s, d } = pending();
    const e = s.sessions[0];
    e.status.workspaces.web.upstream!.auto!.reason = 'conflicts';
    const items = waitingGroups(s, d, capable, now)[0].items;
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe('conflict');
    expect(items[0].command).toBe(`git-strata sync --session ${e.status.manifest.id} --repo web`);
  });
  test('cached behind and detail upstream require the capability; cold entries are not zero', () => {
    const { s } = pending();
    expect(sessionRows(s)[0].behind).toBeUndefined();
    expect(sessionRows(s, capable)[0].behind).toBe(3);
    const id = s.sessions[0].status.manifest.id;
    expect(sessionDetail(s, id)?.repos.find((r) => r.repo === 'web')?.upstream).toBeUndefined();
    expect(
      sessionDetail(s, id, capable)?.repos.find((r) => r.repo === 'web')?.upstream?.behind
    ).toBe(3);
    for (const e of s.sessions)
      e.status.workspaces.web.upstream = { behind: null, unchecked: true };
    expect(sessionRows(s, capable)[0].behind).toBeUndefined();
  });
});

describe('tray policy parity', () => {
  const capable = { ...daemon, features: ['status.upstream'] };
  function deferred() {
    const s = structuredClone(all);
    for (const [i, entry] of s.sessions.entries()) {
      entry.status.upstream = { clean: true, repos: {} };
      entry.status.workspaces.web.pending = [];
      entry.status.manifest.repos.web.tracked = true;
      entry.status.workspaces.web.upstream = {
        behind: 1,
        auto: {
          state: 'deferred',
          source: 'replay',
          reason: 'open_files',
          since: new Date(Date.now() - (26 + i) * 3600000).toISOString(),
          attempts: 2,
        },
      };
    }
    return s;
  }
  test('tagged warnings name their variant and malformed lists are empty', () => {
    expect(
      daemonView({
        ...daemon,
        warnings: [
          'StorePoisoned',
          { VerifyFailed: { path: 'secret' } },
          { ConfigRelaxPending: { repo: 'web' } },
          { FutureVariant: {} },
        ],
      }).warnings
    ).toEqual(['StorePoisoned', 'VerifyFailed', 'ConfigRelaxPending', 'FutureVariant']);
    expect(daemonView({ ...daemon, warnings: {} as unknown as unknown[] }).warnings).toEqual([]);
  });
  test('Deferred aggregates by repo and opens oldest eligible session; ConfigRelax opens Daemon', () => {
    const s = deferred();
    const d = { proposals: [] };
    const items = waitingItems(s, d, capable);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: 'deferred', repo: 'web', number: 2 });
    expect(openTarget('waiting:deferred:web', s, d, capable)).toEqual({
      session: s.sessions[1].status.manifest.id,
    });
    expect(openTarget('waiting:relax:web', s, d, capable)).toEqual({
      session: null,
      screen: 'daemon',
    });
    const warnings = [
      { ConfigRelaxPending: { repo: 'web', reason: 'secret' } },
      { ConfigRelaxPending: { repo: 'web' } },
    ];
    expect(waitingItems(undefined, undefined, { ...daemon, warnings })).toEqual([
      {
        key: 'relax:web',
        kind: 'relax',
        repo: 'web',
        number: 0,
        session: null,
        since: null,
        reason: 'Config relax waiting',
      },
    ]);
  });
  test('covered apply sessions are excluded from Deferred count, conflict deferrals never wait', () => {
    const s = deferred();
    const p = {
      ...deps.proposals[0],
      id: 99,
      repo: 'web',
      mode: 'apply' as const,
      state: { kind: 'Pending' },
    };
    s.sessions[0].status.workspaces.web.pending = [p];
    const items = waitingItems(s, { proposals: [p] }, capable);
    expect(items.map((i) => [i.kind, i.number])).toEqual([
      ['deps', 99],
      ['deferred', 1],
    ]);
    for (const e of s.sessions) e.status.workspaces.web.upstream!.auto!.reason = 'conflicts';
    expect(waitingCount(s, { proposals: [p] }, capable)).toBe(0);
    expect(waitingItems(s, { proposals: [p] }, capable).every((i) => i.kind === 'conflict')).toBe(
      true
    );
  });
  test('unknown enrichment suppresses upstream items while direct proposals and workspace conflicts survive', async () => {
    const { unavailableUpstream } = await import('./view');
    const s = deferred();
    for (const e of s.sessions) delete e.status.upstream;
    expect(unavailableUpstream(s, capable)).toHaveLength(2);
    expect(waitingItems(s, { proposals: [] }, capable)).toHaveLength(0);
    const p = { ...deps.proposals[0], state: { kind: 'Pending' }, mode: 'confirm' as const };
    s.sessions[0].status.workspaces.web.conflicts = ['secret'];
    expect(waitingItems(s, { proposals: [p] }, capable).map((i) => i.kind)).toEqual([
      'deps',
      'conflict',
    ]);
    s.sessions[0].status.upstream = { clean: false, repos: {} };
    expect(unavailableUpstream(s, capable)).toEqual([s.sessions[1].status.manifest.id]);
  });
});
