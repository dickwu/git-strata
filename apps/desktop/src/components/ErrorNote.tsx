'use client';

import { Alert } from 'antd';
import { copy } from '@/lib/copy';
import { asGuiError, reason } from '@/lib/tauri';
import { DaemonDownAlert } from './DaemonStart';
import { Inline } from './Inline';

/** One place that turns a GuiError into §5.2 copy. */
export function errorText(e: unknown): { title: string; hint?: string } {
  const g = asGuiError(e);
  switch (reason(g)) {
    case 'daemon_down':
      return { title: copy.daemon.down };
    case 'daemon_timeout': {
      const d = g.detail as { seconds?: number };
      return { title: copy.daemon.stuck(d.seconds ?? 0) };
    }
    case 'request_in_flight':
      return { title: copy.daemon.busy };
    case 'not_tauri':
      return { title: copy.errors.notInTauri };
    default:
      return { title: g.message || copy.errors.unknown };
  }
}

/** A down daemon gets the Start daemon notice (S0); anything else is a plain warning. */
export function ErrorNote({ error }: { error: unknown }) {
  if (reason(asGuiError(error)) === 'daemon_down') return <DaemonDownAlert />;
  const t = errorText(error);
  return (
    <Alert
      type="warning"
      showIcon
      title={t.title}
      description={t.hint ? <Inline text={t.hint} /> : undefined}
    />
  );
}
