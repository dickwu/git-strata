import { expect, mock, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  createUpdateStore,
  restartingDaemon,
  schemaAcknowledgementRequest,
  type UpdateClient,
} from '@/lib/update';
import { createUpdateSettingsStore } from '@/lib/updateSettings';
import type { GuiError, UpdateState } from '@/lib/types';
import {
  CancelAfterStopDialog,
  canDismissUpdateProgress,
  UpdateProgressContent,
} from './UpdateDialogs';

const candidate = { version: '0.3.1', store_schema: 8 };
const available: UpdateState = { phase: 'available', target: candidate };
const cancelled: UpdateState = { phase: 'cancelled' };
const draining: UpdateState = { phase: 'draining', inflight: [], background: [] };
const stopping: UpdateState = { phase: 'stopping' };

function fakeClock() {
  let now = Date.parse('2026-11-17T20:00:00Z');
  let timer: { when: number; fire: () => void } | undefined;
  return {
    now: () => now,
    after(ms: number, fire: () => void) {
      timer = { when: now + ms, fire };
      return () => {
        timer = undefined;
      };
    },
    advance(ms: number) {
      now += ms;
      if (timer && timer.when <= now) {
        const fire = timer.fire;
        timer = undefined;
        fire();
      }
    },
  };
}

function fake(overrides: Partial<UpdateClient> = {}): UpdateClient {
  return {
    state: async () => available,
    check: async () => available,
    install: async () => {},
    restart: async () => {},
    cancel: async () => {},
    ...overrides,
  };
}

test('the controller hands the target and exact acknowledgement to install and consumes the Channel', async () => {
  let complete!: () => void;
  let send!: Parameters<UpdateClient['install']>[2];
  const install = mock<UpdateClient['install']>((_target, _ack, onProgress) => {
    send = onProgress;
    return new Promise((resolve) => {
      complete = resolve;
    });
  });
  const store = createUpdateStore(fake({ install }));
  await store.getState().refresh();
  expect(store.getState().snapshot).toEqual(available);
  const inflight = store.getState().install(candidate, 8);
  expect(install).toHaveBeenCalledWith('0.3.1', 8, expect.any(Function));
  await store.getState().install(candidate, 8);
  expect(install).toHaveBeenCalledTimes(1);

  send({ phase: 'draining', inflight: [], background: [] });
  expect(restartingDaemon(store.getState())).toBe(false);
  send({ phase: 'stopping' });
  expect(restartingDaemon(store.getState())).toBe(true);
  send({ phase: 'bootout_failed' });
  expect(restartingDaemon(store.getState())).toBe(false);
  complete();
  await inflight;
  send({ phase: 'installing' });
  expect(store.getState().operation?.state.phase).toBe('bootout_failed');
});

test('a successful cancel request waits for the cancellation outcome', async () => {
  let complete!: () => void;
  const cancel = mock(async () => {});
  const store = createUpdateStore(
    fake({
      install: (_target, _ack, progress) =>
        new Promise((resolve) => {
          progress({ phase: 'draining', inflight: [], background: [] });
          complete = () => {
            progress(cancelled);
            resolve();
          };
        }),
      cancel,
    })
  );
  const updating = store.getState().install(candidate);
  await store.getState().cancel();
  await store.getState().cancel();
  expect(cancel).toHaveBeenCalledTimes(1);
  expect(store.getState().operation?.state.phase).toBe('draining');
  expect(store.getState().cancelPending).toBe(true);
  complete();
  await updating;
  expect(store.getState().operation?.state).toEqual(cancelled);
  expect(store.getState().cancelPending).toBe(false);
});

test('a reopened window can cancel a native drain or stop and wait for its terminal state', async () => {
  for (const phase of [draining, stopping]) {
    let native: UpdateState = phase;
    const cancel = mock(async () => {});
    const store = createUpdateStore(fake({ state: async () => native, cancel }));
    await store.getState().refresh();
    expect(store.getState().operation).toBeNull();

    const first = store.getState().cancel();
    await store.getState().cancel();
    await first;
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(store.getState().cancelPending).toBe(true);
    expect(store.getState().snapshot).toEqual(phase);

    native = cancelled;
    await store.getState().refresh();
    expect(store.getState().snapshot).toEqual(cancelled);
    expect(store.getState().cancelPending).toBe(false);
    expect(restartingDaemon(store.getState())).toBe(false);
  }
});

