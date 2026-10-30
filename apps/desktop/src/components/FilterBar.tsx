'use client';

import { Button, Input } from 'antd';
import { SearchOutlined } from '@ant-design/icons';
import { copy } from '@/lib/copy';
import { useNav } from '@/stores/nav';

type Option = { name: string; sessions: number };

function TagGroup({
  label,
  hint,
  options,
  selected,
  onToggle,
}: {
  label: string;
  hint: string;
  options: Option[];
  selected: string[];
  onToggle: (name: string) => void;
}) {
  if (options.length === 0) return null;
  const f = copy.sessions.filter;
  return (
    <div className="filter-group" role="group" aria-label={`${label}. ${hint}`}>
      <span className="filter-group-label" title={hint}>
        {label}
      </span>
      <div className="filter-tags">
        {options.map((o) => {
          const on = selected.includes(o.name);
          return (
            <button
              key={o.name}
              type="button"
              className="filter-tag"
              aria-pressed={on}
              aria-label={f.toggle(o.name, o.sessions, on)}
              onClick={() => onToggle(o.name)}
            >
              <span>{o.name}</span>
              <span className="filter-tag-count" aria-hidden>
                {o.sessions}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

/**
 * The tag bar above the Sessions list: a text filter, every used repo as a toggleable tag
 * with its session count, and agents once the daemon records them. Selection shows on the
 * tags themselves; row tags toggle the same selection.
 */
export function FilterBar({
  repos,
  agents,
  repoSel,
  agentSel,
  shown,
  total,
}: {
  repos: Option[];
  agents: Option[];
  repoSel: string[];
  agentSel: string[];
  shown: number;
  total: number;
}) {
  const { textFilter, setTextFilter, toggleRepoFilter, toggleAgentFilter, clearFilters } = useNav();
  const f = copy.sessions.filter;
  const active = repoSel.length > 0 || agentSel.length > 0 || textFilter.trim() !== '';
  return (
    <section className="filter-bar" role="search" aria-label={f.label}>
      <div className="filter-row">
        <Input
          allowClear
          size="small"
          prefix={<SearchOutlined aria-hidden />}
          placeholder={f.placeholder}
          aria-label={f.label}
          value={textFilter}
          onChange={(e) => setTextFilter(e.target.value)}
          style={{ width: 280 }}
        />
        <span className="muted filter-count" aria-live="polite">
          {active ? f.count(shown, total) : null}
        </span>
        <Button
          type="link"
          size="small"
          onClick={clearFilters}
          disabled={!active}
          style={{ marginLeft: 'auto' }}
        >
          {f.clear}
        </Button>
      </div>
      <TagGroup
        label={f.repos}
        hint={f.reposHint}
        options={repos}
        selected={repoSel}
        onToggle={toggleRepoFilter}
      />
      <TagGroup
        label={f.agents}
        hint={f.agentsHint}
        options={agents}
        selected={agentSel}
        onToggle={toggleAgentFilter}
      />
    </section>
  );
}
