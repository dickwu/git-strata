/** ⌘K entries and filtering. */
import { copy } from './copy';
import { THEME_CHOICES, type ThemeChoice } from '@/stores/theme';
import type { StatusReply } from './types';
import { sessionRows, usedRepos } from './view';

export type Action =
  | { kind: 'screen'; screen: 'sessions' | 'daemon' }
  | { kind: 'session'; id: string }
  | { kind: 'repo'; repo: string }
  | { kind: 'agent'; agent: string }
  | { kind: 'theme'; choice: ThemeChoice }
  | { kind: 'refresh' };

export interface Entry {
  key: string;
  label: string;
  hint?: string;
  keywords: string[];
  action: Action;
}

/**
 * Case-insensitive; every query word must appear in the label, hint or keywords.
 * Ranking: label starts with the query, then a label word starts with it, then the rest;
 * ties keep input order. An empty query returns everything.
 */
export function filterEntries(entries: Entry[], query: string): Entry[] {
  const q = query.trim().toLowerCase();
  if (!q) return entries;
  const words = q.split(/\s+/);
  const scored: Array<{ e: Entry; score: number; i: number }> = [];
  entries.forEach((e, i) => {
    const label = e.label.toLowerCase();
    const hay = [
      label,
      (e.hint ?? '').toLowerCase(),
      ...e.keywords.map((k) => k.toLowerCase()),
    ].join(' ');
    if (!words.every((w) => hay.includes(w))) return;
    const score = label.startsWith(q)
      ? 0
      : label.split(/\W+/).some((w) => w.startsWith(words[0]))
        ? 1
        : 2;
    scored.push({ e, score, i });
  });
  return scored.sort((a, b) => a.score - b.score || a.i - b.i).map((s) => s.e);
}

/**
 * Sessions first (searchable by ticket, id, state, phase and repo names), then one
 * "Sessions with <repo>" entry per repo, which filters the list, then screens and commands.
 */
export function buildEntries(sessions: StatusReply | undefined): Entry[] {
  const screens: Entry[] = [
    {
      key: 'screen:sessions',
      label: copy.palette.goTo(copy.nav.sessions),
      keywords: ['list'],
      action: { kind: 'screen', screen: 'sessions' },
    },
    {
      key: 'screen:daemon',
      label: copy.palette.goTo(copy.nav.daemon),
      keywords: ['status', 'socket', 'warnings'],
      action: { kind: 'screen', screen: 'daemon' },
    },
    // One entry per choice, the same three the status bar switch offers (t93u).
    ...THEME_CHOICES.map((choice): Entry => ({
      key: `theme:${choice}`,
      label: copy.palette.theme(copy.theme.names[choice]),
      keywords: ['theme', 'appearance', 'mode', ...(choice === 'system' ? ['auto', 'os'] : [])],
      action: { kind: 'theme', choice },
    })),
    {
      key: 'refresh',
      label: copy.palette.refresh,
      keywords: ['reload'],
      action: { kind: 'refresh' },
    },
  ];
  const rows = sessionRows(sessions);
  const sessionEntries: Entry[] = rows.map((r) => ({
    key: `session:${r.id}`,
    label: r.ticket ?? r.id,
    hint: r.id,
    keywords: [r.state, r.phase, ...(r.agent ? [r.agent] : []), ...r.repoList.map((x) => x.name)],
    action: { kind: 'session', id: r.id },
  }));
  // Only repos some session uses (t73u), alphabetical.
  const repos = usedRepos(rows)
    .map((x) => x.name)
    .sort();
  const repoEntries: Entry[] = repos.map((repo) => ({
    key: `repo:${repo}`,
    label: copy.palette.withRepo(repo),
    keywords: ['repo', 'filter'],
    action: { kind: 'repo', repo },
  }));
  const agents = [...new Set(rows.flatMap((r) => (r.agent ? [r.agent] : [])))].sort();
  const agentEntries: Entry[] = agents.map((agent) => ({
    key: `agent:${agent}`,
    label: copy.palette.byAgent(agent),
    keywords: ['agent', 'ai', 'filter'],
    action: { kind: 'agent', agent },
  }));
  return [...sessionEntries, ...repoEntries, ...agentEntries, ...screens];
}