test('failure to send Cancel lets a reopened window try again', async () => {
  const error = { code: 'Busy', message: 'Cancellation is temporarily unavailable', detail: null };
  const cancel = mock(async () => {
    throw error;
  });
  const store = createUpdateStore(fake({ state: async () => draining, cancel }));
  await store.getState().refresh();
  for (let i = 0; i < 2; i++) await store.getState().cancel();
  expect(cancel).toHaveBeenCalledTimes(2);
  expect(store.getState().cancelPending).toBe(false);
  expect(store.getState().cancelError).toEqual(error);
});

test('both post-seal consent decisions respond to the same cancel without replaying an install', async () => {
  for (const decision of [8, null]) {
    let native: UpdateState = stopping;
    let decide!: (acknowledgement: number | null) => void;
    const install = mock<UpdateClient['install']>(async () => {});
    const restart = mock<UpdateClient['restart']>(async () => {});
    const cancel = mock(async () => {
      const answer = await new Promise<number | null>((resolve) => {
        decide = resolve;
      });
      native = answer === 8 ? cancelled : { phase: 'cancel_blocked' };
    });
    const store = createUpdateStore(fake({ state: async () => native, install, restart, cancel }));
    await store.getState().refresh();
    const pending = store.getState().cancel();
    const { props } = CancelAfterStopDialog({
      consent: { id: 'request-for-the-stopped-transaction', from: 7, to: 8, decide },
    });
    expect(props.laterText).toBe('Keep daemon stopped');
    if (decision === null) props.onLater();
    else props.onConfirm(props.to);
    await pending;
    expect(store.getState().snapshot.phase).toBe(decision === 8 ? 'cancelled' : 'cancel_blocked');
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(install).toHaveBeenCalledTimes(0);
    expect(restart).toHaveBeenCalledTimes(0);
  }
});

test('a stale passive reply cannot overwrite a manual result', async () => {
  let returnState!: (state: UpdateState) => void;
  const store = createUpdateStore(
    fake({
      state: () =>
        new Promise((resolve) => {
          returnState = resolve;
        }),
    })
  );
  const read = store.getState().refresh();
  await store.getState().check();
  returnState({ phase: 'up_to_date' });
  await read;
  expect(store.getState().snapshot).toEqual(available);
  expect(store.getState().lastCheck).toEqual(available);
});

test('manual check failures are visible while passive failures retain the last offer', async () => {
  const unavailable = {
    code: 'Unsupported',
    message: 'not available',
    detail: { reason: 'not_implemented' },
  };
  let healthy = true;
  const store = createUpdateStore(
    fake({
      state: async () => {
        if (!healthy) throw unavailable;
        return available;
      },
      check: async () => {
        throw unavailable;
      },
    })
  );
  await store.getState().refresh();
  healthy = false;
  await store.getState().refresh();
  expect(store.getState().lastCheck).toBeUndefined();
  expect(store.getState().snapshot).toEqual(available);
  await store.getState().check();
  expect(store.getState().lastCheck?.phase).toBe('check_failed');
});

test('restart sends only the schema acknowledgement and clears suppression after failure', async () => {
  const error = { code: 'Busy', message: 'The daemon did not come back', detail: null };
  const restart = mock<UpdateClient['restart']>(async (_ack, progress) => {
    progress({ phase: 'starting_daemon' });
    throw error;
  });
  const store = createUpdateStore(fake({ restart }));
  await store.getState().restart(8);
  expect(restart).toHaveBeenCalledWith(8, expect.any(Function));
  expect(store.getState().operation?.error).toEqual(error);
  expect(restartingDaemon(store.getState())).toBe(false);
});

test('a seal refusal published after idle is still an alert in the restart dialog', async () => {
  const refusal: GuiError = {
    code: 'Busy',
    message: 'The daemon refused the stop seal',
    detail: { reason: 'seal_refused' },
  };
  const store = createUpdateStore(
    fake({
      restart: async (_ack, progress) => {
        progress(draining);
        progress(stopping);
        progress({ phase: 'idle' });
        throw refusal;
      },
    })
  );
  await store.getState().restart();
  await store.getState().refresh();
  const { operation, restartStatus } = store.getState();
  expect(operation?.state.phase).toBe('idle');
  expect(operation?.error).toEqual(refusal);
  expect(restartingDaemon(store.getState())).toBe(false);
  const html = renderToStaticMarkup(
    <UpdateProgressContent
      state={{ phase: 'idle' }}
      operation={operation}
      restartStatus={restartStatus}
      onKeepWaiting={() => {}}
    />
  );
  expect(html).toContain('role="alert"');
  expect(html).toContain(refusal.message);
  expect(html).not.toContain('Check for updates');
});

