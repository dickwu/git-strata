'use client';

import { Alert, Button, Checkbox, Modal } from 'antd';
import { useRef, useState } from 'react';
import { asGuiError, inTauri, reason } from '@/lib/tauri';
import type { GuiError, UpdateCommandArgs, UpdateState } from '@/lib/types';

export type MoveDaemonMode = 'formula' | 'program_missing' | 'resume' | 'unavailable';

type MoveAction = 'continue' | 'cancel' | 'check_repair';
type ActiveMove = {
  cancelRequested: boolean;
  sendCancel: (() => Promise<void>) | null;
};

const title = 'Move the daemon into the app';

function progressText(state: UpdateState | null): string {
  switch (state?.phase) {
    case 'reconciling':
      return 'Checking the saved move…';
    case 'draining': {
      const pending = state.inflight.length;
      const background = state.background.length;
      const confirmation = state.inflight.some((entry) => entry.phase === 'confirming');
      return [
        `Waiting for ${pending} requests and ${background} background tasks.`,
        confirmation && 'A request is waiting for a Touch ID or password confirmation.',
      ].filter(Boolean).join(' ');
    }
    case 'stopping':
      return 'Waiting for launchd to unload the daemon…';
    case 'installing':
      return 'Installing the bundled daemon’s LaunchAgent…';
    case 'starting_daemon':
    case 'waiting_daemon':
      return 'Waiting for the daemon to answer…';
    case 'waiting_permission':
      return 'Waiting for macOS Files and Folders permission…';
    default:
      return 'Checking the current LaunchAgent…';
  }
}

function names(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item) => typeof item === 'string') : [];
}

function detailOf(error: GuiError | null): Record<string, unknown> {
  const detail = error?.detail;
  return detail && typeof detail === 'object' ? (detail as Record<string, unknown>) : {};
}

