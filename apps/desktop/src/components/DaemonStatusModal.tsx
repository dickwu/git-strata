'use client';

import { useEffect, useId, useRef, type ReactNode } from 'react';
import { Descriptions, Modal, Table, Tag, Tooltip } from 'antd';
import type { UseQueryResult } from '@tanstack/react-query';
import { ago, bytes } from '@/lib/copy';
import { daemonCopy as t } from '@/lib/daemonCopy';
import {
  activityView,
  age,
  count,
  duration,
  isLive,
  rawJson,
  savingsView,
  scanText,
  scanView,
  sectionLanding,
  statusFacts,
  storageView,
  times,
  warningSev,
  when,
  type ActivityView,
  type SavingsRow,
  type SavingsView,
  type ScanView,
  type StorageView,
} from '@/lib/daemonPanel';
import { useSocket } from '@/lib/queries';
import type { Chip, ChipTarget, WarningView } from '@/lib/statusBar';
import type { DaemonStatus, DuReply, StatusReply } from '@/lib/types';
import { daemonView } from '@/lib/view';
import { ErrorNote } from './ErrorNote';
import css from './DaemonStatusModal.module.css';

/** Where a status-bar chip opens the modal (statusBar.ts `ChipTarget`). */
export type DaemonModalSection = Extract<ChipTarget, { kind: 'modal' }>['section'];

/** Session id → ticket (agent text: plain text only). */
type Tickets = ReadonlyMap<string, string>;

const ticketsOf = (sessions: StatusReply | undefined): Tickets =>
  new Map(
    (sessions?.sessions ?? []).flatMap((e) =>
      e.status.manifest.ticket ? [[e.status.manifest.id, e.status.manifest.ticket] as const] : []
    )
  );

/** A field this daemon did not send; while it is unreachable nothing was sent, so just a dash. */
function Missing({ live }: { live: boolean }) {
  return <span className="muted">{live ? t.status.notReported : '—'}</span>;
}

/** A session by its ticket when it has one, else its id; the id stays in the tooltip. */
function SessionName({ id, tickets }: { id: string; tickets: Tickets }) {
  return <span title={id}>{tickets.get(id) ?? <span className="mono">{id}</span>}</span>;
}

/** A titled block, labelled by its heading. */
function Section({ title, children }: { title: string; children: ReactNode }) {
  const id = useId();
  return (
    <section aria-labelledby={id}>
      <h2 id={id} className={css.h}>
        {title}
      </h2>
      {children}
    </section>
  );
}

/**
 * A place a chip can open the modal at: a direct child of the modal body (the positioned scroll
 * box), which scrolls to it (`DaemonStatusContent`).
 */
function Target({ name, children }: { name: DaemonModalSection; children: ReactNode }) {
  return (
    <div data-section={name} className={css.target}>
      {children}
    </div>
  );
}

/** Every status-bar chip, the ones past the bar's "+N" included; text and tooltip as plain text. */
export function NowList({ chips }: { chips: readonly Chip[] }) {
  return (
    <ul className={css.list}>
      {chips.map((c) => (
        <li key={c.id} className={css.item}>
          <span className={css.dot} data-sev={c.sev} aria-hidden />
          <div className={css.itemBody}>
            <div className={css.sev} data-sev={c.sev}>
              {c.text}
            </div>
            {c.tooltip && <div className={css.sub}>{c.tooltip}</div>}
          </div>
        </li>
      ))}
    </ul>
  );
}

/**
 * Classified warnings: one line each in its severity colour (red only for errors, never for a
 * LongHold), the raw entry only behind Details, in a neutral colour.
 */
export function WarningList({ items }: { items: readonly WarningView[] }) {
  if (items.length === 0) return <p className={css.note}>{t.warnings.none}</p>;
  return (
    <ul className={css.list}>
      {items.map((w, i) => {
        const sev = warningSev(w);
        return (
          <li key={i} className={css.item} data-kind={w.kind}>
            <span className={css.dot} data-sev={sev} aria-hidden />
            <div className={css.itemBody}>
              <div className={css.sev} data-sev={sev}>
                {w.text}
              </div>
              <details className={css.details}>
                <summary>{t.warnings.details}</summary>
                <pre className={css.raw}>{rawJson(w.raw)}</pre>
              </details>
            </div>
          </li>
        );
      })}
    </ul>
  );
}

/**
 * Repo locks and session queues as plain facts; the daemon's own work is named, not flagged.
 * `daemonWork`: a warning says the daemon is at work (an activity chip's source), which the
 * locks, sampled at another moment, may no longer show.
 */
