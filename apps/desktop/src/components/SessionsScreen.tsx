'use client';

import { Button, Empty, Table, Tag } from 'antd';
import type { UseQueryResult } from '@tanstack/react-query';
import { copy } from '@/lib/copy';
import type { DaemonStatus, StatusReply } from '@/lib/types';
import { filterRows, knownAgents, sessionRows, usedRepos, type SessionRow } from '@/lib/view';
import { FilterBar } from './FilterBar';
import { AgentBadge } from './AgentBadge';
import { useNav } from '@/stores/nav';
import { ErrorNote } from './ErrorNote';
import { RepoTags } from './RepoTags';
import { SessionTime } from './SessionTime';
import { useNow } from '@/hooks/useNow';

/** "Ok" is the normal case and stays quiet; anything else is a red tag. */
export function healthTag(h: string) {
  return h === 'Ok' ? <span className="muted">Ok</span> : <Tag color="error">{h}</Tag>;
}

export function SessionsScreen({
  sessions,
  daemon,
  legacy,
  waiting = 0,
}: {
  sessions: UseQueryResult<StatusReply>;
  daemon: UseQueryResult<DaemonStatus>;
  legacy: boolean;
  /** Things waiting for a human (tray rule). Shown here since the sidebar has no Sessions item (t76u). */
  waiting?: number;
}) {
  const { openSession, repoFilter, agentFilter, textFilter, toggleRepoFilter, clearFilters } =
    useNav();
  const now = useNow();
  const all = sessionRows(sessions.data);
  const repos = usedRepos(all);
  const agents = knownAgents(all);
  // A selected repo or agent no current session has drops out of the filter (t73u).
  const repoSel = repoFilter.filter((r) => repos.some((x) => x.name === r));
  const agentSel = agentFilter.filter((a) => agents.some((x) => x.name === a));
  const rows = filterRows(all, { repos: repoSel, agents: agentSel, text: textFilter });
  const filtered = rows.length !== all.length;
  const c = copy.sessions.columns;
  const error = daemon.isError ? daemon.error : sessions.isError ? sessions.error : null;
  return (
    <>
      <div className="toolbar">
        <h1 className="screen-title">{copy.sessions.title}</h1>
        {waiting > 0 && (
          <span className="waiting-note" role="status">
            <span className="dot" data-state="down" aria-hidden />
            {copy.nav.waiting(waiting)}
          </span>
        )}
        {legacy && sessions.data && <span className="muted">{copy.sessions.slow}</span>}
      </div>
      <div className="content" style={{ display: 'grid', gap: 12, alignContent: 'start' }}>
        {error ? <ErrorNote error={error} /> : null}
        {all.length > 0 && (
          <FilterBar
            repos={repos}
            agents={agents}
            repoSel={repoSel}
            agentSel={agentSel}
            shown={rows.length}
            total={all.length}
          />
        )}
        {!error || sessions.data ? (
          <Table<SessionRow>
            className="sessions-table"
            size="small"
            rowKey="id"
            dataSource={rows}
            loading={sessions.isPending && !daemon.isError}
            pagination={false}
            tableLayout="fixed"
            // Fixed columns take 650px and Repos gets the rest. 860 fits a 1200px window with
            // the sidebar open (content is ~895px); narrower than that, the table scrolls
            // sideways so Repos keeps ~210px for its wrapping tags instead of a sliver.
            scroll={{ x: 860 }}
            locale={{
              emptyText:
                filtered && all.length > 0 ? (
                  <Empty description={copy.sessions.filter.none}>
                    <Button onClick={clearFilters}>{copy.sessions.filter.clear}</Button>
                  </Empty>
                ) : (
                  <Empty description={copy.sessions.empty} />
                ),
            }}
            onRow={(r) => ({ onClick: () => openSession(r.id), style: { cursor: 'default' } })}
            columns={[
              {
                title: c.ticket,
                key: 'ticket',
                width: 136,
                ellipsis: true,
                render: (_, r) => (
                  <div className="ticket-cell">
                    <span style={{ fontFamily: 'var(--font-title)', fontWeight: 600 }}>
                      {r.ticket ?? '—'}
                    </span>
                    <AgentBadge agent={r.agent} />
                  </div>
                ),
              },
              {
                title: c.id,
                dataIndex: 'id',
                width: 100,
                render: (v: string) => <span className="mono">{v}</span>,
              },
              {
                title: c.state,
                key: 'state',
                width: 88,
                render: (_, r) => (
                  <div className="stacked-cell">
                    <span>{r.state}</span>
                    <span className="muted">{r.phase}</span>
                  </div>
                ),
              },
              {
                title: c.repos,
                key: 'repos',
                render: (_, r) => (
                  <RepoTags repos={r.repoList} selected={repoSel} onToggle={toggleRepoFilter} />
                ),
              },
              { title: c.changes, dataIndex: 'pending', align: 'right', width: 68 },
              {
                title: c.conflicts,
                dataIndex: 'conflicts',
                align: 'right',
                width: 78,
                render: (n: number) => (n > 0 ? <Tag color="error">{n}</Tag> : n),
              },
              { title: 'Health', dataIndex: 'health', width: 64, render: healthTag },
              {
                title: c.time,
                key: 'time',
                width: 112,
                render: (_, r) => <SessionTime created={r.created} state={r.state} now={now} />,
              },
            ]}
          />
        ) : null}
      </div>
    </>
  );
}
