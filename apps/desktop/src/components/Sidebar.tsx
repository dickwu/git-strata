'use client';

import {
  CloudServerOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  UnorderedListOutlined,
} from '@ant-design/icons';
import { copy } from '@/lib/copy';
import { sessionRows } from '@/lib/view';
import type { StatusReply } from '@/lib/types';
import { useNav } from '@/stores/nav';

export function Sidebar({
  sessions,
  daemonUp,
}: {
  sessions: StatusReply | undefined;
  daemonUp: boolean;
}) {
  const { screen, session, sidebarCollapsed, go, openSession, toggleSidebar } = useNav();
  const rows = sessionRows(sessions);
  const label = (s: string) => (sidebarCollapsed ? null : <span>{s}</span>);
  return (
    <nav className="sidebar" aria-label="Main">
      <button
        type="button"
        className="nav-item"
        aria-current={screen === 'sessions' && !session ? 'page' : undefined}
        onClick={() => go('sessions')}
        title={copy.nav.sessions}
      >
        <UnorderedListOutlined />
        {label(copy.nav.sessions)}
      </button>
      {!sidebarCollapsed &&
        rows.map((r) => (
          <button
            key={r.id}
            type="button"
            className="nav-item"
            style={{ paddingLeft: 32 }}
            aria-current={session === r.id ? 'page' : undefined}
            onClick={() => openSession(r.id)}
          >
            <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {r.ticket ?? r.id}
            </span>
            {r.conflicts > 0 && (
              <span className="dot" data-state="down" aria-label="Has conflicts" />
            )}
          </button>
        ))}
      <button
        type="button"
        className="nav-item"
        aria-current={screen === 'daemon' ? 'page' : undefined}
        onClick={() => go('daemon')}
        title={copy.nav.daemon}
      >
        <CloudServerOutlined />
        {label(copy.nav.daemon)}
        {!sidebarCollapsed && (
          <span
            className="dot"
            data-state={daemonUp ? 'up' : 'down'}
            style={{ marginLeft: 'auto' }}
          />
        )}
      </button>
      <button
        type="button"
        className="nav-item"
        onClick={toggleSidebar}
        aria-label="Collapse sidebar"
      >
        {sidebarCollapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />}
      </button>
    </nav>
  );
}
