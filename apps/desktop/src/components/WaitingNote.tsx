'use client';

import { useState } from 'react';
import { Button, Popover } from 'antd';
import { compactTime, copy, fullTime } from '@/lib/copy';
import type { WaitingGroup, WaitingItem } from '@/lib/view';
import { useNav } from '@/stores/nav';
import { targetOf, useReview } from '@/stores/review';
import { Inline } from './Inline';
import { CopyCommand } from './CopyCommand';

/** Repository attribution and quiet auto-apply progress share the same cached replies. */
export function WaitingNote({
  groups,
  stale = false,
}: {
  groups: WaitingGroup[];
  stale?: boolean;
}) {
  const openSession = useNav((s) => s.openSession);
  const openReview = useReview((s) => s.open);
  const [open, setOpen] = useState(false);
  const count = groups.reduce((n, g) => n + g.items.length, 0);
  const applying = groups.filter((g) => g.applying.length > 0);
  const w = copy.waiting;
  const now = new Date();
  const sessionLink = (id: string, label: string) => (
    <Button
      type="link"
      size="small"
      key={id}
      onClick={() => {
        setOpen(false);
        openSession(id);
      }}
    >
      {label}
    </Button>
  );
  const item = (it: WaitingItem) => (
    <li key={it.key}>
      <div className="waiting-head">
        <span className="waiting-label">
          {it.kind === 'deps'
            ? `Deps #${it.number} · `
            : `${it.number || ''} ${it.number === 1 ? 'conflict' : 'conflicts'} · `}
          {it.kind === 'deps'
            ? it.session
              ? sessionLink(it.session, it.session)
              : 'from main'
            : "needs the session's agent"}
        </span>
        {it.since && (
          <time className="muted" dateTime={it.since} title={fullTime(new Date(it.since))}>
            {compactTime(new Date(it.since), now)}
          </time>
        )}
      </div>
      {it.summary && (
        <p className="waiting-summary" title={it.summary}>
          {it.summary}
        </p>
      )}
      {it.summarySince === 'session_base' && <span className="muted">since session base</span>}
      <div className="waiting-how muted">
        {it.reason ??
          (it.kind === 'deps' && it.mode !== 'apply' ? (
            <Inline text={w.approve(it.number)} />
          ) : (
            it.progress
          ))}
      </div>
      <div className="waiting-origin">
        {it.affected?.map((s) => sessionLink(s.id, `${s.ticket ?? 'No ticket'} · ${s.id}`))}
        {it.command ? (
          <CopyCommand command={it.command} />
        ) : (
          (it.kind === 'conflict' || it.mode !== 'apply' || it.reason === 'install failed') &&
          targetOf(it) && (
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
          )
        )}
      </div>
    </li>
  );
  const list = (
    <div className="waiting-list">
      {stale && <p className="waiting-stale">{w.stale}</p>}
      {groups
        .filter((g) => g.items.length > 0)
        .map((g) => (
          <section className="waiting-group" key={g.repo}>
            <h3>{g.repo}</h3>
            <ul>{g.items.map(item)}</ul>
          </section>
        ))}
    </div>
  );
  return (
    <>
      {count > 0 && (
        <Popover
          content={list}
          title={w.title}
          trigger="click"
          placement="bottomLeft"
          open={open}
          onOpenChange={setOpen}
        >
          <button type="button" className="waiting-note" aria-label={w.show(count)}>
            <span className="dot" data-state="down" aria-hidden />
            {copy.nav.waiting(count)}
          </button>
        </Popover>
      )}
      {applying.length > 0 && (
        <details className="applying-note">
          <summary>
            Applying from main · {applying.reduce((n, g) => n + g.applying.length, 0)}{' '}
            {stale && '(last known)'}
          </summary>
          <div className="applying-list panel">
            {applying.map((g) => (
              <section className="waiting-group" key={g.repo}>
                <h3>{g.repo}</h3>
                <ul>{g.applying.map(item)}</ul>
              </section>
            ))}
          </div>
        </details>
      )}
    </>
  );
}
