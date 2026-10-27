'use client';

import { copy } from '@/lib/copy';
import type { RepoChip } from '@/lib/view';

/**
 * A row's repos, all of them, wrapping onto more lines as needed (the row grows,
 * top-aligned). Used repos come first as teal tags and toggle the same selection as the
 * filter bar above the list; the click stops here so the row does not also open the
 * session. Unused repos (not tracked, no changes) stay muted and inert.
 */
export function RepoTags({
  repos,
  selected,
  onToggle,
}: {
  repos: RepoChip[];
  selected: string[];
  onToggle: (repo: string) => void;
}) {
  const f = copy.sessions.filter;
  return (
    <div className="repo-tags">
      {repos.map((r) => {
        if (!r.used)
          return (
            <span key={r.name} className="repo-tag" data-used="false" title={f.unused(r.name)}>
              {r.name}
            </span>
          );
        const on = selected.includes(r.name);
        return (
          <button
            key={r.name}
            type="button"
            className="repo-tag"
            data-used="true"
            aria-pressed={on}
            title={f.rowTag(r.name, on)}
            onClick={(e) => {
              e.stopPropagation();
              onToggle(r.name);
            }}
          >
            {r.name}
          </button>
        );
      })}
    </div>
  );
}
