'use client';

import { Button, Descriptions, Empty, List, Tag } from 'antd';
import { ArrowLeftOutlined } from '@ant-design/icons';
import type { UseQueryResult } from '@tanstack/react-query';
import { useNow } from '@/hooks/useNow';
import { ago, copy } from '@/lib/copy';
import { isLive, storageView } from '@/lib/daemonPanel';
import { useDu, useSocket } from '@/lib/queries';
import type { PollPlan } from '@/lib/poll';
import { retainsReply } from '@/lib/statusBar';
import type { DaemonStatus, StatusReply } from '@/lib/types';
import { daemonView } from '@/lib/view';
import { useNav } from '@/stores/nav';
import { StorageHeadline, WarningList } from './DaemonStatusModal';
import { ErrorNote } from './ErrorNote';

export function DaemonScreen({
  status,
  plan,
  onBack,
  unavailable = [],
  sessions,
}: {
  status: UseQueryResult<DaemonStatus>;
  plan: PollPlan;
  unavailable?: string[];
  /** Names a session in a warning by its ticket when this reply knows it. */
  sessions?: StatusReply;
  /** Back to the session list (the Daemon screen is full-page, reached via ⌘K). */
  onBack: () => void;
}) {
  const socket = useSocket();
  const openSession = useNav((s) => s.openSession);
  const du = useDu(plan.du);
  // Warning severity and text depend on how long a hold has run (status bar ticker cadence).
  const clock = useNow(5_000);
  // A reply that arrived after the last tick is no older than its arrival (as in the bar).
  const now = Math.max(clock, status.dataUpdatedAt);
  const live = isLive(status);
  // One failed poll keeps the last good reply on show, by the status bar's own rule: at most
  // 60 s old, from a daemon that is not stopped, starting or shedding. Past that nothing is
  // classified from the cached reply, where a hold would keep aging (a LongHold turning amber
  // at 30 min) while the daemon is not answering.
  const retained =
    !live &&
    retainsReply(
      {
        pending: status.isPending,
        data: status.data,
        error: status.isError ? status.error : null,
        updatedAt: status.dataUpdatedAt,
      },
      now
    );
  const v = (live || retained) && status.data ? daemonView(status.data, now, sessions) : null;
  const storage = live || retained ? storageView(du.data) : null;
  return (
    <>
      <div className="toolbar">
        <Button
          type="text"
          icon={<ArrowLeftOutlined aria-hidden />}
          onClick={onBack}
          aria-label={copy.detail.back}
        />
        <h1 className="screen-title">{copy.daemon.title}</h1>
        {v && !status.isError && <Tag color="success">{copy.daemon.running}</Tag>}
        {v?.version && <span className="mono muted">{v.version}</span>}
      </div>
      <div
        className="content"
        style={{ display: 'grid', gap: 16, alignContent: 'start', maxWidth: 880 }}
      >
        {status.isError && <ErrorNote error={status.error} />}
        {retained && v && (
          <div className="muted">{copy.daemon.lastAnswer(ago(now - status.dataUpdatedAt))}</div>
        )}
        {v?.legacy && !status.isError && <div className="muted">{copy.daemon.legacy}</div>}
        {v && (
          <section className="panel">
            <Descriptions column={1} size="small">
              <Descriptions.Item label={copy.daemon.socket}>
                <span className="mono">{socket.data ?? '—'}</span>
              </Descriptions.Item>
              <Descriptions.Item label="Reconcile">
                {copy.daemon.reconcile(v.reconcileRuns)}
              </Descriptions.Item>
              <Descriptions.Item label="Holds">{copy.daemon.holds(v.holds)}</Descriptions.Item>
              {storage && (
                <Descriptions.Item label={copy.daemon.storage}>
                  <StorageHeadline v={storage} />
                </Descriptions.Item>
              )}
            </Descriptions>
          </section>
        )}
        {v && (
          <section className="panel">
            <h2 style={{ margin: '0 0 8px', fontSize: 14 }}>{copy.daemon.waiting}</h2>
            {v.waiting.length === 0 ? (
              <span className="muted">{copy.daemon.noneWaiting}</span>
            ) : (
              <List
                size="small"
                dataSource={v.waiting}
                renderItem={(w) => (
                  <List.Item extra={w.n}>
                    <span className="mono">{w.repo}</span>
                  </List.Item>
                )}
              />
            )}
          </section>
        )}
        {v && (
          <section className="panel">
            <h2 style={{ margin: '0 0 8px', fontSize: 14 }}>{copy.daemon.warnings}</h2>
            {v.warnings.length === 0 ? (
              <span className="muted">{copy.daemon.noWarnings}</span>
            ) : (
              <WarningList items={v.warnings} />
            )}
          </section>
        )}
        {unavailable.length > 0 && (
          <section className="panel">
            <h2 style={{ margin: '0 0 8px', fontSize: 14 }}>{copy.tray.upstreamUnavailable}</h2>
            <p className="muted">{copy.tray.upstreamCause}</p>
            {unavailable.map((id) => (
              <Button key={id} type="link" onClick={() => openSession(id)}>
                {id}
              </Button>
            ))}
          </section>
        )}
        {!v && !status.isError && <Empty description={copy.status.loading} />}
      </div>
    </>
  );
}
