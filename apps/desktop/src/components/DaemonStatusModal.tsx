'use client';

import { Descriptions, Modal, Table, Tag, Tooltip } from 'antd';
import type { UseQueryResult } from '@tanstack/react-query';
import { ago, bytes } from '@/lib/copy';
import { daemonCopy as t } from '@/lib/daemonCopy';
import {
  count,
  duration,
  savingsView,
  statusFacts,
  times,
  type SavingsRow,
  type SavingsView,
} from '@/lib/daemonPanel';
import { useSocket } from '@/lib/queries';
import type { DaemonStatus, DuReply, StatusReply } from '@/lib/types';
import { ErrorNote } from './ErrorNote';
import css from './DaemonStatusModal.module.css';

const when = (iso: string | null | undefined) =>
  iso
    ? new Date(iso).toLocaleString(undefined, {
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      })
    : '—';

/** A field this daemon did not send; while it is unreachable nothing was sent, so just a dash. */
function Missing({ live }: { live: boolean }) {
  return <span className="muted">{live ? t.status.notReported : '—'}</span>;
}

function Savings({ v, tickets }: { v: SavingsView; tickets: Map<string, string> }) {
  const s = t.savings;
  const note = s.cumulativeNote(v.cumulative.removed, v.cumulative.unsized);
  const untimedTip = v.timingMissing === 'old-daemon' ? s.notTimedTipOld : s.notTimedTip;
  return (
    <section className={css.savings} aria-labelledby="savings-h">
      <h2 id="savings-h" className={css.h}>
        {s.heading}
      </h2>
      <p className={css.intro}>{s.intro}</p>
      <div className={css.hero}>
        <span className={css.heroNum}>{bytes(v.live.saved)}</span>
        <span className={css.heroCap}>{s.live(v.live.sessions)}</span>
      </div>
      <ul className={css.facts}>
        <li>{s.onDisk(bytes(v.live.onDisk), bytes(v.live.logical))}</li>
        {v.live.files !== null && <li>{s.files(count(v.live.files))}</li>}
        <li>
          {s.cumulative(bytes(v.cumulative.saved), v.cumulative.sessions, when(v.cumulative.since))}
          {note && <span className="muted"> ({note})</span>}
        </li>
      </ul>
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
              render: (id: string) => (
                <span title={id}>{tickets.get(id) ?? <span className="mono">{id}</span>}</span>
              ),
            },
            { title: s.columns.logical, dataIndex: 'logical', align: 'right', render: bytes },
            { title: s.columns.onDisk, dataIndex: 'onDisk', align: 'right', render: bytes },
            {
              title: s.columns.saved,
              dataIndex: 'saved',
              align: 'right',
              render: (b: number) => <strong>{bytes(b)}</strong>,
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

export function DaemonStatusModal({
  open,
  onClose,
  status,
  du,
  sessions,
  now,
}: {
  open: boolean;
  onClose: () => void;
  status: UseQueryResult<DaemonStatus>;
  du: UseQueryResult<DuReply>;
  sessions: StatusReply | undefined;
  now: number;
}) {
  const socket = useSocket();
  const live = status.isSuccess || (status.data !== undefined && !status.isError);
  const f = status.data ? statusFacts(status.data, now) : null;
  const v = savingsView(du.data);
  const tickets = new Map(
    (sessions?.sessions ?? []).flatMap((e) =>
      e.status.manifest.ticket ? [[e.status.manifest.id, e.status.manifest.ticket] as const] : []
    )
  );
  const st = t.status;
  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      width={760}
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
      <div className={css.body}>
        {status.isError && <ErrorNote error={status.error} />}
        {live && v && <Savings v={v} tickets={tickets} />}
        {live && !v && du.isSuccess && <p className={css.note}>{t.savings.unavailable}</p>}
        {live && du.isError && <ErrorNote error={du.error} />}

        <section aria-labelledby="status-h">
          <h2 id="status-h" className={css.h}>
            {st.heading}
          </h2>
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
            <Descriptions.Item label={st.fds}>
              {f?.fds ? (
                <span>
                  {st.fdsValue(count(f.fds.open), count(f.fds.limit))}
                  <span className="muted">, {st.acceptErrors(f.fds.acceptErrors)}</span>
                </span>
              ) : (
                <Missing live={live} />
              )}
            </Descriptions.Item>
            <Descriptions.Item label={st.reconcile}>{f ? f.reconcileRuns : '—'}</Descriptions.Item>
            <Descriptions.Item label={st.holds}>{f ? f.holds : '—'}</Descriptions.Item>
            <Descriptions.Item label={st.waiting}>{f ? f.waiting : '—'}</Descriptions.Item>
            <Descriptions.Item label={st.warnings}>{f ? f.warnings : '—'}</Descriptions.Item>
            <Descriptions.Item label={st.storage}>
              {du.data
                ? st.storageValue(bytes(du.data.total.allocated), bytes(du.data.total.logical))
                : '—'}
            </Descriptions.Item>
            <Descriptions.Item label={st.scan}>
              {du.data?.scan
                ? du.data.scan.running
                  ? st.scanRunning(du.data.scan.queued)
                  : st.scanIdle(du.data.scan.last_full ? when(du.data.scan.last_full.ended) : null)
                : '—'}
            </Descriptions.Item>
            <Descriptions.Item label={st.socket} span={2}>
              <span className="mono">{socket.data ?? '—'}</span>
            </Descriptions.Item>
          </Descriptions>
          {status.data && status.data.warnings.length > 0 && (
            <ul className={css.warnings}>
              {status.data.warnings.map((w, i) => (
                <li key={i}>{typeof w === 'string' ? w : JSON.stringify(w)}</li>
              ))}
            </ul>
          )}
        </section>
      </div>
    </Modal>
  );
}