/** The Daemon panel supplies the launch-agent eligibility and any saved Move phase. */
export function MoveDaemon({
  mode,
  onMoved,
}: {
  mode: MoveDaemonMode;
  onMoved?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [activeAction, setActiveAction] = useState<MoveAction | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [state, setState] = useState<UpdateState | null>(null);
  const [error, setError] = useState<GuiError | null>(null);
  const [schemaAck, setSchemaAck] = useState<number | null>(null);
  const [confirmedServiceExit, setConfirmedServiceExit] = useState(false);
  const inFlight = useRef<ActiveMove | null>(null);
  const kind = error && reason(error);
  const detail = detailOf(error);
  const from = typeof detail.from_schema === 'number' ? detail.from_schema : null;
  const target = kind === 'move_schema_ack_required' && typeof detail.to_schema === 'number'
    ? detail.to_schema
    : null;
  const review = kind === 'move_plist_decision';
  const incomplete = kind === 'move_incomplete';
  const resume = mode === 'resume';
  const repair = incomplete && detail.retry_move !== true;
  const needsServiceExit = repair && detail.confirm_service_exit === true;
  const done = state?.phase === 'done';
  const cancelled = state?.phase === 'cancelled';
  const restored = state?.phase === 'restored';
  const finished = done || cancelled || restored;
  const running = activeAction !== null;
  const canCancel = activeAction === 'continue'
    && (state?.phase === 'checking' || state?.phase === 'reconciling' || state?.phase === 'draining');

  if (mode === 'unavailable' && !open) return null;

  async function move(action: MoveAction) {
    if (inFlight.current || finished
      || (action === 'continue' && target !== null && schemaAck !== target)
      || (action === 'check_repair' && needsServiceExit && !confirmedServiceExit)) return;
    const active: ActiveMove = { cancelRequested: action === 'cancel', sendCancel: null };
    inFlight.current = active;
    setActiveAction(action);
    setCancelling(action === 'cancel');
    setState(null);
    setError(null);
    setConfirmedServiceExit(false);
    try {
      const [{ Channel, invoke }, { emitTo }] = await Promise.all([
        import('@tauri-apps/api/core'),
        import('@tauri-apps/api/event'),
      ]);
      const channel = new Channel<UpdateState>();
      const send = (choice: MoveAction) => emitTo(
        { kind: 'WebviewWindow', label: 'main' },
        `git-strata-move-action-${channel.id}`,
        choice,
      );
      channel.onmessage = (update) => {
        setState(update);
        if (update.phase === 'done') onMoved?.();
        if (update.phase === 'checking' && !active.sendCancel) {
          // The command sends this state once its action listener is ready.
          active.sendCancel = () => send('cancel');
          void send(active.cancelRequested ? 'cancel' : action).catch((e: unknown) => {
            if (inFlight.current === active) setError(asGuiError(e));
          });
        }
      };
      const args: UpdateCommandArgs['daemon_move_into_app'] = { onProgress: channel };
      if (schemaAck !== null) args.schemaAcknowledgement = schemaAck;
      await invoke<void>('daemon_move_into_app', { ...args });
    } catch (e) {
      setError(asGuiError(e));
    } finally {
      inFlight.current = null;
      setActiveAction(null);
      setCancelling(false);
    }
  }

  async function cancelActive() {
    const active = inFlight.current;
    if (!active || active.cancelRequested) return;
    active.cancelRequested = true;
    setCancelling(true);
    try {
      await active.sendCancel?.();
    } catch (e) {
      if (inFlight.current === active) {
        active.cancelRequested = false;
        setError(asGuiError(e));
        setCancelling(false);
      }
    }
  }

  const buttonText = repair ? 'Check repair'
    : review ? 'Recheck and move'
    : incomplete ? 'Try the move again'
    : resume ? 'Resume move'
    : 'Move daemon';

  return (
    <>
      <Button disabled={running || !inTauri()} onClick={() => {
        if (finished) {
          setState(null);
          setError(null);
          setSchemaAck(null);
        }
        setOpen(true);
      }}>
        {resume ? 'Resume daemon move' : title}
      </Button>
      <Modal
        open={open}
        destroyOnHidden
        title={resume ? 'Resume daemon move' : title}
        onCancel={() => setOpen(false)}
        keyboard={!running}
        closable={!running}
        mask={{ closable: false }}
        footer={
          running ? (
            canCancel && (
              <Button onClick={cancelActive} loading={cancelling}>Cancel move</Button>
            )
          ) : (
            <>
              <Button onClick={() => setOpen(false)}>Close</Button>
              {resume && !review && !incomplete && !finished && kind !== 'move_cancel_blocked' && (
                <Button onClick={() => move('cancel')}>Cancel saved move</Button>
              )}
              {repair && detail.can_resume === true && (
                <Button onClick={() => move('continue')}>Resume saved move</Button>
              )}
              {!finished && (
                <Button
                  type="primary"
                  disabled={(target !== null && schemaAck !== target) || (needsServiceExit && !confirmedServiceExit)}
                  onClick={() => move(repair ? 'check_repair' : 'continue')}
                >
                  {buttonText}
                </Button>
              )}
            </>
          )
        }
      >
        {running && <p aria-live="polite">{cancelling ? 'Checking whether the move can be cancelled…' : progressText(state)}</p>}
        {done && <Alert type="success" showIcon title="The app’s daemon is running." />}
        {cancelled && <Alert type="info" showIcon title="The daemon move was cancelled." />}
        {restored && (
          <Alert type="info" showIcon title={error?.message ?? 'The formula daemon is running again.'} />
        )}
        {!running && !error && !finished && (
          <p>
            {resume
              ? 'GIT STRATA will check the saved move and continue from the recorded phase.'
              : 'GIT STRATA will save a copy of the current LaunchAgent, then start the daemon from the app. Daemon writes pause while the service moves.'}
          </p>
        )}
        {target !== null && (
          <Alert
            type="warning"
            showIcon
            title={from === null ? 'The store version could not be verified.' : `Upgrade the store from v${from} to v${target}`}
            description={
              <>
                <p>
                  The bundled daemon uses store v{target}. If the store is upgraded, the daemon saves a
                  verified backup first. Going back to an older format means restoring that backup by hand.
                </p>
                <Checkbox checked={schemaAck === target} onChange={(event) => setSchemaAck(event.target.checked ? target : null)}>
                  I understand
                </Checkbox>
              </>
            }
          />
        )}
        {error && target === null && !restored && (
          <Alert
            type={review ? 'warning' : 'error'}
            showIcon
            title={error.message}
            description={
              review ? (
                <>
                  {names(detail.carry_over).length > 0 && <p>Settings that can be kept include {names(detail.carry_over).join(', ')}.</p>}
                  {names(detail.requires_review).length > 0 && (
                    <p>Review {names(detail.requires_review).join(', ')} before replacing these settings.</p>
                  )}
                  {typeof detail.print_defaults === 'string' && (
                    <p>To compare with the default settings, run <code>{detail.print_defaults}</code> in Terminal.</p>
                  )}
                  <p>
                    To continue, back up the custom settings and revert them in
                    {' '}{typeof detail.plist === 'string' ? <code>{detail.plist}</code> : 'the LaunchAgent plist'}.
                    Then choose Recheck and move.
                  </p>
                </>
              ) : incomplete ? (
                <>
                  {typeof detail.cause === 'string' && <p>{detail.cause}</p>}
                  <ol>
                    {names(detail.manual_steps).map((step) => <li key={step}>{step}</li>)}
                  </ol>
                  {needsServiceExit && (
                    <Checkbox
                      checked={confirmedServiceExit}
                      onChange={(event) => setConfirmedServiceExit(event.target.checked)}
                    >
                      I verified that no earlier launchctl command is still running
                    </Checkbox>
                  )}
                </>
              ) : undefined
            }
          />
        )}
      </Modal>
    </>
  );
}
