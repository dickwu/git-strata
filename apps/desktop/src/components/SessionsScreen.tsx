'use client';

import { Empty, Table, Tag } from 'antd';
import type { UseQueryResult } from '@tanstack/react-query';
import { copy } from '@/lib/copy';
import type { DaemonStatus, StatusReply } from '@/lib/types';
import { sessionRows, type SessionRow } from '@/lib/view';
import { useNav } from '@/stores/nav';
import { ErrorNote } from './ErrorNote';

export function healthTag(h: string) {
  return h === 'Ok' ? <Tag>Ok</Tag> : <Tag color="error">{h}</Tag>;
}

export function SessionsScreen({
  sessions,
  daemon,
  legacy,
}: {
  sessions: UseQueryResult<StatusReply>;
  daemon: UseQueryResult<DaemonStatus>;
  legacy: boolean;
}) {
  const openSession = useNav((s) => s.openSession);
  const rows = sessionRows(sessions.data);
  const c = copy.sessions.columns;
  const error = daemon.isError ? daemon.error : sessions.isError ? sessions.error : null;
  return (
    <>
      <div className="toolbar">
        <h1 className="screen-title">{copy.sessions.title}</h1>
        {legacy && sessions.data && <span className="muted">{copy.sessions.slow}</span>}
      </div>
      <div className="content" style={{ display: 'grid', gap: 16, alignContent: 'start' }}>
        {error ? <ErrorNote error={error} /> : null}
        {!error || sessions.data ? (
          <Table<SessionRow>
            size="small"
            rowKey="id"
            dataSource={rows}
            loading={sessions.isPending && !daemon.isError}
            pagination={false}
            locale={{ emptyText: <Empty description={copy.sessions.empty} /> }}
            onRow={(r) => ({ onClick: () => openSession(r.id), style: { cursor: 'default' } })}
            columns={[
              {
                title: c.ticket,
                key: 'ticket',
                render: (_, r) => (
                  <span style={{ fontFamily: 'var(--font-title)', fontWeight: 600 }}>
                    {r.ticket ?? '—'}
                  </span>
                ),
              },
              {
                title: c.id,
                dataIndex: 'id',
                render: (v: string) => <span className="mono">{v}</span>,
              },
              {
                title: c.state,
                key: 'state',
                render: (_, r) => (
                  <>
                    {r.state} <span className="muted">· {r.phase}</span>
                  </>
                ),
              },
              { title: c.repos, dataIndex: 'repos', align: 'right' },
              { title: c.changes, dataIndex: 'pending', align: 'right' },
              {
                title: c.conflicts,
                dataIndex: 'conflicts',
                align: 'right',
                render: (n: number) => (n > 0 ? <Tag color="error">{n}</Tag> : n),
              },
              { title: 'Health', dataIndex: 'health', render: healthTag },
              {
                title: c.created,
                dataIndex: 'created',
                render: (v: string) => (
                  <span className="muted">{new Date(v).toLocaleString()}</span>
                ),
              },
            ]}
          />
        ) : null}
      </div>
    </>
  );
}