test('when a command reply arrives before its terminal phase, a native read settles progress', async () => {
  const install: UpdateClient['install'] = async (_target, _ack, progress) => {
    progress({ phase: 'relaunching' });
  };
  const finished = createUpdateStore(fake({ install, state: async () => ({ phase: 'done' }) }));
  await finished.getState().install(candidate);
  expect(finished.getState().operation?.state.phase).toBe('done');

  const idle = createUpdateStore(fake({ install, state: async () => ({ phase: 'idle' }) }));
  await idle.getState().install(candidate);
  expect(idle.getState().operation).toBeNull();
  expect(idle.getState().restartStatus).toBeNull();
});

test('dismissing a snapshot-only daemon failure leaves the next update’s identical failure visible', async () => {
  const failed = { phase: 'daemon_failed', reason: 'restart_timeout' } as const;
  let native: UpdateState = draining;
  const store = createUpdateStore(fake({ state: async () => ({ ...native }) }), fakeClock());
  const { refresh, dismiss } = store.getState();
  await refresh();
  native = failed;
  await refresh();
  expect(store.getState().snapshotDismissed).toBe(false);

  dismiss();
  await refresh();
  expect(store.getState().snapshotDismissed).toBe(true);

  native = draining;
  await refresh();
  native = failed;
  await refresh();
  expect(store.getState().operation).toBeNull();
  expect(store.getState().snapshot).toEqual(failed);
  expect(store.getState().snapshotDismissed).toBe(false);
});

test('a dismissed command outcome stays closed when the same outcome arrives by polling', async () => {
  const failed = { phase: 'daemon_failed', reason: 'restart_timeout' } as const;
  let native: UpdateState = available;
  const store = createUpdateStore(
    fake({
      state: async () => native,
      install: async (_target, _ack, progress) => progress(failed),
    })
  );
  const { refresh, install, dismiss } = store.getState();
  await refresh();
  await install(candidate);
  dismiss();

  native = failed;
  await refresh();
  await refresh();
  expect(store.getState().operation).toBeNull();
  expect(store.getState().snapshot).toEqual(failed);
  expect(store.getState().snapshotDismissed).toBe(true);
});

test('the restart window expires across phases even if passive reads fail', async () => {
  let failRead = false;
  const clock = fakeClock();
  let native: UpdateState = { phase: 'waiting_daemon', since: new Date(clock.now()).toISOString() };
  const store = createUpdateStore(
    fake({
      state: async () => {
        if (failRead) throw new Error('state unavailable');
        return native;
      },
    }),
    clock
  );
  await store.getState().refresh();
  for (const phase of ['relaunching', 'reconciling'] as const) {
    clock.advance(40_000);
    native = { phase };
    await store.getState().refresh();
    expect(restartingDaemon(store.getState())).toBe(true);
  }
  failRead = true;
  await store.getState().refresh();
  clock.advance(69_999);
  expect(restartingDaemon(store.getState())).toBe(true);
  clock.advance(1);
  expect(store.getState().snapshot.phase).toBe('reconciling');
  expect(restartingDaemon(store.getState())).toBe(false);
});

test('a cold reopen in reconciliation at +140s expires ten seconds later even if reads fail', async () => {
  const clock = fakeClock();
  const native = { phase: 'reconciling' as const, start_requested_ms: clock.now() };
  let failRead = false;
  const ipc = fake({
    state: async () => {
      if (failRead) throw new Error('state unavailable');
      return native;
    },
  });
  clock.advance(140_000);
  const reopened = createUpdateStore(ipc, clock);
  await reopened.getState().refresh();
  expect(restartingDaemon(reopened.getState())).toBe(true);
  expect(canDismissUpdateProgress(native, null, reopened.getState().restartStatus)).toBe(false);
  failRead = true;
  await reopened.getState().refresh();
  clock.advance(9_999);
  expect(restartingDaemon(reopened.getState())).toBe(true);
  clock.advance(1);
  const { restartStatus, snapshot, dismiss } = reopened.getState();
  expect(restartStatus).toBe('expired');
  expect(canDismissUpdateProgress(snapshot, null, restartStatus)).toBe(true);
  dismiss();
  expect(reopened.getState().snapshot).toEqual(native);
});

