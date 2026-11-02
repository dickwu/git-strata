'use client';

import { Empty, Modal, Spin, Table, Tag } from 'antd';
import { copy } from '@/lib/copy';
import type { DaemonStatus, UpstreamRepo } from '@/lib/types';
import { CopyCommand } from './CopyCommand';
import { syncCommand } from '@/lib/view';
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
  daemon,
}: {
  id: string | null;
  interval: number | false;
  daemon?: DaemonStatus;
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
      {id && <DetailBody id={id} interval={interval} daemon={daemon} />}
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

function DetailBody({
  id,
  interval,
  daemon,
}: {
  id: string;
  interval: number | false;
  daemon?: DaemonStatus;
}) {
  const q = useSession(id, interval);
  const d = sessionDetail(q.data, id, daemon);
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
          {d.repos.some((r) => r.upstream) && (
            <section className="panel upstream-section">
              <h2>From main</h2>
              {d.repos
                .filter((r) => r.upstream)
                .map((r) => (
                  <UpstreamDetails key={r.repo} repo={r.repo} session={id} upstream={r.upstream!} />
                ))}
            </section>
          )}
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

function UpstreamDetails({
  repo,
  session,
  upstream: u,
}: {
  repo: string;
  session: string;
  upstream: UpstreamRepo;
}) {
  const conflicts = [
    ...new Set([
      ...(u.conflicts ?? []),
      ...(u.auto?.reason === 'conflicts' ? (u.auto.detail?.paths ?? []) : []),
    ]),
  ];
  return (
    <article className="upstream-repo">
      <div className="waiting-head">
        <strong>{repo}</strong>
        <span className="muted">
          {u.unchecked || u.behind == null
            ? 'Behind not checked yet'
            : `${u.behind}${u.capped ? '+' : ''} commits behind`}
        </span>
      </div>
      {u.following === false && <p className="muted">Not following main</p>}
      {u.diverged && <p>Needs the human: diverged from main</p>}
      {(u.commits?.length ?? 0) > 0 && (
        <ul>
          {u.commits!.slice(0, 20).map((c) => (
            <li key={c.oid}>
              <code>{short(c.oid)}</code> {c.subject}{' '}
              <span className="muted">
                {c.author}
                {c.own ? ' · this session' : ''}
              </span>
            </li>
          ))}
        </ul>
      )}
      {u.diverged && (u.base_only?.length ?? 0) > 0 && (
        <details>
          <summary>Commits only in this session</summary>
          <ul>
            {u.base_only!.slice(0, 20).map((c) => (
              <li key={c.oid}>
                <code>{short(c.oid)}</code> {c.subject}
              </li>
            ))}
          </ul>
        </details>
      )}
      {u.deps && (
        <p>
          Deps #{u.deps.proposal}
          {u.deps.summary ? ` · ${u.deps.summary}` : ''}{' '}
          <span className="muted">
            · {u.deps.state.replace(/_/g, ' ')} · {u.deps.mode}
          </span>
        </p>
      )}
      {u.auto && (
        <p className="muted">
          {u.auto.state}
          {u.auto.reason ? `: ${u.auto.reason.replace(/_/g, ' ')}` : ''} since{' '}
          <time dateTime={u.auto.since} title={u.auto.since}>
            {new Date(u.auto.since).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
          </time>
          , attempt {u.auto.attempts}
        </p>
      )}
      {(conflicts.length > 0 || u.auto?.reason === 'conflicts') && (
        <>
          <p>Needs the session's agent</p>
          <ul>
            {conflicts.map((p) => (
              <li className="mono" key={p}>
                {p}
              </li>
            ))}
          </ul>
        </>
      )}
      <p className="muted">Give this to the session's agent</p>
      <CopyCommand command={u.fix ?? syncCommand(session, repo)} />
    </article>
  );
}
