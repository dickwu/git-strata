'use client';

import { useMemo, useState } from 'react';
import { Input, Modal } from 'antd';
import { copy } from '@/lib/copy';
import { buildEntries, filterEntries, type Entry } from '@/lib/palette';
import type { StatusReply } from '@/lib/types';
import { useNav } from '@/stores/nav';
import type { ThemeChoice } from '@/stores/theme';

export function CommandPalette({
  sessions,
  onRefresh,
  onTheme,
}: {
  sessions: StatusReply | undefined;
  onRefresh: () => void;
  onTheme: (choice: ThemeChoice) => void;
}) {
  const {
    paletteOpen,
    setPalette,
    go,
    openSession,
    addRepoFilter,
    agentFilter,
    toggleAgentFilter,
  } = useNav();
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const entries = useMemo(() => filterEntries(buildEntries(sessions), query), [sessions, query]);

  const close = () => {
    setPalette(false);
    setQuery('');
    setActive(0);
  };
  const run = (e: Entry | undefined) => {
    if (!e) return;
    const a = e.action;
    if (a.kind === 'screen') go(a.screen);
    else if (a.kind === 'session') openSession(a.id);
    else if (a.kind === 'repo') {
      go('sessions');
      addRepoFilter(a.repo);
    } else if (a.kind === 'agent') {
      go('sessions');
      if (!agentFilter.includes(a.agent)) toggleAgentFilter(a.agent);
    } else if (a.kind === 'theme') onTheme(a.choice);
    else onRefresh();
    close();
  };

  return (
    <Modal
      open={paletteOpen}
      onCancel={close}
      footer={null}
      closable={false}
      width={560}
      destroyOnHidden
    >
      <Input
        autoFocus
        size="large"
        placeholder={copy.palette.placeholder}
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
        }}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') {
            e.preventDefault();
            setActive((i) => Math.min(i + 1, entries.length - 1));
          } else if (e.key === 'ArrowUp') {
            e.preventDefault();
            setActive((i) => Math.max(i - 1, 0));
          } else if (e.key === 'Enter') {
            run(entries[active]);
          }
        }}
        aria-controls="palette-list"
        aria-activedescendant={entries[active] ? `palette-${entries[active].key}` : undefined}
      />
      <div
        id="palette-list"
        role="listbox"
        style={{ marginTop: 8, maxHeight: 320, overflowY: 'auto' }}
      >
        {entries.length === 0 && (
          <div className="muted" style={{ padding: 8 }}>
            {copy.palette.empty}
          </div>
        )}
        {entries.map((e, i) => (
          <div
            key={e.key}
            id={`palette-${e.key}`}
            role="option"
            aria-selected={i === active}
            className="nav-item"
            data-active={i === active}
            onMouseEnter={() => setActive(i)}
            onClick={() => run(e)}
          >
            <span>{e.label}</span>
            {e.hint && (
              <span className="mono" style={{ marginLeft: 'auto', opacity: 0.7 }}>
                {e.hint}
              </span>
            )}
          </div>
        ))}
      </div>
    </Modal>
  );
}
