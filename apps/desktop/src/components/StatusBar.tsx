'use client';

import { useEffect, useState } from 'react';
import type { UseQueryResult } from '@tanstack/react-query';
import { ago, bytes, copy } from '@/lib/copy';
import { daemonCopy as t } from '@/lib/daemonCopy';
import { useDu } from '@/lib/queries';
import { isDaemonDown } from '@/lib/tauri';
import { errorText } from './ErrorNote';
import { DaemonStatusModal } from './DaemonStatusModal';
import type { DaemonStatus, StatusReply } from '@/lib/types';
import css from './StatusBar.module.css';

/** The bottom bar: the daemon lives here; clicking it opens its status modal. */
export function StatusBar({
  status,
  sessions,
}: {
  status: UseQueryResult<DaemonStatus>;
  sessions: StatusReply | undefined;
}) {
  const [now, setNow] = useState(() => Date.now());
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(t);
  }, []);
  const down = status.isError && isDaemonDown(status.error);
  // Live only while the last daemon_status answered; any other error shows its own text.
  const live = status.isSuccess || (status.data !== undefined && !status.isError);
  // du answers from the daemon's cache (daemon.md §5.12); the same key the daemon screen polls.
  const du = useDu(live ? 60_000 : false);
  const state = status.isPending ? 'pending' : live ? 'up' : 'down';
  const name = status.isPending
    ? t.trigger.connecting
    : live
      ? t.trigger.running
      : t.trigger.offline;
  const detail = status.isPending
    ? null
    : live
      ? copy.status.live(ago(now - status.dataUpdatedAt))
      : down
        ? null
        : errorText(status.error).title;
  const saved = live ? du.data?.savings?.live.savings : undefined;
  return (
    <footer className="statusbar" role="status">
      <button
        type="button"
        className={css.trigger}
        onClick={() => setOpen(true)}
        aria-haspopup="dialog"
        aria-label={`${name}. ${t.trigger.open}`}
        data-testid="daemon-trigger"
      >
        <span className="dot" data-state={state} />
        <span className={css.name}>{name}</span>
        {detail && <span className={css.detail}>{detail}</span>}
      </button>
      <span className={css.spacer} />
      {saved !== undefined && saved > 0 && (
        <button
          type="button"
          className={`${css.trigger} ${css.saved}`}
          onClick={() => setOpen(true)}
          aria-haspopup="dialog"
        >
          {t.trigger.saved(bytes(saved))}
        </button>
      )}
      {sessions && <span>{copy.status.sessions(sessions.sessions.length)}</span>}
      <DaemonStatusModal
        open={open}
        onClose={() => setOpen(false)}
        status={status}
        du={du}
        sessions={sessions}
        now={now}
      />
    </footer>
  );
}
