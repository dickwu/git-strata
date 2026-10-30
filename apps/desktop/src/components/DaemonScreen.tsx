'use client';

import { Button, Descriptions, Empty, List, Tag } from 'antd';
import { ArrowLeftOutlined } from '@ant-design/icons';
import type { UseQueryResult } from '@tanstack/react-query';
import { bytes, copy } from '@/lib/copy';
import { useDu, useSocket } from '@/lib/queries';
import type { PollPlan } from '@/lib/poll';
import type { DaemonStatus } from '@/lib/types';
import { daemonView } from '@/lib/view';
import { ErrorNote } from './ErrorNote';

export function DaemonScreen({
  status,
  plan,
  onBack,
}: {
  status: UseQueryResult<DaemonStatus>;
  plan: PollPlan;
  /** Back to the session list (the Daemon screen is full-page, reached via ⌘K). */
  onBack: () => void;
}) {
  const socket = useSocket();
  const du = useDu(plan.du);
  const v = status.data ? daemonView(status.data) : null;
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
              {du.data && (
                <Descriptions.Item label={copy.daemon.storage}>
                  {bytes(du.data.total.allocated)} on disk ({bytes(du.data.total.logical)} logical)
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
              <List
                size="small"
                dataSource={v.warnings}
                renderItem={(w) => <List.Item>{w}</List.Item>}
              />
            )}
          </section>
        )}
        {!v && !status.isError && <Empty description={copy.status.loading} />}
      </div>
    </>
  );
}
