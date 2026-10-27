'use client';

import { Button, Empty, Table, Tag } from 'antd';
import { ArrowLeftOutlined } from '@ant-design/icons';
import { copy } from '@/lib/copy';
import { useSession } from '@/lib/queries';
import { sessionDetail, short, type RepoView } from '@/lib/view';
import { useNav } from '@/stores/nav';
import { ErrorNote } from './ErrorNote';
import { healthTag } from './SessionsScreen';
import { SessionTime } from './SessionTime';
import { AgentBadge } from './AgentBadge';
import { useNow } from '@/hooks/useNow';

export function SessionDetail({ id, interval }: { id: string; interval: number | false }) {
  const openSession = useNav((s) => s.openSession);
  const q = useSession(id, interval);
  const d = sessionDetail(q.data, id);
  const t = copy.detail;
  const now = useNow();
  return (
    <>
      <div className="toolbar">
        <Button
          type="text"
          icon={<ArrowLeftOutlined aria-hidden />}
          onClick={() => openSession(null)}
          aria-label={t.back}
        />
        <h1 className="screen-title">{d?.row.ticket ?? id}</h1>
        {d && <AgentBadge agent={d.row.agent} />}
        {d && <span className="mono muted">{d.row.id}</span>}
        {d && (
          <span>
            {d.row.state}
            <span className="muted" style={{ marginLeft: 6 }}>
              {d.row.phase}
            </span>
          </span>
        )}
        {d && (
          <span style={{ marginLeft: 'auto' }}>
            <SessionTime created={d.row.created} state={d.row.state} now={now} />
          </span>
        )}
      </div>
      <div className="content" style={{ display: 'grid', gap: 16, alignContent: 'start' }}>
        {q.isError && <ErrorNote error={q.error} />}
        {q.isSuccess && !d && <Empty description={t.notFound} />}
        {d && (
          <>
            <div className="mono muted">{d.root}</div>
            <section className="panel">
              <h2 style={{ margin: '0 0 8px', fontSize: 14 }}>{t.repos}</h2>
              <Table<RepoView>
                size="small"
                rowKey="repo"
                pagination={false}
                dataSource={d.repos}
                columns={[
                  { title: 'Repo', dataIndex: 'repo', render: (v: string) => <strong>{v}</strong> },
                  {
                    title: t.head,
                    dataIndex: 'head',
                    render: (v: string) => <span className="mono">{short(v)}</span>,
                  },
                  {
                    title: t.baseTree,
                    dataIndex: 'baseTree',
                    render: (v: string) => <span className="mono">{short(v)}</span>,
                  },
                  {
                    title: t.depsSet,
                    dataIndex: 'depsSet',
                    render: (v: string) => <span className="mono">{short(v)}</span>,
                  },
                  { title: t.health, dataIndex: 'health', render: healthTag },
                  { title: t.pending, dataIndex: 'pending', align: 'right' },
                  {
                    title: t.conflicts,
                    dataIndex: 'conflicts',
                    align: 'right',
                    render: (n: number) => (n > 0 ? <Tag color="error">{n}</Tag> : n),
                  },
                  {
                    title: t.chain,
                    key: 'chain',
                    render: (_, r) =>
                      r.chain ? (
                        <span className="mono">
                          {r.chain.branch} <span className="muted">({r.chain.state})</span>
                        </span>
                      ) : (
                        '—'
                      ),
                  },
                  {
                    title: t.path,
                    dataIndex: 'path',
                    render: (v: string) => <span className="mono muted">{v}</span>,
                  },
                ]}
              />
            </section>
            <section className="panel">
              <h2 style={{ margin: '0 0 8px', fontSize: 14 }}>{t.checkpoints}</h2>
              {d.checkpoints.length === 0 ? (
                <span className="muted">{t.noCheckpoints}</span>
              ) : (
                <Table
                  size="small"
                  rowKey="id"
                  pagination={false}
                  dataSource={d.checkpoints}
                  columns={[
                    { title: '#', dataIndex: 'id', align: 'right' },
                    { title: 'Repo', dataIndex: 'repo' },
                    { title: 'When', dataIndex: 'at', render: (v: Date) => v.toLocaleString() },
                    { title: 'Reason', dataIndex: 'reason' },
                    { title: 'Entries', dataIndex: 'entries', align: 'right' },
                  ]}
                />
              )}
            </section>
          </>
        )}
      </div>
    </>
  );
}
