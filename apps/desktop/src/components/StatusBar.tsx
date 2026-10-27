'use client';

import { useEffect, useState } from 'react';
import type { UseQueryResult } from '@tanstack/react-query';
import { ago, copy } from '@/lib/copy';
import { isDaemonDown } from '@/lib/tauri';
import { errorText } from './ErrorNote';
import type { DaemonStatus, StatusReply } from '@/lib/types';

export function StatusBar({
  status,
  sessions,
}: {
  status: UseQueryResult<DaemonStatus>;
  sessions: StatusReply | undefined;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(t);
  }, []);
  const down = status.isError && isDaemonDown(status.error);
  // Live only while the last daemon_status answered; any other error shows its own text.
  const live = status.isSuccess || (status.data !== undefined && !status.isError);
  const state = status.isPending ? 'pending' : live ? 'up' : 'down';
  const text = status.isPending
    ? copy.status.loading
    : live
      ? copy.status.live(ago(now - status.dataUpdatedAt))
      : down
        ? copy.status.offline
        : errorText(status.error).title;
  return (
    <footer className="statusbar" role="status">
      <span style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
        <span className="dot" data-state={state} />
        {text}
      </span>
      {sessions && <span>{copy.status.sessions(sessions.sessions.length)}</span>}
    </footer>
  );
}
