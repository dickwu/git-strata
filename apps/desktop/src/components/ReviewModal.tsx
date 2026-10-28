'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Alert, Button, Modal, Spin, Tag } from 'antd';
import { useQueryClient, type QueryKey } from '@tanstack/react-query';
import {
  REFRESH_AFTER_APPROVE,
  STILL_WORKING_MS,
  inputsChanged,
  verdict,
  type Outcome,
  type Verdict,
} from '@/lib/approve';
import { refreshAfter } from '@/lib/refresh';
import { compactTime, copy, fullTime } from '@/lib/copy';
import { useDeps, useSession, useSessions } from '@/lib/queries';
import { approveProposal, asGuiError, reason } from '@/lib/tauri';
import { short } from '@/lib/view';
import type { Proposal } from '@/lib/types';
import { useNav } from '@/stores/nav';
import { targetKey, useReview, type ReviewTarget } from '@/stores/review';
import { errorText } from './ErrorNote';
import css from './ReviewModal.module.css';

const t = copy.review;
/** The modal re-reads while open (design §4.5: deps polls every 10 s while visible). */
const POLL_MS = 10_000;
const DEPS_KEYS: QueryKey[] = [['deps'], ['sessions']];

/**
 * Re-read before showing anything (gui.md:320). Joins a read already in flight instead of
 * cancelling it (refreshAfter). `since` is Infinity until the reads that began before the
 * modal opened have ended; a read landing at or after `since` began after the open.
 */
function useFreshSince(keys: QueryKey[]): [number, () => void] {
  const qc = useQueryClient();
  const [since, setSince] = useState(Infinity);
  const live = useRef(true);
  const asked = useRef(0);
  const reread = useCallback(() => {
    const n = ++asked.current;
    setSince(Infinity);
    void refreshAfter(qc, keys).then((t) => {
      if (live.current && n === asked.current) setSince(t);
    });
  }, [qc, keys]);
  useEffect(() => {
    live.current = true;
    reread();
    return () => {
      live.current = false;
    };
  }, [reread]);
  return [since, reread];
}

function useCopy(): [string | null, (text: string, key: string) => void] {
  const [done, setDone] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    []
  );
  const run = useCallback((text: string, key: string) => {
    const ok = () => setDone(key);
    const fail = () => setDone(`!${key}`);
    try {
      void navigator.clipboard.writeText(text).then(ok, fail);
    } catch {
      fail();
    }
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setDone(null), 1600);
  }, []);
  return [done, run];
}

function copyLabel(done: string | null, key: string, idle: string): string {
  if (done === key) return t.copied;
  if (done === `!${key}`) return t.copyFailed;
  return idle;
}

/** A read failure as plain text: no Start daemon button inside a review (that acts on the service). */
function ReadError({ error }: { error: unknown }) {
  return <Alert type="warning" showIcon title={errorText(error).title} />;
}

function Banner({ tone, title, notes }: { tone: string; title: string; notes?: string[] }) {
  return (
    <div className={css.banner} data-tone={tone} role="status">
      <div className={css.bannerTitle}>{title}</div>
      {notes?.map((n) => (
        <p key={n} className={css.bannerNote}>
          {n}
        </p>
      ))}
    </div>
  );
}

function Origin({ p }: { p: Proposal }) {
  const when = p.created ? new Date(p.created) : null;
  return (
    <p className={css.origin}>
      {p.origin?.kind === 'Session' && p.origin.session
        ? t.fromSession(p.origin.session)
        : t.fromMain}
      {when && (
        <>
          {', '}
          {t.proposed}{' '}
          <time dateTime={p.created} title={fullTime(when)}>
            {compactTime(when, new Date())}
          </time>
        </>
      )}
    </p>
  );
}

function Facts({ p }: { p: Proposal }) {
  const changed = inputsChanged(p);
  return (
    <dl className={css.facts}>
      {p.head && (
        <>
          <dt>{t.mainAt}</dt>
          <dd>
            <span className="mono" title={p.head}>
              {short(p.head)}
            </span>
          </dd>
        </>
      )}
      <dt>{t.deps}</dt>
      <dd>
        <span className="mono" title={p.from}>
          {short(p.from, 8)}
        </span>
        <span className={css.arrow} aria-hidden>
          {' → '}
        </span>
        <span className="mono" title={p.to}>
          {short(p.to, 8)}
        </span>
        {changed !== null && (
          <span className={css.inputs}>{changed ? t.inputsChanged : t.inputsSame}</span>
        )}
      </dd>
    </dl>
  );
}

