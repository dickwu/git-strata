'use client';

import { useQuery } from '@tanstack/react-query';
import { useStore } from 'zustand';
import { createStore } from 'zustand/vanilla';
import { asGuiError, inTauri, reason } from './tauri';
import type {
  GuiError,
  UpdateCommandArgs,
  UpdateCommandResults,
  UpdateState,
  UpdateTarget,
} from './types';

type Command = keyof UpdateCommandArgs;
type Progress = (state: UpdateState) => void;

async function invokeUpdate<K extends Command>(
  command: K,
  args: UpdateCommandArgs[K]
): Promise<UpdateCommandResults[K]> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<UpdateCommandResults[K]>(command, args === undefined ? undefined : { ...args });
}

async function channel(onProgress: Progress) {
  const { Channel } = await import('@tauri-apps/api/core');
  return new Channel<UpdateState>(onProgress);
}

export interface UpdateClient {
  state: () => Promise<UpdateState>;
  check: () => Promise<UpdateState>;
  install: (
    target: string,
    schemaAcknowledgement: number | undefined,
    progress: Progress
  ) => Promise<void>;
  restart: (schemaAcknowledgement: number | undefined, progress: Progress) => Promise<void>;
  cancel: () => Promise<void>;
  recover?: (startOnly: boolean, progress: Progress) => Promise<void>;
}

const client: UpdateClient = {
  state: () => invokeUpdate('update_state', undefined),
  check: () => invokeUpdate('update_check', undefined),
  install: async (target, schemaAcknowledgement, progress) =>
    invokeUpdate('update_install', {
      target,
      schemaAcknowledgement,
      onProgress: await channel(progress),
    }),
  restart: async (schemaAcknowledgement, progress) =>
    invokeUpdate('daemon_restart', { schemaAcknowledgement, onProgress: await channel(progress) }),
  cancel: () => invokeUpdate('update_cancel', undefined),
  recover: async (startOnly, progress) =>
    invokeUpdate('update_rollback', { startOnly, onProgress: await channel(progress) }),
};

export function useAppVersion() {
  return useQuery({
    queryKey: ['appVersion'],
    queryFn: async () => (await import('@tauri-apps/api/app')).getVersion(),
    enabled: inTauri(),
    retry: false,
    staleTime: Infinity,
  }).data;
}

const preUpdate = new Set<UpdateState['phase']>([
  'idle',
  'checking',
  'up_to_date',
  'available',
  'check_failed',
]);
const active = new Set<UpdateState['phase']>([
  'confirming',
  'downloading',
  'verifying_bundle',
  'draining',
  'stopping',
  'installing',
  'starting_daemon',
  'relaunching',
  'reconciling',
  'waiting_daemon',
  'waiting_permission',
]);

export const isProgress = (state: UpdateState) => !preUpdate.has(state.phase);
export const isActive = (state: UpdateState) => active.has(state.phase);

const starting = new Set<UpdateState['phase']>([
  'stopping',
  'installing',
  'starting_daemon',
  'relaunching',
  'reconciling',
  'waiting_daemon',
  'waiting_permission',
]);
const START_BUDGET_MS = 150_000;

type RestartTiming = { deadline: number; nativeSince?: number; pausedAt?: number };
export type RestartStatus = 'expected' | 'expired' | 'unknown' | null;

function nativeRestartTiming(state: UpdateState, now: number): RestartTiming | undefined {
  // The native transaction's start reflects completed pauses. A pending pause freezes elapsed time.
  if (!('start_requested_ms' in state)) return;
  const start = state.start_requested_ms;
  if (typeof start !== 'number' || !Number.isSafeInteger(start) || start < 0 || start > now) {
    return;
  }
  const deadline = start + START_BUDGET_MS;
  const elapsed = 'permission_wait_elapsed_ms' in state ? state.permission_wait_elapsed_ms : null;
  if (elapsed == null) return { nativeSince: start, deadline };
  if (
    typeof elapsed !== 'number' ||
    !Number.isSafeInteger(elapsed) ||
    elapsed < 0 ||
    elapsed > now - start
  ) {
    return;
  }
  return { nativeSince: start, deadline, pausedAt: start + elapsed };
}

const updateClock = {
  now: Date.now,
  after(ms: number, expire: () => void) {
    const timer = setTimeout(expire, ms);
    return () => clearTimeout(timer);
  },
};

export function canCancelUpdate(state: UpdateState) {
  return state.phase === 'draining' || state.phase === 'stopping';
}

export interface UpdateOperation {
  kind: 'install' | 'restart' | 'recovery';
  stopDetail?: Extract<UpdateState, { phase: 'stop_detail' }>;
  target?: UpdateTarget;
  state: UpdateState;
  since: number;
  running: boolean;
  error?: GuiError;
}

