'use client';

import { useState } from 'react';
import { Button, Popover } from 'antd';
import { compactTime, copy, fullTime } from '@/lib/copy';
import type { WaitingItem } from '@/lib/view';
import { useNav } from '@/stores/nav';
import { targetOf, useReview } from '@/stores/review';
import { Inline } from './Inline';

/**
 * The Sessions header's "N waiting for you". The count alone gave no way to see what was
 * waiting, so it read as stuck; it now opens the list the tray shows (t110u).
 */
export function WaitingNote({ items, stale = false }: { items: WaitingItem[]; stale?: boolean }) {
  const openSession = useNav((s) => s.openSession);
  const openReview = useReview((s) => s.open);
  const [open, setOpen] = useState(false);
  if (items.length === 0) return null;
  const w = copy.waiting;
  const now = new Date();
  const list = (
    <div className="waiting-list">
      {stale && <p className="waiting-stale">{w.stale}</p>}
      <ul>
        {items.map((it) => (
          <li key={it.key}>
            <div className="waiting-head">
              <span className="waiting-label">
                {it.kind === 'deps' ? w.deps(it.number, it.repo) : w.conflict(it.number, it.repo)}
              </span>
              {it.since && (
                <time className="muted" dateTime={it.since} title={fullTime(new Date(it.since))}>
                  {compactTime(new Date(it.since), now)}
                </time>
              )}
            </div>
            <div className="waiting-how muted">
              {it.kind === 'deps' ? <Inline text={w.approve(it.number)} /> : w.resolve}
            </div>
            <div className="waiting-origin">
              {it.session ? (
                <Button
                  type="link"
                  size="small"
                  onClick={() => {
                    setOpen(false);
                    openSession(it.session);
                  }}
                >
                  {w.open(it.session)}
                </Button>
              ) : (
                <span className="muted">{w.fromMain}</span>
              )}
              {targetOf(it) && (
                <Button
                  size="small"
                  className="waiting-review"
                  aria-label={w.reviewLabel(
                    it.kind === 'deps' ? w.deps(it.number, it.repo) : w.conflict(it.number, it.repo)
                  )}
                  onClick={() => {
                    const t = targetOf(it);
                    setOpen(false);
                    if (t) openReview(t);
                  }}
                >
                  {w.review}
                </Button>
              )}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
  return (
    <Popover
      content={list}
      title={w.title}
      trigger="click"
      placement="bottomLeft"
      open={open}
      onOpenChange={setOpen}
    >
      <button type="button" className="waiting-note" aria-label={w.show(items.length)}>
        <span className="dot" data-state="down" aria-hidden />
        {copy.nav.waiting(items.length)}
      </button>
    </Popover>
  );
}