function Rows({ v, onOpen }: { v: Verdict; onOpen: (s: string) => void }) {
  if (v.rows.length === 0) return null;
  return (
    <ul className={css.rows} aria-label={t.result.heading}>
      {v.rows.map((row) => (
        <li key={row.session} data-state={row.state}>
          <span className={css.rowState}>{row.label}</span>
          <Button
            type="link"
            size="small"
            className={css.rowSession}
            onClick={() => onOpen(row.session)}
          >
            <span className="mono">{row.session}</span>
          </Button>
          {row.conflicts > 0 && (
            <span className={css.rowConflicts}>{t.result.conflicts(row.conflicts)}</span>
          )}
          {row.detail && <span className={css.rowDetail}>{row.detail}</span>}
        </li>
      ))}
    </ul>
  );
}

type Phase =
  { kind: 'idle' } | { kind: 'confirming'; slow: boolean } | { kind: 'result'; v: Verdict };

/** An approve outcome: the headline banner and one line per session. */
export function ResultView({ v, onOpen }: { v: Verdict; onOpen: (s: string) => void }) {
  return (
    <>
      <Banner tone={v.tone} title={v.title} notes={v.notes} />
      <Rows v={v} onOpen={onOpen} />
    </>
  );
}

function DepsReview({
  target,
  onBusy,
}: {
  target: Extract<ReviewTarget, { kind: 'deps' }>;
  onBusy: (busy: boolean) => void;
}) {
  const qc = useQueryClient();
  const deps = useDeps(POLL_MS);
  const [since, reread] = useFreshSince(DEPS_KEYS);
  const { open, close, approving, finished, readyAt, start, markSeen } = useReview();
  const openSession = useNav((s) => s.openSession);
  const [phase, setPhase] = useState<Phase>({ kind: 'idle' });
  const [done, copyText] = useCopy();
  const slow = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (slow.current) clearTimeout(slow.current);
      onBusy(false);
    },
    [onBusy]
  );

  // A read refused as request_in_flight is another read finishing; the poll picks it up.
  const readError =
    deps.isError && reason(asGuiError(deps.error)) !== 'request_in_flight' ? deps.error : null;
  // Fresh: read after the modal opened and after the last approval ended (readyAt).
  const fresh = deps.dataUpdatedAt >= Math.max(since, readyAt) && !deps.isError;
  const p = deps.data?.proposals.find((x) => x.id === target.id) ?? null;
  const pending = p?.state?.kind === 'Pending';
  const other =
    deps.data?.proposals.find(
      (x) => x.repo === target.repo && x.id !== target.id && x.state?.kind === 'Pending'
    ) ?? null;
  const unseen = finished !== null && !finished.seen && finished.id === target.id;
  const canApprove = phase.kind === 'idle' && fresh && pending && approving === null && !unseen;

  // The outcome of this proposal's call, whichever modal started it: this one (confirming),
  // this one after Stop waiting, or an earlier one closed while the call was out (§4.4).
  useEffect(() => {
    if (!unseen || !finished) return;
    if (slow.current) clearTimeout(slow.current);
    markSeen();
    setPhase({ kind: 'result', v: verdict(target.id, finished.outcome) });
    onBusy(false);
  }, [unseen, finished, target.id, markSeen, onBusy]);

  // Pointer or keyboard activation of the Approve button only; nothing binds Enter (§4.7).
  const onApprove = () => {
    if (!canApprove) return;
    const id = target.id;
    const sent = start(
      id,
      () =>
        approveProposal(id).then(
          (reply): Outcome => ({ kind: 'reply', reply }),
          (e): Outcome => ({ kind: 'error', error: asGuiError(e) })
        ),
      qc
    );
    if (!sent) return;
    setPhase({ kind: 'confirming', slow: false });
    onBusy(true);
    slow.current = setTimeout(() => setPhase({ kind: 'confirming', slow: true }), STILL_WORKING_MS);
  };

  // Ends the wait, not the call: its outcome replaces this view when it comes (effect above).
  const onStopWaiting = () => {
    void refreshAfter(qc, REFRESH_AFTER_APPROVE);
    setPhase({ kind: 'result', v: verdict(target.id, { kind: 'detached' }) });
    onBusy(false);
  };

  const onAgain = () => {
    setPhase({ kind: 'idle' });
    reread();
  };

  const toSession = (s: string) => {
    close();
    openSession(s);
  };

  const commandButton = (
    <Button onClick={() => copyText(t.command(target.id), 'cmd')}>
      {copyLabel(done, 'cmd', t.copyCommand)}
    </Button>
  );

  let body: React.ReactNode;
  let footer: React.ReactNode;
  if (phase.kind === 'confirming') {
    body = (
      <div className={css.confirming} aria-live="polite">
        <span className={css.pulse} aria-hidden />
        <div>
          <div className={css.confirmingTitle}>{t.confirming(target.id)}</div>
          <p className={css.note}>{t.confirmingNote}</p>
          {phase.slow && <p className={css.note}>{t.stillWorking}</p>}
        </div>
      </div>
    );
    footer = phase.slow ? <Button onClick={onStopWaiting}>{t.stopWaiting}</Button> : null;
  } else if (phase.kind === 'result') {
    const v = phase.v;
    body = <ResultView v={v} onOpen={toSession} />;
    footer = (
      <>
        {v.replacement !== null && (
          <Button onClick={() => open({ kind: 'deps', id: v.replacement!, repo: target.repo })}>
            {t.reviewOther(v.replacement)}
          </Button>
        )}
        {v.again && <Button onClick={onAgain}>{t.reviewAgain}</Button>}
        <Button onClick={close}>{t.close}</Button>
      </>
    );
  } else if (readError) {
    body = (
      <>
        <ReadError error={readError} />
        <p className={css.note}>{t.readFailed}</p>
        <p className={css.terminal}>
          {t.terminal} <code className="mono">{t.command(target.id)}</code>
        </p>
      </>
    );
    footer = (
      <>
        {commandButton}
        <Button onClick={close}>{t.close}</Button>
      </>
    );
  } else if (!fresh) {
    body = (
      <div className={css.loading}>
        <Spin size="small" /> <span className="muted">{t.loading}</span>
      </div>
    );
    footer = <Button onClick={close}>{t.close}</Button>;
  } else if (!p || !pending) {
    body = (
      <>
        <Banner tone="neutral" title={t.gone(target.id)} notes={[t.goneNote]} />
        {other && <p className={css.note}>{t.otherPending(other.id, other.repo)}</p>}
      </>
    );
    footer = (
      <>
        {other && (
          <Button onClick={() => open({ kind: 'deps', id: other.id, repo: other.repo })}>
            {t.reviewOther(other.id)}
          </Button>
        )}
        <Button onClick={close}>{t.close}</Button>
      </>
    );
  } else {
    body = (
      <>
        <div className={css.stateLine}>
          <Tag className={css.tag}>Pending</Tag>
        </div>
        <Origin p={p} />
        <Facts p={p} />
        <section className={css.section} aria-labelledby="review-scope">
          <h3 id="review-scope" className={css.h}>
            {t.scopeHeading}
          </h3>
          <p>{t.scope(p.id, p.repo)}</p>
        </section>
        <section className={css.section} aria-labelledby="review-prompt">
          <h3 id="review-prompt" className={css.h}>
            {t.promptHeading}
          </h3>
          <p>{t.prompt(p.id)}</p>
          <p className={css.strong}>{t.promptCheck}</p>
        </section>
        <p className={css.terminal}>
          {t.terminal} <code className="mono">{t.command(p.id)}</code>
        </p>
        {approving !== null && <Banner tone="warning" title={t.result.inFlight} />}
      </>
    );
    footer = (
      <>
        {commandButton}
        <Button onClick={close}>{t.close}</Button>
        <Button type="primary" htmlType="button" disabled={!canApprove} onClick={onApprove}>
          {t.approve}
        </Button>
      </>
    );
  }

  return (
    <div className={css.body}>
      {body}
      {footer && <div className={css.footer}>{footer}</div>}
    </div>
  );
}

