import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { renderToStaticMarkup } from 'react-dom/server';
import type { UseQueryResult } from '@tanstack/react-query';
import type { DaemonStatus, StatusReply } from '../lib/types';
import { SessionsScreen } from './SessionsScreen';

const fixture = <T,>(name: string): T =>
  JSON.parse(
    readFileSync(join(import.meta.dir, '../../src-tauri/tests/fixtures/p1', `${name}.json`), 'utf8')
  );

describe('session completion rendering', () => {
  test('done is neutral and note markup stays escaped in the ticket tooltip', () => {
    const reply = fixture<StatusReply>('status_all');
    reply.sessions[0].status.mark = {
      state: 'done',
      note: '<img src=x onerror="alert(1)"> review',
      by: { kind: 'agent' },
      at: '2026-11-04T12:00:00Z',
    };
    const sessions = {
      data: reply,
      isError: false,
      isPending: false,
    } as UseQueryResult<StatusReply>;
    const daemon = {
      data: fixture<DaemonStatus>('daemon_status'),
      isError: false,
    } as UseQueryResult<DaemonStatus>;
    const html = renderToStaticMarkup(
      <SessionsScreen sessions={sessions} daemon={daemon} legacy={false} />
    );
    expect(html).toContain('title="&lt;img src=x onerror=&quot;alert(1)&quot;&gt; review"');
    expect(html).not.toContain('<img src=x');
    expect(html).toMatch(/ant-tag[^>]*>Done<\/span>/);
    expect(html).not.toContain('ant-tag-success');
    expect(html).toContain('Show sessions with any selected completion mark');
  });
});