test('a reopened wait with no deadline remains dismissible without suppressing outage reporting', async () => {
  const store = createUpdateStore(fake({ state: async () => ({ phase: 'reconciling' }) }));
  await store.getState().refresh();
  const { restartStatus, snapshot } = store.getState();
  expect(restartingDaemon(store.getState())).toBe(false);
  expect(restartStatus).toBe('unknown');
  expect(canDismissUpdateProgress(snapshot, null, restartStatus)).toBe(true);
});

test('closing an expired live operation leaves native progress and its install gate intact', async () => {
  const clock = fakeClock();
  let finish!: () => void;
  const install = mock<UpdateClient['install']>((_target, _ack, progress) => {
    progress({ phase: 'starting_daemon' });
    return new Promise<void>((resolve) => {
      finish = () => {
        progress({ phase: 'done' });
        resolve();
      };
    });
  });
  const store = createUpdateStore(fake({ install }), clock);
  const pending = store.getState().install(candidate);
  clock.advance(150_000);
  const { operation, restartStatus, dismiss } = store.getState();
  expect(operation?.running).toBe(true);
  expect(restartStatus).toBe('expired');
  expect(canDismissUpdateProgress({ phase: 'starting_daemon' }, operation, restartStatus)).toBe(true);
  dismiss();
  await store.getState().install(candidate);
  expect(install).toHaveBeenCalledTimes(1);
  expect(store.getState().operation).toBe(operation);
  finish();
  await pending;
  expect(store.getState().operation?.state.phase).toBe('done');
  expect(store.getState().restartStatus).toBeNull();
});

test('a reopened daemon wait uses its native start time and pauses for macOS permission', async () => {
  const clock = fakeClock();
  const waiting: UpdateState = {
    phase: 'waiting_daemon',
    since: new Date(clock.now()).toISOString(),
  };
  let native: UpdateState = waiting;
  clock.advance(140_000);
  const store = createUpdateStore(fake({ state: async () => native }), clock);
  await store.getState().refresh();
  expect(restartingDaemon(store.getState())).toBe(true);

  native = { phase: 'waiting_permission' };
  await store.getState().refresh();
  clock.advance(60_000);
  await store.getState().refresh();
  clock.advance(120_000);
  expect(restartingDaemon(store.getState())).toBe(true);

  native = waiting;
  await store.getState().refresh();
  clock.advance(9_999);
  expect(restartingDaemon(store.getState())).toBe(true);
  clock.advance(1);
  expect(restartingDaemon(store.getState())).toBe(false);
});

test('a cold reopen restores the native permission pause and resumes with the same remaining budget', async () => {
  const clock = fakeClock();
  const since = clock.now();
  const paused = {
    phase: 'reconciling' as const,
    start_requested_ms: since,
    permission_wait_elapsed_ms: 140_000,
  };
  let native: UpdateState = paused;
  const ipc = fake({ state: async () => native });
  clock.advance(200_000);
  const reopened = createUpdateStore(ipc, clock);
  await reopened.getState().refresh();
  expect(restartingDaemon(reopened.getState())).toBe(true);
  clock.advance(120_000);
  expect(restartingDaemon(reopened.getState())).toBe(true);
  const resumed = { phase: 'reconciling' as const, start_requested_ms: since + 180_000 };
  native = resumed;
  await reopened.getState().refresh();
  clock.advance(9_999);
  expect(restartingDaemon(reopened.getState())).toBe(true);
  clock.advance(1);
  expect(reopened.getState().restartStatus).toBe('expired');
});

test('a terminal state ends suppression immediately instead of waiting for the deadline', async () => {
  const clock = fakeClock();
  let native: UpdateState = { phase: 'waiting_daemon', since: new Date(clock.now()).toISOString() };
  const ipc = fake({ state: async () => native });
  const store = createUpdateStore(ipc, clock);
  await store.getState().refresh();
  expect(restartingDaemon(store.getState())).toBe(true);
  native = { phase: 'done' };
  await store.getState().refresh();
  expect(restartingDaemon(store.getState())).toBe(false);
  native = { phase: 'reconciling' };
  await store.getState().refresh();
  expect(store.getState().restartStatus).toBe('unknown');
});

const migrationRefusal: GuiError = {
  code: 'NotConfirmed',
  message: 'Confirm the store migration warning',
  detail: { reason: 'schema_acknowledgement_required', from_schema: 7, to_schema: 8 },
};