interface Updates {
  snapshot: UpdateState;
  snapshotSince: number;
  snapshotDismissed: boolean;
  lastCheck?: UpdateState;
  checkError?: GuiError;
  operation: UpdateOperation | null;
  restartStatus: RestartStatus;
  cancelPending: boolean;
  cancelError?: GuiError;
  refresh: () => Promise<void>;
  check: () => Promise<void>;
  fromTray: (result: { Ok: UpdateState } | { Err: GuiError }) => void;
  recover: (startOnly: boolean) => Promise<void>;
  install: (target: UpdateTarget, schemaAcknowledgement?: number) => Promise<void>;
  restart: (schemaAcknowledgement?: number) => Promise<void>;
  confirmSchema: (acknowledgement: number) => Promise<void>;
  cancel: () => Promise<void>;
  dismiss: () => void;
}

/** Cached state from Rust covers automatic checks and relaunch recovery. A command's Channel
 * takes precedence during its run, so a late state read cannot put the app back in an old phase. */
export function createUpdateStore(ipc: UpdateClient = client, clock = updateClock) {
  let revision = 0;
  let reading = false;
  let cancelAttempt = 0;
  return createStore<Updates>((set, get) => {
    let timing: RestartTiming | undefined;
    let cancelExpiry: (() => void) | undefined;
    let dismissedNativeId: string | undefined;
    let dismissedLegacyPhase: UpdateState['phase'] | undefined;

    function observePhase(state: UpdateState, fromChannel = false): Partial<Updates> {
      cancelExpiry?.();
      cancelExpiry = undefined;
      if (!starting.has(state.phase)) {
        timing = undefined;
        return {
          restartStatus: null,
          ...(isActive(state) ? {} : { cancelPending: false, cancelError: undefined }),
        };
      }

      const now = clock.now();
      const nativeTiming = nativeRestartTiming(state, now);
      let next = nativeTiming ?? timing;
      if (!nativeTiming && state.phase === 'waiting_daemon') {
        const since = Date.parse(state.since);
        // A changed native start already accounts for completed permission waits.
        // Repeated timestamps keep any pause observed by this WebView.
        if (Number.isFinite(since) && since <= now && since !== next?.nativeSince) {
          next = { nativeSince: since, deadline: since + START_BUDGET_MS };
        }
      } else if (!next && (state.phase === 'stopping' || state.phase === 'installing'
        || (fromChannel && state.phase === 'starting_daemon'))) {
        // Only an observed start on this command's channel can supply a provisional deadline.
        // A reopened WebView in reconciling cannot infer when the native start happened.
        next = { deadline: now + START_BUDGET_MS };
      }
      if (!next) return { restartStatus: 'unknown' };

      if (!nativeTiming) {
        if (state.phase === 'waiting_permission') {
          if (next.pausedAt === undefined && next.deadline > now) {
            next = { ...next, pausedAt: now };
          }
        } else if (next.pausedAt !== undefined) {
          next = {
            ...next,
            deadline: next.deadline + Math.max(0, now - next.pausedAt),
            pausedAt: undefined,
          };
        }
      }
      timing = next;
      const remaining = next.deadline - (next.pausedAt ?? now);
      if (remaining <= 0) return { restartStatus: 'expired' };
      if (next.pausedAt === undefined) {
        cancelExpiry = clock.after(remaining, () => set({ restartStatus: 'expired' }));
      }
      return { restartStatus: 'expected' };
    }

    function setSnapshot(snapshot: UpdateState) {
      set((prev) => {
        const current = prev.operation;
        // A command can return before relaunch. Continue from the native transaction state.
        const follow = current && !current.running && !current.error && isActive(current.state);
        const samePhase = snapshot.phase === prev.snapshot.phase;
        const sameTransaction = snapshot.transaction_id === prev.snapshot.transaction_id;
        if (!sameTransaction && !current?.running) timing = undefined;
        const dismissed = snapshot.transaction_id !== undefined
          ? snapshot.transaction_id === dismissedNativeId
          : snapshot.phase === dismissedLegacyPhase || (samePhase && sameTransaction && prev.snapshotDismissed);
        if (snapshot.transaction_id === undefined && (!samePhase || dismissed))
          dismissedLegacyPhase = undefined;
        const next = {
          snapshot,
          snapshotSince: samePhase && sameTransaction ? prev.snapshotSince : clock.now(),
          snapshotDismissed: dismissed,
          ...(!current || follow ? observePhase(snapshot.operation_error ? { phase: 'idle' } : snapshot) : {}),
        };
        if (!follow) return next;
        if (!isProgress(snapshot)) return { ...next, operation: null };
        return {
          ...next,
          operation: {
            ...current,
            state: snapshot,
            since: snapshot.phase === current.state.phase ? current.since : clock.now(),
          },
        };
      });
    }

    function canBegin() {
      const { snapshot, operation, lastCheck } = get();
      return !operation && lastCheck?.phase !== 'checking'
        && (!isActive(snapshot) || snapshot.operation_error !== undefined);
    }

    async function run(
      kind: UpdateOperation['kind'],
      target: UpdateTarget | undefined,
      call: (progress: Progress) => Promise<void>
    ) {
      if (kind === 'recovery' ? get().operation?.running : !canBegin()) return;
      revision++;
      dismissedNativeId = undefined;
      dismissedLegacyPhase = undefined;
      const state = { phase: 'confirming' } as const;
      set({
        snapshotDismissed: false,
        operation: {
          kind,
          target,
          state,
          since: clock.now(),
          running: true,
        },
        ...observePhase(state),
      });
      const runRevision = revision;
      function change(patch: (op: UpdateOperation) => Partial<UpdateOperation>, fromChannel = false) {
        const op = get().operation;
        if (!op?.running || revision !== runRevision) return;
        const next = { ...op, ...patch(op) };
        set({
          operation: next,
          ...observePhase(next.error ? { phase: 'idle' } : next.state, fromChannel),
        });
      }
      try {
        await call((state) => {
          if (state.phase === 'stop_detail') {
            change(() => ({ stopDetail: state }));
            return;
          }
          change(
            (op) => ({ state, since: op.state.phase === state.phase ? op.since : clock.now() }),
            true
          );
        });
        change(() => ({ running: false }));
        const operation = get().operation;
        if (operation && (isActive(operation.state) || operation.state.transaction_id))
          await get().refresh();
      } catch (error) {
        const failure = asGuiError(error);
        change((op) => ({
          running: false,
          error: isActive(op.state) || !isProgress(op.state) || op.state.transaction_id
            ? failure
            : undefined,
        }));
      }
    }

    return {
      snapshot: { phase: 'idle' },
      snapshotSince: 0,
      snapshotDismissed: false,
      operation: null,
      restartStatus: null,
      cancelPending: false,
      refresh: async () => {
        const { lastCheck } = get();
        if (reading || lastCheck?.phase === 'checking') return;
        reading = true;
        const readRevision = revision;
        try {
          const snapshot = await ipc.state();
          if (readRevision !== revision) return;
          setSnapshot(snapshot);
        } catch {
          // Passive checks are silent and retain the last result (§3).
        } finally {
          reading = false;
        }
      },
      check: async () => {
        if (!canBegin()) return;
        revision++;
        set({ lastCheck: { phase: 'checking' }, checkError: undefined });
        try {
          const checked = await ipc.check();
          setSnapshot(checked);
          set({ lastCheck: checked });
        } catch (error) {
          set({ lastCheck: { phase: 'check_failed' }, checkError: asGuiError(error) });
        }
      },
      fromTray: (result) => {
        if ('Err' in result) {
          set({ lastCheck: { phase: 'check_failed' }, checkError: result.Err });
        } else {
          set({ lastCheck: result.Ok, checkError: undefined });
          if (!get().operation?.running) void get().refresh();
        }
      },
      recover: (startOnly) =>
        run('recovery', undefined, (progress) => ipc.recover?.(startOnly, progress) ?? Promise.resolve()),
      install: (target, acknowledgement) =>
        run('install', target, (progress) =>
          ipc.install(target.version, acknowledgement, progress)
        ),
      restart: (acknowledgement) =>
        run('restart', undefined, (progress) => ipc.restart(acknowledgement, progress)),
      confirmSchema: async (acknowledgement) => {
        const { operation } = get();
        const request = schemaAcknowledgementRequest(operation);
        if (!operation || operation.running || !request || request.to !== acknowledgement) return;
        get().dismiss();
        if (operation.kind === 'restart') await get().restart(acknowledgement);
        else if (operation.target) await get().install(operation.target, acknowledgement);
      },
      cancel: async () => {
        const { operation, snapshot, cancelPending } = get();
        if (operation?.error || !canCancelUpdate(operation?.state ?? snapshot) || cancelPending)
          return;
        const runRevision = revision;
        const attempt = ++cancelAttempt;
        set({ cancelPending: true, cancelError: undefined });
        try {
          await ipc.cancel();
          // The request is an intent. Only a terminal progress reply ends the operation.
          void get().refresh();
        } catch (error) {
          if (get().cancelPending && revision === runRevision && attempt === cancelAttempt) {
            set({ cancelPending: false, cancelError: asGuiError(error) });
          }
        }
      },
      dismiss: () => {
        const { operation, snapshot } = get();
        const outcome = operation?.state;
        dismissedNativeId = outcome?.transaction_id ?? snapshot.transaction_id;
        if (!dismissedNativeId && outcome && !isActive(outcome) && outcome.phase !== snapshot.phase)
          dismissedLegacyPhase = outcome.phase;
        if (!operation || operation.running || (!operation.error && isActive(operation.state))) {
          set({ snapshotDismissed: true });
          return;
        }
        revision++;
        set({ operation: null, snapshotDismissed: true });
      },
    };
  });
}