function ConflictReview({ target }: { target: Extract<ReviewTarget, { kind: 'conflict' }> }) {
  const one = useSession(target.session, POLL_MS);
  const list = useSessions(POLL_MS);
  const keys = useMemo<QueryKey[]>(
    () => [['sessions'], ['session', target.session]],
    [target.session]
  );
  const [since] = useFreshSince(keys);
  const { close } = useReview();
  const openSession = useNav((s) => s.openSession);
  const [done, copyText] = useCopy();
  // The newest successful read wins: a failed detail refresh must not hide a fresher list.
  const source = [one, list]
    .filter((q) => q.data !== undefined && q.dataUpdatedAt > 0)
    .sort((a, b) => b.dataUpdatedAt - a.dataUpdatedAt)[0];
  const fresh = source !== undefined && source.dataUpdatedAt >= since;
  const entry = fresh
    ? (source.data?.sessions.find((s) => s.status.manifest.id === target.session) ?? null)
    : null;
  const ws = entry?.status.workspaces?.[target.repo];
  const paths = (ws?.conflicts ?? []).map((c) => (typeof c === 'string' ? c : JSON.stringify(c)));
  const root = entry?.status.manifest.repos?.[target.repo]?.path ?? null;
  const full = (rel: string) => (root ? `${root.replace(/\/$/, '')}/${rel}` : rel);
  const readError = (q: typeof one) =>
    q.isError && reason(asGuiError(q.error)) !== 'request_in_flight';
  const error = !fresh && readError(one) && readError(list) ? one.error : null;
  return (
    <div className={css.body}>
      <dl className={css.facts}>
        <dt>{t.conflictSession}</dt>
        <dd className="mono">{target.session}</dd>
        <dt>{t.conflictRepo}</dt>
        <dd className="mono">{target.repo}</dd>
      </dl>
      {error ? (
        <ReadError error={error} />
      ) : !fresh ? (
        <div className={css.loading}>
          <Spin size="small" /> <span className="muted">{t.loading}</span>
        </div>
      ) : paths.length === 0 ? (
        <Banner tone="neutral" title={t.noConflicts(target.repo)} />
      ) : (
        <>
          <p>{t.conflictNote}</p>
          <h3 className={css.h}>{t.conflictPaths(paths.length)}</h3>
          <ul className={css.paths}>
            {paths.map((path) => (
              <li key={path}>
                <span className="mono" title={full(path)}>
                  {path}
                </span>
                <Button size="small" type="text" onClick={() => copyText(full(path), path)}>
                  {copyLabel(done, path, t.copyPath)}
                </Button>
              </li>
            ))}
          </ul>
        </>
      )}
      <div className={css.footer}>
        <Button
          onClick={() => {
            close();
            openSession(target.session);
          }}
        >
          {t.openSession(target.session)}
        </Button>
        <Button onClick={close}>{t.close}</Button>
      </div>
    </div>
  );
}

/**
 * The review modal (gui-approve design §4): a deps proposal with Approve (D1 Option A, D2=b:
 * the daemon prompts and approves the whole proposal), or one workspace's conflicts,
 * read-only (D6). No reject (D3 = R1). While a call is out, the modal cannot be dismissed
 * by mask, Esc or the close icon; Stop waiting (after 70 s) is the only way out.
 */
export function ReviewModal() {
  const target = useReview((s) => s.target);
  const close = useReview((s) => s.close);
  const [busy, setBusy] = useState(false);
  const title = target
    ? target.kind === 'deps'
      ? t.depsTitle(target.id, target.repo)
      : t.conflictTitle(target.repo)
    : '';
  return (
    <Modal
      open={target !== null}
      onCancel={() => {
        if (!busy) close();
      }}
      footer={null}
      width={720}
      destroyOnHidden
      maskClosable={!busy}
      keyboard={!busy}
      closable={!busy}
      title={<span className={css.title}>{title}</span>}
    >
      {target?.kind === 'deps' && (
        <DepsReview key={targetKey(target)} target={target} onBusy={setBusy} />
      )}
      {target?.kind === 'conflict' && <ConflictReview key={targetKey(target)} target={target} />}
    </Modal>
  );
}