export function ActivityBlock({
  v,
  tickets,
  daemonWork = false,
}: {
  v: ActivityView;
  tickets: Tickets;
  daemonWork?: boolean;
}) {
  const a = t.activity;
  const c = a.columns;
  const elapsed = (ms: number | null) => (ms === null ? a.none : duration(ms));
  const session = (id: string | null) =>
    id === null ? a.none : <SessionName id={id} tickets={tickets} />;
  return (
    <div>
      {v.holds.length > 0 && (
        <>
          <h3 className={css.h3}>{a.holds}</h3>
          <table className={css.lockTable}>
            <thead>
              <tr>
                <th scope="col">{c.repo}</th>
                <th scope="col">{c.heldBy}</th>
                <th scope="col">{c.session}</th>
                <th scope="col">{c.elapsed}</th>
              </tr>
            </thead>
            <tbody>
              {v.holds.map((h, i) => (
                <tr key={i} data-background={h.background}>
                  <td className="mono">{h.repo}</td>
                  <td title={h.method}>{a.holder(h.method, h.background)}</td>
                  <td>{session(h.session)}</td>
                  <td>{elapsed(h.elapsedMs)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
      {v.queues.length > 0 && (
        <>
          <h3 className={css.h3}>{a.queues}</h3>
          <table className={css.lockTable}>
            <thead>
              <tr>
                <th scope="col">{c.session}</th>
                <th scope="col">{c.heldBy}</th>
                <th scope="col">{c.waiting}</th>
                <th scope="col">{c.elapsed}</th>
              </tr>
            </thead>
            <tbody>
              {v.queues.map((q, i) => (
                <tr key={i} data-background={q.background}>
                  <td>{session(q.session)}</td>
                  <td title={q.holder ?? undefined}>
                    {q.holder === null ? a.none : a.holder(q.holder, q.background)}
                  </td>
                  <td>{q.queued.length > 0 ? q.queued.join(', ') : a.none}</td>
                  <td>{elapsed(q.elapsedMs)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
      {v.waiting.length > 0 && (
        <p className={css.note}>
          {a.waiting(v.waiting.map((w) => a.waitingRepo(w.repo, w.n)).join(', '))}
        </p>
      )}
      {(daemonWork || [...v.holds, ...v.queues].some((r) => r.background)) && (
        <p className={css.formula}>{a.background}</p>
      )}
    </div>
  );
}

/** "git-strata alone holds at least 43 GB" with its caveat; never the allocated sum. */
export function StorageHeadline({ v }: { v: StorageView }) {
  const s = t.storage;
  const h = v.headline;
  if (h.bytes === null)
    return (
      <Tooltip title={s.unavailableTip}>
        <span className={css.headline}>{s.headlineUnavailable}</span>
      </Tooltip>
    );
  return (
    <span>
      <Tooltip title={s.headlineTip}>
        <span className={css.headline}>{s.headline(bytes(h.bytes))}</span>
      </Tooltip>
      {h.state === 'measuring' && <span className="muted"> {s.measuring}</span>}
      {h.asOf && <span className="muted">, {s.asOf(when(h.asOf))}</span>}
    </span>
  );
}

/** Storage (design §2.3): private lower bounds by group, trash gc may empty, allocated hidden. */
export function StorageBlock({ v }: { v: StorageView }) {
  const s = t.storage;
  const trash = v.trash;
  return (
    <>
      <p className={css.lead}>
        <StorageHeadline v={v} />
      </p>
      {v.groups.length > 0 && (
        <>
          <h3 className={css.h3}>{s.groupsHeading}</h3>
          <table className={css.groupTable}>
            <tbody>
              {v.groups.map(({ key, fig }) => (
                <tr key={key}>
                  <th scope="row">{s.groups[key]}</th>
                  <td className={css.num}>
                    {fig.bytes === null ? s.unavailable : bytes(fig.bytes)}
                  </td>
                  <td className="muted">{fig.state === 'measuring' ? s.measuring : ''}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
      {trash && trash.eligible > 0 && (
        <p className={css.note}>
          {s.trashEligible(trash.eligible, age(trash.floorSecs))}{' '}
          <code className="inline-code">{trash.gc}</code> {s.trashHuman}
        </p>
      )}
      {trash && trash.young > 0 && (
        <p className={css.note}>{s.trashYoung(trash.young, age(trash.floorSecs))}</p>
      )}
      {(v.allocated !== null || v.logical !== null) && (
        <details className={css.details}>
          <summary>{s.details}</summary>
          <dl className={css.dl}>
            {v.allocated !== null && (
              <>
                <dt>{s.allocated}</dt>
                <dd>{bytes(v.allocated)}</dd>
              </>
            )}
            {v.logical !== null && (
              <>
                <dt>{s.logical}</dt>
                <dd>{bytes(v.logical)}</dd>
              </>
            )}
          </dl>
        </details>
      )}
    </>
  );
}

/** The size scan: counts only; failures as plain history, never a chip on 0.2.2 (design §11.4). */
export function ScanBlock({ v }: { v: ScanView }) {
  const s = t.scan;
  return (
    <>
      <p className={css.lead}>{scanText(v)}</p>
      {v.failed.length > 0 && (
        <>
          <h3 className={css.h3}>{s.failed}</h3>
          <ul className={css.failed}>
            {v.failed.map((f) => (
              <li key={`${f.unit}\n${f.error}`}>
                <span className="mono">{f.unit}</span>: {f.error}
                {f.times > 1 && <span className="muted"> {s.times(f.times)}</span>}
              </li>
            ))}
          </ul>
          <p className={css.formula}>{s.failedNote}</p>
        </>
      )}
    </>
  );
}

function Savings({ v, tickets }: { v: SavingsView; tickets: Tickets }) {
  const s = t.savings;
  const note = s.cumulativeNote(v.cumulative.removed, v.cumulative.unsized);
  const untimedTip = v.timingMissing === 'old-daemon' ? s.notTimedTipOld : s.notTimedTip;
  const asOf = [
    v.live.asOf ? s.asOf(when(v.live.asOf)) : null,
    v.live.complete ? null : s.measuringMark,
  ].filter((p) => p !== null);
  const since = when(v.cumulative.since);
  // A row whose figures are inexact has neither bound (daemonPanel.ts, Accuracy).
  const unavailable = (
    <Tooltip title={s.inexactRow} placement="topRight">
      <span className="muted">{t.storage.unavailable}</span>
    </Tooltip>
  );
  return (
    <section className={css.savings} aria-labelledby="savings-h">
      <h2 id="savings-h" className={css.h}>
        {s.heading}
      </h2>
      <p className={css.intro}>{s.intro}</p>
      <div className={css.hero}>
        {v.live.saved === null ? (
          <span className={css.heroNone}>{s.liveUnavailable(v.live.sessions)}</span>
        ) : (
          <>
            <span className={css.heroPre}>{s.upTo}</span>
            <span className={css.heroNum}>{bytes(v.live.saved)}</span>
            <span className={css.heroCap}>{s.live(v.live.sessions)}</span>
          </>
        )}
      </div>
      {asOf.length > 0 && <p className={css.asOf}>{asOf.join(' ')}</p>}
      <ul className={css.facts}>
        <li>
          {v.live.onDisk === null
            ? s.heldAloneUnavailable(bytes(v.live.logical))
            : s.heldAlone(bytes(v.live.onDisk), bytes(v.live.logical))}
        </li>
        {v.live.files !== null && <li>{s.files(count(v.live.files))}</li>}
        <li>
          {v.cumulative.saved === null
            ? s.cumulativeUnavailable(v.cumulative.sessions, since)
            : s.cumulative(bytes(v.cumulative.saved), v.cumulative.sessions, since)}
          {note && <span className="muted"> ({note})</span>}
        </li>
      </ul>
      {v.inexact && <p className={css.note}>{s.inexact}</p>}
      {!v.live.complete && <p className={css.note}>{s.measuring}</p>}

      <h3 className={css.h3}>{s.timeHeading}</h3>
      {v.timing ? (
        <div className={css.race}>
          <div>
            <div className={css.raceLabel}>{s.measured}</div>
            <div className={css.raceValue}>
              {s.measuredValue(duration(v.timing.avgCreateMs), v.timing.timed)}
            </div>
          </div>
          <div>
            <div className={css.raceLabel}>{s.estimated}</div>
            <div className={css.raceValue}>
              {s.estimatedValue(duration(v.timing.copyEstimateMs))}
            </div>
          </div>
          {v.timing.speedup !== null && (
            <div className={css.speedup}>
              {s.speedup(times(v.timing.speedup))} <span className="muted">{s.speedupNote}</span>
            </div>
          )}
        </div>
      ) : (
        <p className={css.note}>{v.timingMissing === 'old-daemon' ? s.timingOld : s.timingNone}</p>
      )}
      <p className={css.note}>{s.liveEstimate(duration(v.live.copyEstimateMs))}</p>
      <p className={css.formula}>{s.formula}</p>

      {v.rows.length > 0 && (
        <Table<SavingsRow>
          className={css.table}
          size="small"
          pagination={false}
          rowKey="session"
          dataSource={v.rows}
          columns={[
            {
              title: s.columns.session,
              dataIndex: 'session',
              render: (id: string) => <SessionName id={id} tickets={tickets} />,
            },
            { title: s.columns.logical, dataIndex: 'logical', align: 'right', render: bytes },
            {
              title: s.columns.heldAlone,
              dataIndex: 'onDisk',
              align: 'right',
              render: (b: number | null) => (b === null ? unavailable : bytes(b)),
            },
            {
              title: s.columns.saved,
              dataIndex: 'saved',
              align: 'right',
              render: (b: number | null) =>
                b === null ? unavailable : <strong>{bytes(b)}</strong>,
            },
            {
              title: s.columns.created,
              dataIndex: 'createMs',
              align: 'right',
              render: (ms: number | null) =>
                ms === null ? (
                  <Tooltip title={untimedTip} placement="topRight">
                    <span className={`muted ${css.untimed}`} aria-label={untimedTip}>
                      {s.notTimed}
                    </span>
                  </Tooltip>
                ) : (
                  duration(ms)
                ),
            },
            {
              title: s.columns.estimate,
              dataIndex: 'copyEstimateMs',
              align: 'right',
              render: (ms: number) => <span className="muted">≥ {duration(ms)}</span>,
            },
          ]}
        />
      )}
      {v.untimed > 0 && v.timingMissing !== 'old-daemon' && (
        <p className={css.formula}>{s.notTimedNote(v.untimed)}</p>
      )}
      <p className={css.formula}>{s.accuracy}</p>
    </section>
  );
}

export interface DaemonStatusContentProps {
  status: UseQueryResult<DaemonStatus>;
  du: UseQueryResult<DuReply>;
  sessions: StatusReply | undefined;
  now: number;
  /** The daemon socket path, once known. */
  socket: string | undefined;
  /** Scroll this section into view when the modal opens. */
  section?: DaemonModalSection;
  /** The status bar's chips, overflow included; listed under "Now" when non-empty. */
  chips?: readonly Chip[];
}

/** The modal's body. No data hooks, so it renders on its own (tests render it directly). */
export function DaemonStatusContent({
  status,
  du,
  sessions,
  now,
  socket,
  section,
  chips = [],
}: DaemonStatusContentProps) {
  const live = isLive(status);
  // Warnings and activity only from a live answer: a stale hold would keep "running".
  const data = live ? status.data : undefined;
  const f = status.data ? statusFacts(status.data, now) : null;
  const warnings = data ? daemonView(data, now).warnings : [];
  const activity = data ? activityView(data, now) : null;
  // An activity chip opens the Activity section, so it is there whenever a warning says the
  // daemon is at work, even if the locks (sampled at another moment) show nothing.
  const daemonWork = warnings.some((w) => w.cls === 'activity');
  const hasActivity = activity
    ? daemonWork || activity.holds.length + activity.queues.length + activity.waiting.length > 0
    : false;
  const hasStorage = live && (du.data !== undefined || du.isError);
  const v = savingsView(du.data);
  const storage = live ? storageView(du.data) : null;
  const scan = live ? scanView(du.data?.scan) : null;
  const tickets = ticketsOf(sessions);
  const leftovers = Array.isArray(sessions?.leftovers) ? sessions.leftovers.length : null;
  const st = t.status;
  // Open at the chip's section once per opening (the modal unmounts its body on close).
  const body = useRef<HTMLDivElement>(null);
  const landed = useRef(false);
  useEffect(() => {
    const box = body.current;
    if (landed.current || !box) return;
    const children = Array.from(box.children, (el) => ({
      name: (el as HTMLElement).dataset.section,
      offsetTop: (el as HTMLElement).offsetTop,
    }));
    const { top, done } = sectionLanding(children, section, box.scrollTop);
    if (top !== null) box.scrollTop = top;
    landed.current = done;
  });
  return (
    <div ref={body} className={css.body}>
      {status.isError && <ErrorNote error={status.error} />}
      {chips.length > 0 && (
        <Section title={t.now.heading}>
          <NowList chips={chips} />
        </Section>
      )}
      {warnings.length > 0 && (
        <Target name="warnings">
          <Section title={t.warnings.heading}>
            <WarningList items={warnings} />
          </Section>
        </Target>
      )}
      {activity && hasActivity && (
        <Target name="activity">
          <Section title={t.activity.heading}>
            <ActivityBlock v={activity} tickets={tickets} daemonWork={daemonWork} />
          </Section>
        </Target>
      )}
      {hasStorage && (
        <Target name="storage">
          {v && <Savings v={v} tickets={tickets} />}
          {!v && du.isSuccess && <p className={css.note}>{t.savings.unavailable}</p>}
          {du.isError && <ErrorNote error={du.error} />}
          {storage && (
            <Section title={t.storage.heading}>
              <StorageBlock v={storage} />
            </Section>
          )}
        </Target>
      )}
      {scan && (
        <Target name="scan">
          <Section title={t.scan.heading}>
            <ScanBlock v={scan} />
          </Section>
        </Target>
      )}
      <Target name="daemon">
        <Section title={st.heading}>
          <Descriptions column={2} size="small" bordered className={css.desc}>
            <Descriptions.Item label={st.state}>
              <span className={css.state}>
                <span className="dot" data-state={live ? 'up' : 'down'} />
                {live ? t.modal.running : t.modal.down}
              </span>
            </Descriptions.Item>
            <Descriptions.Item label={st.version}>
              {f?.version ? (
                <span className="mono">
                  {f.version}
                  {f.api !== null && ` (api ${f.api})`}
                </span>
              ) : (
                <Missing live={live} />
              )}
            </Descriptions.Item>
            <Descriptions.Item label={st.pid}>
              {f?.pid != null ? <span className="mono">{f.pid}</span> : <Missing live={live} />}
            </Descriptions.Item>
            <Descriptions.Item label={st.uptime}>
              {f?.uptimeMs != null ? duration(f.uptimeMs) : <Missing live={live} />}
            </Descriptions.Item>
            <Descriptions.Item label={st.sessions}>
              {sessions ? sessions.sessions.length : '—'}
            </Descriptions.Item>
            <Descriptions.Item
              label={
                <Tooltip title={st.leftoversTip}>
                  <span>{st.leftovers}</span>
                </Tooltip>
              }
            >
              {leftovers ?? '—'}
            </Descriptions.Item>
            <Descriptions.Item label={st.fds} span={2}>
              {f?.fds ? (
                <span>
                  {st.fdsValue(count(f.fds.open), count(f.fds.limit))}
                  <span className="muted">, {st.acceptErrors(f.fds.acceptErrors)}</span>
                </span>
              ) : (
                <Missing live={live} />
              )}
            </Descriptions.Item>
            {status.data?.config && (
              <Descriptions.Item label="Configuration" span={2}>
                <div>Generation {status.data.config.generation}</div>
                <div className="mono">{status.data.config.hash}</div>
                <pre>
                  {JSON.stringify(
                    {
                      loaded_at: status.data.config.loaded_at,
                      last_attempt: status.data.config.last_attempt,
                      restart_required: status.data.config.restart_required,
                    },
                    null,
                    2
                  )}
                </pre>
              </Descriptions.Item>
            )}
            <Descriptions.Item label={st.reconcile}>{f ? f.reconcileRuns : '—'}</Descriptions.Item>
            <Descriptions.Item label={st.holds}>{f ? f.holds : '—'}</Descriptions.Item>
            <Descriptions.Item label={st.waiting}>{f ? f.waiting : '—'}</Descriptions.Item>
            <Descriptions.Item label={st.warnings}>{f ? f.warnings : '—'}</Descriptions.Item>
            <Descriptions.Item label={st.socket} span={2}>
              <span className="mono">{socket ?? '—'}</span>
            </Descriptions.Item>
          </Descriptions>
        </Section>
      </Target>
    </div>
  );
}

export function DaemonStatusModal({
  open,
  onClose,
  status,
  du,
  sessions,
  now,
  section,
  chips,
}: {
  open: boolean;
  onClose: () => void;
  status: UseQueryResult<DaemonStatus>;
  du: UseQueryResult<DuReply>;
  sessions: StatusReply | undefined;
  now: number;
  /** Open scrolled to this section (a chip's target). */
  section?: DaemonModalSection;
  /** Every current status-bar chip, overflow included. */
  chips?: readonly Chip[];
}) {
  const socket = useSocket();
  const live = isLive(status);
  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      width={760}
      destroyOnHidden
      title={
        <span className={css.title}>
          {t.modal.title}
          <Tag color={live ? 'success' : 'error'}>{live ? t.modal.running : t.modal.down}</Tag>
          {live && (
            <span className={css.updated}>{t.modal.updated(ago(now - status.dataUpdatedAt))}</span>
          )}
        </span>
      }
    >
      <DaemonStatusContent
        status={status}
        du={du}
        sessions={sessions}
        now={now}
        socket={socket.data}
        section={section}
        chips={chips}
      />
    </Modal>
  );
}
