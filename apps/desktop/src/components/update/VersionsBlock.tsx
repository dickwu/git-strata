'use client';

import type { ReactNode } from 'react';
import { Button, Descriptions, Space } from 'antd';
import { duration, statusFacts } from '@/lib/daemonPanel';
import type { DaemonStatus } from '@/lib/types';

interface CliLink {
  path: string;
  label: 'this app' | `Homebrew formula ${string}` | 'other';
}

/** Verified program and link data plus actions eligible for the current installation. */
export interface VersionDetails {
  program?: string;
  cli?: readonly CliLink[];
  actions?: ReactNode;
}

export function VersionsBlock({
  appVersion,
  status,
  now,
  restarting,
  program,
  cli,
  actions,
  onRestart,
  onSettings,
}: VersionDetails & {
  appVersion?: string;
  status?: DaemonStatus;
  now: number;
  restarting: boolean;
  onRestart?: () => void;
  onSettings: () => void;
}) {
  const facts = status ? statusFacts(status, now) : null;
  const daemon = restarting ? 'Restarting…' : (facts?.version ?? '?');
  const uptime = facts?.uptimeMs;
  return (
    <>
      <Descriptions
        size="small"
        column={1}
        items={[
          { key: 'app', label: 'App', children: <span className="mono">{appVersion ?? '?'}</span> },
          {
            key: 'daemon',
            label: 'Daemon',
            children: (
              <span className="mono">
                {daemon}
                {!restarting && facts?.version && facts.api !== null && ` (api ${facts.api})`}
              </span>
            ),
          },
          { key: 'pid', label: 'PID', children: restarting ? '—' : (facts?.pid ?? '—') },
          {
            key: 'uptime',
            label: 'Uptime',
            children: !restarting && uptime != null ? duration(uptime) : '—',
          },
          {
            key: 'program',
            label: 'Daemon program',
            children: program ? (
              <span className="mono">{program}</span>
            ) : (
              <span className="muted">Not reported</span>
            ),
          },
          {
            key: 'cli',
            label: 'CLI on PATH',
            children: cli?.length ? (
              cli.map(({ path, label }) => (
                <div key={path}>
                  <code>{path}</code> · {label}
                </div>
              ))
            ) : (
              <span className="muted">Not reported</span>
            ),
          },
        ]}
      />
      <Space wrap>
        {onRestart && !restarting && <Button onClick={onRestart}>Restart daemon</Button>}
        {!restarting && actions}
        <Button onClick={onSettings}>Update settings</Button>
      </Space>
    </>
  );
}
