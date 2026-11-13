'use client';

import { useEffect } from 'react';
import { useNow } from '@/hooks/useNow';
import { useQueryClient } from '@tanstack/react-query';
import { sessionFromUrl, sessionHash, useNav } from '@/stores/nav';
import { useTheme } from '@/stores/theme';
import { usePlan, useSessions, useDeps } from '@/lib/queries';
import { OPEN_EVENT, inTauri } from '@/lib/tauri';
import { refreshAfter } from '@/lib/refresh';
import { openTarget, waitingGroups, unavailableUpstream } from '@/lib/view';
import type { DaemonStatus, DepsList, StatusReply } from '@/lib/types';
import { Titlebar } from './Titlebar';
import { StatusBar } from './StatusBar';
import { CommandPalette } from './CommandPalette';
import { DaemonScreen } from './DaemonScreen';
import { SessionsScreen } from './SessionsScreen';
import { SessionDetailModal } from './SessionDetail';
import { ReviewModal } from './ReviewModal';

/** Navigate first, then focus/open the existing Waiting popover without closing an open one. */
export function showWaiting(go: (screen: 'sessions') => void): void {
  go('sessions');
  requestAnimationFrame(() => {
    const note = document.querySelector<HTMLButtonElement>('button.waiting-note');
    if (!note) return;
    note.focus();
    const list = document.querySelector('.waiting-list');
    if (!list || list.getClientRects().length === 0) note.click();
  });
}

export function Shell() {
  const { screen, session, setPalette, openSession, go } = useNav();
  const setTheme = useTheme((s) => s.setChoice);
  const { plan, status } = usePlan();
  const sessions = useSessions(plan.sessions);
  // Kept warm so a tray "deps" item can find its session; the status bar counts from it too.
  const deps = useDeps(plan.deps);
  const qc = useQueryClient();
  const now = useNow();

  // ⌘K / Ctrl+K opens the palette.
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setPalette(true);
      }
    };
    window.addEventListener('keydown', on);
    return () => window.removeEventListener('keydown', on);
  }, [setPalette]);

  // Deep links: /session/<id>, ?session=<id> or #/session/<id> open the list with that
  // session's modal; the hash then follows the modal so a reload reopens it.
  useEffect(() => {
    const read = () => {
      const id = sessionFromUrl(window.location);
      if (id && id !== useNav.getState().session) openSession(id);
    };
    read();
    window.addEventListener('hashchange', read);
    return () => window.removeEventListener('hashchange', read);
  }, [openSession]);
  useEffect(() => {
    const { pathname, search, hash } = window.location;
    const path = pathname.startsWith('/session/') ? '/' : pathname;
    const params = new URLSearchParams(search);
    params.delete('session');
    const q = params.toString();
    const next = `${path}${q ? `?${q}` : ''}${session ? sessionHash(session) : ''}`;
    if (next !== `${pathname}${search}${hash}`) window.history.replaceState(null, '', next);
  }, [session]);

  // Tray "Waiting for you" items (src-tauri/src/tray.rs) land on the session they belong to.
  useEffect(() => {
    if (!inTauri()) return;
    let unlisten: (() => void) | undefined;
    let gone = false;
    import('@tauri-apps/api/event').then(({ listen }) =>
      listen<string>(OPEN_EVENT, async (e) => {
        await refreshAfter(qc, [['daemonStatus'], ['sessions'], ['deps']]);
        if (gone) return;
        const t = openTarget(
          e.payload,
          qc.getQueryData<StatusReply>(['sessions']),
          qc.getQueryData<DepsList>(['deps']),
          qc.getQueryData<DaemonStatus>(['daemonStatus'])
        );
        if (t.screen === 'daemon') go('daemon');
        else openSession(t.session);
      }).then((u) => {
        if (gone) u();
        else unlisten = u;
      })
    );
    return () => {
      gone = true;
      unlisten?.();
    };
  }, [qc, openSession, go]);

  return (
    <div className="shell">
      <Titlebar />
      <main className="main">
        {screen === 'daemon' ? (
          <DaemonScreen
            status={status}
            plan={plan}
            unavailable={unavailableUpstream(sessions.data, status.data)}
            onBack={() => go('sessions')}
          />
        ) : (
          <SessionsScreen
            sessions={sessions}
            daemon={status}
            legacy={!status.data || !('api' in status.data)}
            waiting={waitingGroups(sessions.data, deps.data, status.data, now)}
            waitingStale={deps.isError || sessions.isError}
          />
        )}
      </main>
      <SessionDetailModal id={session} interval={plan.session} daemon={status.data} />
      <ReviewModal />
      <StatusBar
        status={status}
        sessions={sessions}
        deps={deps.data}
        onWaiting={() => showWaiting(go)}
        onSession={openSession}
      />
      <CommandPalette
        sessions={sessions.data}
        onRefresh={() => qc.invalidateQueries()}
        onTheme={setTheme}
      />
    </div>
  );
}
