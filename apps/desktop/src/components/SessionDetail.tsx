'use client';

import { Empty, Modal, Spin, Table, Tag } from 'antd';
import { copy } from '@/lib/copy';
import { useSession } from '@/lib/queries';
import { sessionDetail, short, type RepoView } from '@/lib/view';
import { useNav } from '@/stores/nav';
import { ErrorNote } from './ErrorNote';
import { healthTag } from './SessionsScreen';
import { SessionTime } from './SessionTime';
import { AgentBadge } from './AgentBadge';
import { useNow } from '@/hooks/useNow';

/**
 * Session detail as a wide modal over the list (t87u: no sidebar). The list stays mounted
 * underneath, so closing (Esc, ×, mask) lands back on it with its filter and scroll intact.
 */
export function SessionDetailModal({
  id,
  interval,
}: {
  id: string | null;
  interval: number | false;
}) {
  const openSession = useNav((s) => s.openSession);
  return (
    <Modal
      open={id !== null}
      onCancel={() => openSession(null)}
      footer={null}
      width="min(1240px, calc(100vw - 48px))"
      centered
      destroyOnHidden
      closable={{ 'aria-label': copy.detail.close }}
      classNames={{ body: 'session-modal-body' }}
      title={id && <DetailTitle id={id} interval={interval} />}
    >
      {id && <DetailBody id={id} interval={interval} />}
    </Modal>
  );
}

function DetailTitle({ id, interval }: { id: string; interval: number | false }) {
  const q = useSession(id, interval);
  const d = sessionDetail(q.data, id);
  const now = useNow();
  return (
    <div className="session-modal-title">
      <span className="screen-title">{d?.row.ticket ?? id}</span>
      {d && <AgentBadge agent={d.row.agent} />}
      {d && <span className="mono muted">{d.row.id}</span>}
      {d && (
        <span>
          {d.row.state} <span className="muted">{d.row.phase}</span>
        </span>
      )}
      {d && (
        <span className="session-modal-time">
          <SessionTime created={d.row.created} state={d.row.state} now={now} />
        </span>
      )}
    </div>
  );
}

function DetailBody({ id, interval }: { id: string; interval: number | false }) {
  const q = useSession(id, interval);
  const d = sessionDetail(q.data, id);
  const t = copy.detail;
  return (
    // minmax(0, 1fr): the wide Repos table scrolls inside its panel instead of widening it.
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: 'minmax(0, 1fr)',
        gap: 16,
        alignContent: 'start',
      }}
    >
      {q.isPending && <Spin style={{ justifySelf: 'center', padding: 24 }} />}
      {q.isError && <ErrorNote error={q.error} />}
      {q.isSuccess && !d && <Empty description={t.notFound} />}
      {d && (
        <>
          <div className="mono muted">{d.root}</div>
          <section className="panel">
            <h2 style={{ margin: '0 0 8px', fontSize: 14 }}>{t.repos}</h2>
            <Table<RepoView>
              size="small"
              scroll={{ x: 'max-content' }}
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
  );
}
