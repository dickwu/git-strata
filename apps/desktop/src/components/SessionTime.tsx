'use client';

import { Tooltip } from 'antd';
import { compactTime, copy, duration, fullTime } from '@/lib/copy';
import { sessionAge } from '@/lib/view';

/**
 * Time column: when the session was created (compact, local) and how long it has existed.
 * `now` ticks once a minute from the caller; the tooltip has the full timestamps.
 */
export function SessionTime({
  created,
  state,
  now,
}: {
  created: string;
  state: string;
  now: number;
}) {
  const t = new Date(created);
  if (Number.isNaN(t.getTime())) return <span className="muted">—</span>;
  const age = sessionAge(created, state, now);
  const c = copy.sessions.time;
  const tip = (
    <div>
      <div>{c.created(fullTime(t))}</div>
      <div>{age === null ? c.ended : c.running(duration(age))}</div>
    </div>
  );
  return (
    <Tooltip title={tip} placement="topLeft">
      <span className="session-time" tabIndex={0}>
        <time dateTime={t.toISOString()}>{compactTime(t, new Date(now))}</time>
        {age !== null && <span className="session-age">{duration(age)}</span>}
      </span>
    </Tooltip>
  );
}
