'use client';

import { Fragment } from 'react';
import { Tooltip } from 'antd';
import { compactTimeParts, copy, duration, fullTime } from '@/lib/copy';
import { sessionAge } from '@/lib/view';

/**
 * Time column: when the session was created (compact, local) and how long it has existed.
 * `now` ticks once a minute from the caller; the tooltip has the full timestamps.
 * The date and the clock time are separate parts so a narrow column wraps between them.
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
    <Tooltip title={tip} placement="topLeft" trigger={['hover', 'focus']}>
      {/* A mouse click must not focus the span: the row's modal would refocus it on
          close and the focus trigger would reopen the tooltip. Tab still focuses it. */}
      <span className="session-time" tabIndex={0} onMouseDown={(e) => e.preventDefault()}>
        <time dateTime={t.toISOString()}>
          {/* The ", " sits between the nowrap parts: the only place the line may break. */}
          {compactTimeParts(t, new Date(now)).map((p, i) => (
            <Fragment key={i}>
              {i > 0 && ', '}
              <span className="session-time-part">{p}</span>
            </Fragment>
          ))}
        </time>
        {age !== null && <span className="session-age">{duration(age)}</span>}
      </span>
    </Tooltip>
  );
}