export const updateStore = createUpdateStore();

export function useUpdates<T>(select: (state: Updates) => T): T {
  return useStore(updateStore, select);
}

export function restartingDaemon(state: Pick<Updates, 'restartStatus'>) {
  return state.restartStatus === 'expected';
}

export function isExpectedDaemonRestartError(error: unknown) {
  switch (reason(asGuiError(error))) {
    case 'daemon_down':
    case 'daemon_timeout':
    case 'request_in_flight':
    case 'starting':
      return true;
    default:
      return false;
  }
}

/** The warning acknowledges a specific target. Rust rechecks the signed schema before draining. */
export function schemaWarning(target: number | undefined, live: number | undefined) {
  return target !== undefined && (live === undefined || target > live);
}

export const SCHEMA_ACKNOWLEDGEMENT_REQUIRED = 'schema_acknowledgement_required';

function isSchema(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 0xffffffff;
}

/** T07's pre-seal refusal; the install adapter can return the same details after verification. */
export function schemaAcknowledgementRequest(operation: UpdateOperation | null) {
  if (!operation?.error || operation.error.code !== 'NotConfirmed') return undefined;
  if (reason(operation.error) !== SCHEMA_ACKNOWLEDGEMENT_REQUIRED) return undefined;
  const state = operation.state;
  const preflight =
    state.phase === 'confirming' ||
    (operation.kind === 'install'
      ? state.phase === 'verifying_bundle' || state.phase === 'downloading'
      : state.phase === 'draining');
  // After the seal, only the native transaction can continue or decline the recovery.
  if (!preflight) return undefined;
  const { from_schema, to_schema } = operation.error.detail as Record<string, unknown>;
  const from = from_schema == null ? undefined : from_schema;
  if (from !== undefined && !isSchema(from)) return undefined;
  if (!isSchema(to_schema) || !schemaWarning(to_schema, from)) return undefined;
  return { from, to: to_schema };
}

