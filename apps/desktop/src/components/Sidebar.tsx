'use client';

import type { ReactNode } from 'react';
import { Tooltip } from 'antd';
import {
  CloudServerOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  UnorderedListOutlined,
} from '@ant-design/icons';
import { copy } from '@/lib/copy';
import { sessionRows } from '@/lib/view';
import type { StatusReply } from '@/lib/types';
import { useLayout } from '@/stores/layout';
import { useNav } from '@/stores/nav';

/** Tooltip only when collapsed: expanded rows already show their label. */
function Tip({ show, title, children }: { show: boolean; title: string; children: ReactNode }) {
  return (
    <Tooltip title={show ? title : undefined} placement="right" mouseEnterDelay={0.3}>
      {children}
    </Tooltip>
  );
}

export function Sidebar({
  sessions,
  daemonUp,
  waiting,
}: {
  sessions: StatusReply | undefined;
  daemonUp: boolean;
  /** Things waiting for a human (tray rule); badged on Sessions in both widths. */
  waiting: number;
}) {
  const { screen, session, go, openSession } = useNav();
  const { sidebarCollapsed: collapsed, toggleSidebar } = useLayout();
  const rows = sessionRows(sessions);
  const n = copy.nav;
  const toggle = collapsed ? n.expand : n.collapse;
  const sessionsName = waiting > 0 ? `${n.sessions}, ${n.waiting(waiting)}` : n.sessions;
  const daemonName = `${n.daemon}, ${daemonUp ? n.up : n.down}`;
  return (
    <nav className="sidebar" aria-label={n.main} data-collapsed={collapsed}>
      <Tip show title={`${toggle} (${n.shortcut})`}>
        <button
          type="button"
          className="nav-item sidebar-toggle"
          onClick={toggleSidebar}
          aria-label={toggle}
          aria-expanded={!collapsed}
          aria-keyshortcuts="Meta+Backslash"
        >
          {collapsed ? <MenuUnfoldOutlined aria-hidden /> : <MenuFoldOutlined aria-hidden />}
        </button>
      </Tip>
      <Tip show={collapsed} title={sessionsName}>
        <button
          type="button"
          className="nav-item"
          aria-current={screen === 'sessions' && !session ? 'page' : undefined}
          aria-label={sessionsName}
          onClick={() => go('sessions')}
        >
          <span className="nav-icon">
            <UnorderedListOutlined aria-hidden />
            {waiting > 0 && <span className="nav-badge-dot" aria-hidden />}
          </span>
          <span className="nav-label">{n.sessions}</span>
          {waiting > 0 && (
            <span className="nav-badge" aria-hidden>
              {waiting}
            </span>
          )}
        </button>
      </Tip>
      <div className="nav-sub" hidden={collapsed}>
        {rows.map((r) => (
          <button
            key={r.id}
            type="button"
            className="nav-item"
            aria-current={session === r.id ? 'page' : undefined}
            onClick={() => openSession(r.id)}
          >
            <span className="nav-label">{r.ticket ?? r.id}</span>
            {r.conflicts > 0 && (
              <span className="dot" data-state="down" role="img" aria-label={n.conflicts} />
            )}
          </button>
        ))}
      </div>
      <Tip show={collapsed} title={daemonName}>
        <button
          type="button"
          className="nav-item"
          aria-current={screen === 'daemon' ? 'page' : undefined}
          aria-label={daemonName}
          onClick={() => go('daemon')}
        >
          <span className="nav-icon">
            <CloudServerOutlined aria-hidden />
            <span className="dot nav-icon-dot" data-state={daemonUp ? 'up' : 'down'} aria-hidden />
          </span>
          <span className="nav-label">{n.daemon}</span>
        </button>
      </Tip>
    </nav>
  );
}
