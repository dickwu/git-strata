// Browser fixture for the mounted Shell. The fake IPC never starts a daemon.
import { App } from 'antd';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { mockIPC } from '@tauri-apps/api/mocks';
import type { Channel } from '@tauri-apps/api/core';
import { createRoot } from 'react-dom/client';
import { Shell } from '@/components/Shell';
import { updateStore } from '@/lib/update';
import type { UpdateState } from '@/lib/types';

const candidate = { version: '0.3.1', store_schema: 7 };
const available = {
  phase: 'available', target: candidate,
  install_eligibility: { kind: 'eligible', version: candidate.version },
} satisfies UpdateState;
let native: UpdateState = available;
let installs = 0;
let checks = 0;
let recovered = false;
const consentAnswers: Array<{ request_id: string; acknowledgement: number | null }> = [];
let settleInstall: ((accepted: boolean) => void) | undefined;

function assert(condition: unknown, explanation: string): asserts condition {
  if (!condition) throw new Error(explanation);
}

mockIPC((command, rawArgs) => {
  const args = rawArgs as Record<string, unknown>;
  switch (command) {
    case 'plugin:event|listen':
      return args.handler;
    case 'plugin:event|unlisten':
      return undefined;
    case 'plugin:event|emit_to':
      if (args.event === 'git-strata-schema-answer') {
        assert((args.target as { label: string }).label === 'main', 'Consent used the wrong window');
        const decision = args.payload as (typeof consentAnswers)[number];
        consentAnswers.push(decision);
        settleInstall?.(decision.acknowledgement === 8);
      }
      return undefined;
    case 'daemon_status':
      return {
        version: '0.3.0', pid: 41, started: '2026-11-17T15:00:00Z',
        holds: [], warnings: [], queues: [], features: ['daemon.drain'], store_schema: 7,
      };
    case 'sessions_list':
      return { sessions: [] };
    case 'deps_list':
      return { proposals: [] };
    case 'du':
      return {};
    case 'plugin:app|version':
      return '0.3.0';
    case 'update_state':
      return native;
    case 'update_check':
      checks++;
      return available;
    case 'update_install': {
      assert(args.target === candidate.version, 'The mounted dialog chose the wrong target');
      const channel = args.onProgress as Channel<UpdateState>;
      const id = `native-${++installs}`;
      return new Promise<void>((resolve, reject) => {
        settleInstall = (accepted) => {
          const failure = { code: 'Busy', message: 'Fake daemon did not start', detail: { reason: 'start_timeout' } };
          native = accepted
            ? { phase: 'daemon_failed', reason: 'start_timeout', transaction_id: id, operation_error: failure,
                recovery: { start: true, rollback_version: '0.3.0' } }
            : { phase: 'cancelled', transaction_id: id };
          channel.onmessage(native);
          if (accepted) reject(failure);
          else resolve();
          void updateStore.getState().refresh();
        };
        native = {
          phase: 'stopping', transaction_id: id, start_requested_ms: Date.now(),
          schema_consent: { request_id: id, from_schema: 7, to_schema: 8, version: '0.3.1' },
        };
        channel.onmessage(native);
        void updateStore.getState().refresh();
      });
    }
    case 'update_rollback': {
      assert(args.startOnly === true, 'Start selected the wrong recovery action');
      recovered = true;
      native = { phase: 'done', transaction_id: 'native-2' };
      (args.onProgress as Channel<UpdateState>).onmessage(native);
      return undefined;
    }
    default:
      return undefined;
  }
});

const pause = () => new Promise<void>((resolve) => setTimeout(resolve, 25));
async function find<T>(probe: () => T | undefined, label: string): Promise<T> {
  for (let n = 0; n < 140; n++) {
    const value = probe();
    if (value !== undefined) return value;
    await pause();
  }
  throw new Error(`Missing ${label}`);
}

async function button(text: string) {
  const selected = await find(
    () => Array.from(document.querySelectorAll('button')).find((b) =>
      !b.disabled && b.getClientRects().length > 0
      && (b.getAttribute('aria-label') === text || b.textContent?.trim() === text)),
    text,
  );
  selected.click();
}

async function exercise() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const target = document.createElement('div');
  document.body.append(target);
  createRoot(target).render(
    <QueryClientProvider client={client}><App><Shell /></App></QueryClientProvider>
  );

  await button('Update to 0.3.1');
  await button('Update');
  await button('Keep daemon stopped');
  await find(() => installs === 1 && consentAnswers.length === 1 ? true : undefined, 'first consent');
  assert(consentAnswers[0]?.request_id === 'native-1' && consentAnswers[0]?.acknowledgement === null, 'The decline did not reach its native request');
  await button('Close');
  native = available;
  await updateStore.getState().refresh();

  await button('Update to 0.3.1');
  await button('Update');
  const acknowledgement = await find(
    () => Array.from(document.querySelectorAll<HTMLInputElement>('input[type=checkbox]')).find(
      (input) => input.closest('label')?.textContent?.includes('I understand')),
    'schema acknowledgement',
  );
  acknowledgement.click();
  await button('Restart daemon');
  await find(() => consentAnswers.length === 2 ? true : undefined, 'second consent');
  assert(consentAnswers[1]?.request_id === 'native-2' && consentAnswers[1]?.acknowledgement === 8, 'The verified schema did not reach its native request');
  await find(
    () => Array.from(document.querySelectorAll('button')).find((b) => b.textContent?.includes('Roll back to 0.3.0')),
    'rollback action',
  );
  await button('Start daemon');
  await find(() => recovered ? true : undefined, 'native Start daemon recovery call');
  assert(recovered, 'Mounted recovery did not invoke the native action');

  await button('Close');
  native = available;
  await updateStore.getState().refresh();
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', metaKey: true }));
  const option = await find(() => Array.from(document.querySelectorAll<HTMLElement>('[role=option]'))
    .find((row) => row.textContent?.trim() === 'Check for updates…'), 'check command');
  option.click();
  await find(() => checks === 1 ? true : undefined, 'manual check');
  assert(checks === 1, 'The command palette invoked the check more than once');
  document.body.dataset.result = 'passed';
}

void exercise().catch((error: unknown) => {
  document.body.dataset.result = 'failed';
  document.body.textContent = String(error);
});