test('accepting a verified schema rise resubmits the exact schema instead of the manifest value', async () => {
  const offer = { ...candidate, store_schema: 7 };
  const install = mock<UpdateClient['install']>(async (_target, ack, progress) => {
    progress({ phase: 'verifying_bundle' });
    if (ack !== 8) throw migrationRefusal;
    progress(draining);
  });
  const store = createUpdateStore(fake({ install, state: async () => draining }));
  await store.getState().install(offer);
  expect(schemaAcknowledgementRequest(store.getState().operation)).toEqual({ from: 7, to: 8 });

  await store.getState().confirmSchema(7);
  expect(install).toHaveBeenCalledTimes(1);
  await store.getState().confirmSchema(8);
  expect(install).toHaveBeenLastCalledWith(offer.version, 8, expect.any(Function));
  expect(store.getState().operation?.state).toEqual(draining);
});

test('declining the verified schema prompt leaves the update offer and makes no second install call', async () => {
  const install = mock<UpdateClient['install']>(async (_target, _ack, progress) => {
    progress({ phase: 'verifying_bundle' });
    throw migrationRefusal;
  });
  const store = createUpdateStore(fake({ install }));
  await store.getState().refresh();
  await store.getState().install(candidate);
  expect(schemaAcknowledgementRequest(store.getState().operation)?.to).toBe(8);
  store.getState().dismiss();
  await store.getState().confirmSchema(8);
  expect(install).toHaveBeenCalledTimes(1);
  expect(store.getState().snapshot).toEqual(available);
});

test('the native pre-seal restart refusal can acknowledge an unknown baseline', async () => {
  for (const phase of [{ phase: 'confirming' } as const, draining]) {
    const restart = mock<UpdateClient['restart']>(async (ack, progress) => {
      if (ack !== 8) {
        progress(phase);
        throw {
          ...migrationRefusal,
          detail: { reason: 'schema_acknowledgement_required', from_schema: null, to_schema: 8 },
        };
      }
      progress({ phase: 'done' });
    });
    const store = createUpdateStore(fake({ restart }));
    await store.getState().restart();
    expect(schemaAcknowledgementRequest(store.getState().operation)).toEqual({
      from: undefined,
      to: 8,
    });
    await store.getState().confirmSchema(8);
    expect(restart).toHaveBeenLastCalledWith(8, expect.any(Function));
    expect(store.getState().operation?.state.phase).toBe('done');
  }
});

test('a terminal daemon failure cannot replay a restart as pre-seal consent', async () => {
  const restart = mock<UpdateClient['restart']>(async (_ack, progress) => {
    progress({ phase: 'daemon_failed', reason: 'schema_acknowledgement_required' });
    throw migrationRefusal;
  });
  const store = createUpdateStore(fake({ restart }));
  await store.getState().restart();
  expect(schemaAcknowledgementRequest(store.getState().operation)).toBeUndefined();
  await store.getState().confirmSchema(8);
  expect(restart).toHaveBeenCalledTimes(1);
});

test('a schema refusal after stopping cannot replay the original install', async () => {
  const install = mock<UpdateClient['install']>(async (_target, _ack, progress) => {
    progress(stopping);
    throw migrationRefusal;
  });
  const store = createUpdateStore(fake({ install }));
  await store.getState().install(candidate);
  expect(schemaAcknowledgementRequest(store.getState().operation)).toBeUndefined();
  await store.getState().confirmSchema(8);
  expect(install).toHaveBeenCalledTimes(1);
});

test('Settings reads and saves the native preference without disabling manual checks', async () => {
  type SettingsEvents = NonNullable<Parameters<typeof createUpdateSettingsStore>[0]>;
  let receive: Parameters<SettingsEvents['listen']>[0] | undefined;
  let saved = true;
  const events: SettingsEvents = {
    listen: async (callback) => {
      receive = callback;
      return () => {
        receive = undefined;
      };
    },
    read: async () => {
      expect(receive).toBeDefined();
      receive?.({ Ok: saved });
    },
    write: async (enabled) => {
      saved = enabled;
    },
  };

  const settings = createUpdateSettingsStore(events);
  const disconnect = settings.getState().connect();
  await Promise.resolve();
  expect(settings.getState().automatic).toBe(true);
  await settings.getState().setAutomatic(false);
  expect(settings.getState().saving).toBe(true);
  expect(settings.getState().automatic).toBe(true);
  receive?.({ Ok: saved });
  expect(settings.getState().automatic).toBe(false);
  disconnect();

  const reopened = createUpdateSettingsStore(events);
  const close = reopened.getState().connect();
  await Promise.resolve();
  expect(reopened.getState().automatic).toBe(false);
  const check = mock(async () => available);
  await createUpdateStore(fake({ check })).getState().check();
  expect(check).toHaveBeenCalledTimes(1);
  close();
});
