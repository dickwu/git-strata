'use client';

import { Alert } from 'antd';
import { copy } from '@/lib/copy';
import { splitInlineCode } from '@/lib/inlineCode';
import { asGuiError, reason } from '@/lib/tauri';

/** One place that turns a GuiError into §5.2 copy. */
export function errorText(e: unknown): { title: string; hint?: string } {
  const g = asGuiError(e);
  switch (reason(g)) {
    case 'daemon_down':
      return { title: copy.daemon.down, hint: copy.daemon.downHint };
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

export function ErrorNote({ error }: { error: unknown }) {
  const t = errorText(error);
  const hint = t.hint
    ? splitInlineCode(t.hint).map((p, i) =>
        p.code ? (
          <code key={i} className="inline-code">
            {p.text}
          </code>
        ) : (
          <span key={i}>{p.text}</span>
        )
      )
    : undefined;
  return <Alert type="warning" showIcon title={t.title} description={hint} />;
}
