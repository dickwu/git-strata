/** ⌘K entries and filtering. */
export type Action =
  | { kind: 'screen'; screen: 'sessions' | 'daemon' }
  | { kind: 'session'; id: string }
  | { kind: 'theme' }
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
