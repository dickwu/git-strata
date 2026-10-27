'use client';

import { useEffect } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useNav } from '@/stores/nav';
import { useTheme } from '@/stores/theme';
import { isSidebarShortcut, useLayout } from '@/stores/layout';
import { usePlan, useSessions, useDeps } from '@/lib/queries';
import { OPEN_EVENT, inTauri } from '@/lib/tauri';
import { openTarget, waitingCount } from '@/lib/view';
import type { DepsList, StatusReply } from '@/lib/types';
import { Titlebar } from './Titlebar';
import { Sidebar } from './Sidebar';
import { StatusBar } from './StatusBar';
import { CommandPalette } from './CommandPalette';
import { DaemonScreen } from './DaemonScreen';
import { SessionsScreen } from './SessionsScreen';
import { SessionDetail } from './SessionDetail';

export function Shell() {
  const { screen, session, setPalette, openSession } = useNav();
  const sidebarCollapsed = useLayout((s) => s.sidebarCollapsed);
  const toggleTheme = useTheme((s) => s.toggle);
  const { plan, status } = usePlan();
  const sessions = useSessions(plan.sessions);
  // Kept warm so a tray "deps" item can find its session.
  const deps = useDeps(plan.deps);
  const qc = useQueryClient();

  // ⌘K / Ctrl+K opens the palette; ⌘\ / Ctrl+\ toggles the sidebar.
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setPalette(true);
      } else if (isSidebarShortcut(e)) {
        e.preventDefault();
        useLayout.getState().toggleSidebar();
      }
    };
    window.addEventListener('keydown', on);
    return () => window.removeEventListener('keydown', on);
  }, [setPalette]);

  // Tray "Waiting for you" items (src-tauri/src/tray.rs) land on the session they belong to.
  useEffect(() => {
    if (!inTauri()) return;
    let unlisten: (() => void) | undefined;
    let gone = false;
    import('@tauri-apps/api/event').then(({ listen }) =>
      listen<string>(OPEN_EVENT, (e) => {
        const t = openTarget(
          e.payload,
          qc.getQueryData<StatusReply>(['sessions']),
          qc.getQueryData<DepsList>(['deps'])
        );
        openSession(t.session);
      }).then((u) => {
        if (gone) u();
        else unlisten = u;
      })
    );
    return () => {
      gone = true;
      unlisten?.();
    };
  }, [qc, openSession]);

  return (
    <div className="shell" data-collapsed={sidebarCollapsed}>
      <Titlebar />
      <Sidebar
        sessions={sessions.data}
        daemonUp={status.isSuccess}
        waiting={waitingCount(sessions.data, deps.data)}
      />
      <main className="main">
        {screen === 'daemon' ? (
          <DaemonScreen status={status} plan={plan} />
        ) : session ? (
          <SessionDetail id={session} interval={plan.session} />
        ) : (
          <SessionsScreen
            sessions={sessions}
            daemon={status}
            legacy={!status.data || !('api' in status.data)}
          />
        )}
      </main>
      <StatusBar status={status} sessions={sessions.data} />
      <CommandPalette
        sessions={sessions.data}
        onRefresh={() => qc.invalidateQueries()}
        onToggleTheme={toggleTheme}
      />
    </div>
  );
}