export const RELEASES_URL = 'https://github.com/dickwu/git-strata/releases';

/** The HTTPS destination comes from the advisory manifest, so only offer the project's tag. */
export function releaseNotesUrl(target: UpdateTarget): string | undefined {
  const expected = `${RELEASES_URL}/tag/v${encodeURIComponent(target.version)}`;
  return target.notes_url === expected ? expected : undefined;
}

/** The app already grants opener:default. No WebView navigation or shell command is needed. */
export async function openReleaseUrl(url: string) {
  const { invoke } = await import('@tauri-apps/api/core');
  await invoke('plugin:opener|open_url', { url });
}

const skillTarget = { kind: 'WebviewWindow', label: 'main' } as const;
let skillRequest = 0;

export async function updateSkillFromApp(): Promise<string> {
  const { emitTo, listen } = await import('@tauri-apps/api/event');
  const id = ++skillRequest;
  let resolve!: (output: string) => void;
  let reject!: (error: unknown) => void;
  const result = new Promise<string>((ok, fail) => { resolve = ok; reject = fail; });
  const unlisten = await listen<{ id: number; result: { Ok: string } | { Err: GuiError } }>(
    'strata://update-skill-result',
    ({ payload }) => {
      if (payload.id === id) {
        if ('Ok' in payload.result) resolve(payload.result.Ok);
        else reject(payload.result.Err);
      }
    }
  );
  const timeout = setTimeout(() => reject(new Error('Could not confirm that the skill was updated')), 120_000);
  try {
    await emitTo(skillTarget, 'git-strata-update-skill', id);
    return await result;
  } finally {
    clearTimeout(timeout);
    unlisten();
  }
}

export async function answerSchema(requestId: string, acknowledgement: number | null) {
  const { emitTo } = await import('@tauri-apps/api/event');
  await emitTo(skillTarget, 'git-strata-schema-answer', { request_id: requestId, acknowledgement });
}
