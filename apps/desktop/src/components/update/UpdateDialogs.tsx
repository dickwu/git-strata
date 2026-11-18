'use client';

import { useState, type ReactNode } from 'react';
import { App, Button, Checkbox, Modal, Progress } from 'antd';
import { useNow } from '@/hooks/useNow';
import { asGuiError, reason } from '@/lib/tauri';
import type { GuiError, UpdateState, UpdateTarget } from '@/lib/types';
import {
  canCancelUpdate,
  isActive,
  openReleaseUrl,
  releaseNotesUrl,
  RELEASES_URL,
  SCHEMA_ACKNOWLEDGEMENT_REQUIRED,
  schemaWarning,
  type RestartStatus,
  type UpdateOperation,
} from '@/lib/update';
import { DAEMON_LOG, operationErrorCopy, SKILL_COMMAND, updateCopy } from '@/lib/updateCopy';
import { CopyCommand } from '../CopyCommand';
import css from './UpdateDialog.module.css';

function ReleaseLink({ url, children }: { url: string; children: ReactNode }) {
  const { message } = App.useApp();
  return (
    <Button
      type="link"
      href={url}
      onClick={(e) => {
        e.preventDefault();
        void openReleaseUrl(url).catch(() => message.error("Couldn't open the release page"));
      }}
    >
      {children}
    </Button>
  );
}

interface ConfirmProps {
  appVersion?: string;
  daemonVersion?: string;
  liveSchema?: number;
  activeSessions?: number;
  target: UpdateTarget;
  eligibility?: InstallEligibility;
  missingDrain?: boolean;
  onOpenVersions?: () => void;
  onLater: () => void;
  onSkip: () => void;
  onUpdate: (schemaAcknowledgement?: number) => void;
}

/** Presentation of the native install preflight for a specific offer. */
export type InstallEligibility = { version: string } & (
  | { kind: 'eligible' }
  | { kind: 'move' }
  | { kind: 'setup' }
  | { kind: 'refused'; message: string }
);

function manualDownloadUrl(target: UpdateTarget) {
  // T04 computes this from the installed version. Its additive field is absent on T00.
  if (!('manual_download_url' in target) || typeof target.manual_download_url !== 'string')
    return undefined;
  return diskImageUrl(target.version, target.manual_download_url);
}

function diskImageUrl(target: string, reported: unknown) {
  const version = encodeURIComponent(target);
  const expected = `${RELEASES_URL}/download/v${version}/Git-Strata_${version}_arm64.dmg`;
  return reported === expected ? expected : RELEASES_URL;
}

export function DaemonRestartNotice() {
  return (
    <p>
      The daemon restarts once it is idle. This can take a few minutes. While it drains and
      restarts, writes get “busy, draining,” then agents that call git-strata get “daemon not
      running” and tell you. Hook checkpoints made then are not recorded.
    </p>
  );
}

function MigrationWarning({
  from,
  to,
  previousVersion,
  restart,
  acknowledged,
  onChange,
}: {
  from?: number;
  to: number;
  previousVersion?: string;
  restart?: boolean;
  acknowledged: boolean;
  onChange: (checked: boolean) => void;
}) {
  const action = restart ? 'Restarting the daemon' : 'This update';
  return (
    <>
      <p className={css.warning}>
        {from === undefined
          ? `The current store schema is unknown. ${restart ? 'The daemon restart' : action} may upgrade it to v${to}.`
          : `${action} upgrades the store from v${from} to v${to}.`}{' '}
        Before upgrading the store, the daemon writes a verified backup to{' '}
        <code>store/backups/</code>. After a store upgrade,{' '}
        {previousVersion ?? 'the previous version'} cannot open the store. Going back means
        restoring the backup by hand.
      </p>
      <Checkbox checked={acknowledged} onChange={(e) => onChange(e.target.checked)}>
        I understand
      </Checkbox>
    </>
  );
}

type SchemaAcknowledgementProps = {
  from?: number;
  to: number;
  previousVersion?: string;
  action: 'Update' | 'Restart daemon';
  laterText?: string;
  onLater: () => void;
  onConfirm: (acknowledgement: number) => void;
};

export function SchemaAcknowledgementContent({
  from,
  to,
  previousVersion,
  action,
  laterText = 'Later',
  onLater,
  onConfirm,
}: SchemaAcknowledgementProps) {
  const [acknowledged, setAcknowledged] = useState(false);
  return (
    <div className={css.stack}>
      <MigrationWarning
        from={from}
        to={to}
        previousVersion={previousVersion}
        restart={action === 'Restart daemon'}
        acknowledged={acknowledged}
        onChange={setAcknowledged}
      />
      <div className={css.actions}>
        <Button onClick={onLater}>{laterText}</Button>
        <Button
          type="primary"
          disabled={!acknowledged}
          onClick={() => {
            if (!acknowledged) return;
            setAcknowledged(false);
            onConfirm(to);
          }}
        >
          {action}
        </Button>
      </div>
    </div>
  );
}

export function SchemaAcknowledgementDialog(props: SchemaAcknowledgementProps) {
  return (
    <Modal title="Confirm store upgrade" open footer={null} onCancel={props.onLater}>
      <SchemaAcknowledgementContent {...props} />
    </Modal>
  );
}

/** The native continuation owns the stopped transaction and consumes one decision. */
export interface CancelAfterStopConsent {
  id: string;
  from?: number;
  to: number;
  previousVersion?: string;
  decide: (acknowledgement: number | null) => void;
}

export function CancelAfterStopDialog({ consent }: { consent: CancelAfterStopConsent }) {
  return (
    <SchemaAcknowledgementDialog
      key={consent.id}
      from={consent.from}
      to={consent.to}
      previousVersion={consent.previousVersion}
      action="Restart daemon"
      laterText="Keep daemon stopped"
      onLater={() => consent.decide(null)}
      onConfirm={consent.decide}
    />
  );
}

export function ConfirmUpdateContent({
  appVersion,
  daemonVersion,
  liveSchema,
  activeSessions,
  target,
  eligibility,
  missingDrain,
  onOpenVersions,
  onLater,
  onSkip,
  onUpdate,
}: ConfirmProps) {
  const [acknowledged, setAcknowledged] = useState(false);
  const manual = manualDownloadUrl(target);
  const checked = eligibility?.version === target.version ? eligibility : undefined;
  if (manual || missingDrain || checked?.kind !== 'eligible') {
    const needsMove = !missingDrain && checked?.kind === 'move';
    const needsSetup = !missingDrain && checked?.kind === 'setup';
    let message: ReactNode =
      'GIT STRATA couldn’t verify that this update can be installed from the app.';
    if (manual) {
      message = (
        <>
          Update by hand.{' '}
          <ReleaseLink url={manual}>
            {manual === RELEASES_URL ? 'Find the disk image on GitHub' : 'Download the disk image'}
          </ReleaseLink>
        </>
      );
    } else if (missingDrain) {
      message = 'This daemon can’t be updated safely from the app.';
    } else if (needsMove) {
      message = 'Move the daemon into the app first.';
    } else if (needsSetup) {
      message = 'Set up the daemon first.';
    } else if (checked?.kind === 'refused') {
      message = checked.message;
    }
    return (
      <div className={css.stack}>
        <p>{message}</p>
        <div className={css.actions}>
          <Button onClick={onLater}>Close</Button>
          {!manual && (needsMove || needsSetup) && onOpenVersions && (
            <Button type="primary" onClick={onOpenVersions}>
              Open Daemon panel
            </Button>
          )}
        </div>
      </div>
    );
  }
  const schema = target.store_schema;
  const warn = schema !== undefined && schemaWarning(schema, liveSchema);
  const notesUrl = releaseNotesUrl(target);
  return (
    <div className={css.stack}>
      <p>
        App {appVersion ?? '?'} → {target.version} · daemon {daemonVersion ?? '?'} →{' '}
        {target.version} · CLI follows the app
      </p>
      {target.notes && <pre className={css.notes}>{target.notes}</pre>}
      {notesUrl && <ReleaseLink url={notesUrl}>Release notes</ReleaseLink>}
      <DaemonRestartNotice />
      <p>
        {activeSessions === undefined
          ? 'Active session count unavailable'
          : `${activeSessions} active ${activeSessions === 1 ? 'session' : 'sessions'}`}
      </p>
      {warn && (
        <MigrationWarning
          from={liveSchema}
          to={schema}
          previousVersion={appVersion}
          acknowledged={acknowledged}
          onChange={setAcknowledged}
        />
      )}
      <div className={css.actions}>
        <Button type="text" onClick={onSkip}>
          Skip this version
        </Button>
        <Button onClick={onLater}>Later</Button>
        <Button
          type="primary"
          disabled={warn && !acknowledged}
          onClick={() => onUpdate(warn && acknowledged ? schema : undefined)}
        >
          Update
        </Button>
      </div>
    </div>
  );
}

export function ConfirmUpdateDialog(props: ConfirmProps) {
  return (
    <Modal
      title={`Update GIT STRATA to ${props.target.version}`}
      open
      destroyOnHidden
      footer={null}
      onCancel={props.onLater}
    >
      <ConfirmUpdateContent
        key={`${props.target.version}:${props.target.store_schema}:${props.liveSchema}`}
        {...props}
      />
    </Modal>
  );
}

function DrainWork({ state }: { state: Extract<UpdateState, { phase: 'draining' }> }) {
  if (!state.inflight.length && !state.background.length) return null;
  return (
    <dl>
      {state.inflight.length > 0 && (
        <>
          <dt>Requests in progress</dt>
          {state.inflight.map((job, i) => (
            <dd key={i}>
              {[job.method, job.repo, job.session].filter(Boolean).join(' · ')}
              {job.phase === 'confirming' && ' · Waiting for someone to answer a Touch ID prompt'}
            </dd>
          ))}
        </>
      )}
      {state.background.length > 0 && (
        <>
          <dt>Background work</dt>
          {state.background.map((job, i) => (
            <dd key={i}>{[job.method, job.repo, job.session].filter(Boolean).join(' · ')}</dd>
          ))}
        </>
      )}
    </dl>
  );
}

export interface RecoveryActions {
  start?: () => void;
  rollback?: { version: string; run: () => void };
}

type ProgressProps = {
  state: UpdateState;
  appVersion?: string;
  operation: UpdateOperation | null;
  restartStatus?: RestartStatus;
  slowStop?: boolean;
  cancelPending?: boolean;
  cancelError?: GuiError;
  onKeepWaiting: () => void;
  onCancel?: () => void;
  onRetry?: () => void;
  onClose?: () => void;
  /** Supplied only when native code verified the job and rollback eligibility. */
  recovery?: RecoveryActions;
  updateSkill?: () => Promise<string>;
};

export function UpdateProgressContent({
  state,
  appVersion,
  operation,
  restartStatus,
  slowStop,
  cancelPending,
  cancelError,
  onKeepWaiting,
  onCancel,
  onRetry,
  onClose,
  recovery,
  updateSkill,
}: ProgressProps) {
  const { message } = App.useApp();
  const [skillRunning, setSkillRunning] = useState(false);
  const [skillResult, setSkillResult] = useState<{ output: string; error?: boolean }>();
  const error = operation ? operationErrorCopy(operation) : undefined;
  const manualUrl =
    operation?.target && operation.error && reason(operation.error) === 'below_min_updatable_from'
      ? diskImageUrl(
          operation.target.version,
          (operation.error.detail as { url?: unknown } | null)?.url
        )
      : undefined;
  const expired = restartStatus === 'expired';
  const failed =
    error ||
    expired ||
    state.phase === 'not_starting' ||
    state.phase === 'daemon_failed' ||
    state.phase === 'restore_failed' ||
    state.phase === 'cancel_blocked' ||
    state.phase === 'bootout_failed' ||
    state.phase === 'fence_uncertain' ||
    state.phase === 'install_failed_unchanged' ||
    state.phase === 'restored';
  const diagnostics =
    state.phase === 'daemon_failed' ||
    state.phase === 'not_starting' ||
    state.phase === 'bootout_failed' ||
    state.phase === 'install_failed_unchanged' ||
    state.phase === 'restore_failed' ||
    state.phase === 'fence_uncertain';
  const manualDmg =
    state.phase === 'daemon_failed' ||
    state.phase === 'restore_failed' ||
    state.phase === 'cancel_blocked';
  const canCancel =
    !error && onCancel && canCancelUpdate(state) && (state.phase === 'draining' || slowStop);
  return (
    <div className={css.stack} data-phase={state.phase}>
      <p role={failed ? 'alert' : 'status'} className={failed ? css.error : undefined}>
        {error ??
          (expired
            ? 'The daemon didn’t respond within the restart window'
            : slowStop
              ? 'The daemon is taking long to stop'
              : updateCopy(state, appVersion, operation?.target?.version, operation?.kind))}
      </p>
      {(expired || restartStatus === 'unknown') && (
        <p>You can close this dialog while GIT STRATA continues checking the restart.</p>
      )}
      {state.phase === 'downloading' && state.pct != null && !error && (
        <Progress percent={state.pct} showInfo={false} />
      )}
      {state.phase === 'draining' && <DrainWork state={state} />}
      {cancelPending && isActive(state) && <output>Cancelling…</output>}
      {cancelError && (
        <p role="alert" className={css.error}>
          {cancelError.message}
        </p>
      )}
      {state.phase === 'relaunching' && <p>Open GIT STRATA again if it does not reopen.</p>}
      {state.phase === 'waiting_permission' && (
        <p>Allow access in System Settings → Privacy &amp; Security → Files and Folders.</p>
      )}
      {state.phase === 'daemon_failed' && (
        <>
          <p>{state.reason}</p>
          <p>
            Store migration errors are shown in the Daemon panel. Verified backups are in{' '}
            <code>store/backups/</code>.
          </p>
        </>
      )}
      {state.phase === 'fence_uncertain' && (
        <p>
          The daemon’s launch agent may still be disabled. Copy diagnostics before seeking recovery
          help.
        </p>
      )}
      {diagnostics && (
        <div>
          <code className={css.code}>{DAEMON_LOG}</code>{' '}
          <Button
            onClick={async () => {
              const failure = state.phase === 'daemon_failed' ? state.reason : state.phase;
              try {
                await navigator.clipboard.writeText(
                  [
                    updateCopy(state, appVersion, operation?.target?.version, operation?.kind),
                    `App ${appVersion ?? '?'}`,
                    failure,
                    DAEMON_LOG,
                  ].join('\n')
                );
                message.success('Diagnostics copied');
              } catch {
                message.error("Couldn't copy diagnostics");
              }
            }}
          >
            Copy diagnostics
          </Button>
        </div>
      )}
      {manualDmg && (
        <p>
          Download a matching or newer disk image from{' '}
          <ReleaseLink url={RELEASES_URL}>GitHub releases</ReleaseLink>
          and reinstall GIT STRATA.
        </p>
      )}
      {manualUrl && (
        <ReleaseLink url={manualUrl}>
          {manualUrl === RELEASES_URL ? 'Find the disk image on GitHub' : 'Download the disk image'}
        </ReleaseLink>
      )}
      {state.phase === 'done' && operation?.kind !== 'restart' && (
        <div>
          {updateSkill ? (
            <Button
              loading={skillRunning}
              onClick={async () => {
                setSkillRunning(true);
                setSkillResult(undefined);
                try {
                  const output = await updateSkill();
                  setSkillResult({ output: output || 'git-strata skill updated' });
                } catch (error) {
                  setSkillResult({ output: asGuiError(error).message, error: true });
                } finally {
                  setSkillRunning(false);
                }
              }}
            >
              Update git-strata skill
            </Button>
          ) : (
            <p>Update the git-strata skill from Terminal</p>
          )}
          <CopyCommand command={SKILL_COMMAND} />
          {skillResult && (
            <pre className={css.notes} role={skillResult.error ? 'alert' : 'status'}>
              {skillResult.output}
            </pre>
          )}
        </div>
      )}
      <div className={css.actions}>
        {state.phase === 'daemon_failed' && recovery?.start && (
          <Button onClick={recovery.start}>Start daemon</Button>
        )}
        {state.phase === 'daemon_failed' && recovery?.rollback && (
          <Button onClick={recovery.rollback.run}>Roll back to {recovery.rollback.version}</Button>
        )}
        {slowStop && !cancelPending && <Button onClick={onKeepWaiting}>Keep waiting</Button>}
        {canCancel && (
          <Button onClick={onCancel} disabled={cancelPending}>
            Cancel
          </Button>
        )}
        {onClose && <Button onClick={onClose}>Close</Button>}
        {onRetry && (
          <Button type="primary" onClick={onRetry}>
            Retry
          </Button>
        )}
      </div>
    </div>
  );
}

export function canDismissUpdateProgress(
  state: UpdateState,
  operation: UpdateOperation | null,
  restartStatus: RestartStatus
) {
  if (
    restartStatus === 'expired' ||
    restartStatus === 'unknown' ||
    state.phase === 'waiting_permission'
  ) {
    return true;
  }
  return !operation?.running && (operation !== null || !isActive(state));
}

export function UpdateProgressDialog({
  state,
  since,
  operation,
  restartStatus,
  appVersion,
  cancelPending,
  cancelError,
  onDismiss,
  onCancel,
  onRetry,
  recovery,
  updateSkill,
}: {
  state: UpdateState;
  since: number;
  operation: UpdateOperation | null;
  restartStatus: RestartStatus;
  appVersion?: string;
  cancelPending: boolean;
  cancelError?: GuiError;
  onDismiss: () => void;
  onCancel: () => void;
  onRetry?: () => void;
  recovery?: RecoveryActions;
  updateSkill?: () => Promise<string>;
}) {
  const now = useNow(1000);
  const [waitedUntil, setWaitedUntil] = useState(0);
  const slowStop =
    state.phase === 'stopping' &&
    !operation?.error &&
    now >= Math.max(since + 120_000, waitedUntil);
  const canClose = canDismissUpdateProgress(state, operation, restartStatus);
  return (
    <Modal
      open
      title={operation?.kind === 'restart' ? 'Restart daemon' : 'GIT STRATA update'}
      closable={canClose}
      keyboard={canClose}
      mask={{ closable: false }}
      onCancel={canClose ? onDismiss : undefined}
      footer={null}
    >
      <UpdateProgressContent
        state={state}
        appVersion={appVersion}
        operation={operation}
        restartStatus={restartStatus}
        slowStop={slowStop}
        cancelPending={cancelPending}
        cancelError={cancelError}
        onKeepWaiting={() => setWaitedUntil(Date.now() + 120_000)}
        onCancel={onCancel}
        onRetry={canClose ? onRetry : undefined}
        onClose={canClose ? onDismiss : undefined}
        recovery={canClose ? recovery : undefined}
        updateSkill={canClose ? updateSkill : undefined}
      />
    </Modal>
  );
}

export function UpdateSettingsContent({
  state,
  automatic,
  saving,
  error,
  onAutomaticChange,
  onCheck,
  onUpdate,
}: {
  state: UpdateState;
  automatic?: boolean;
  saving?: boolean;
  error?: string;
  onAutomaticChange?: (enabled: boolean) => void;
  onCheck: () => void;
  onUpdate?: () => void;
}) {
  return (
    <div className={css.stack}>
      <Checkbox
        checked={automatic ?? true}
        disabled={!onAutomaticChange || saving}
        onChange={(e) => onAutomaticChange?.(e.target.checked)}
      >
        Check for updates automatically
      </Checkbox>
      {error ? <p role="alert">{error}</p> : saving && <output>Saving preference…</output>}
      <p>
        Update checks make one HTTPS request to GitHub. GitHub receives your IP address and the
        updater’s User-Agent. GIT STRATA collects no telemetry.
      </p>
      {state.phase !== 'idle' && (
        <p role={state.phase === 'check_failed' ? 'alert' : 'status'}>{updateCopy(state)}</p>
      )}
      <div className={css.actions}>
        <Button loading={state.phase === 'checking'} onClick={onCheck}>
          Check for updates…
        </Button>
        {state.phase === 'available' && onUpdate && (
          <Button type="primary" onClick={onUpdate}>
            {updateCopy(state)}
          </Button>
        )}
      </div>
    </div>
  );
}

export function retryableDownload(operation: UpdateOperation | null): boolean {
  return Boolean(
    operation?.error &&
    reason(operation.error) !== 'not_implemented' &&
    reason(operation.error) !== 'below_min_updatable_from' &&
    reason(operation.error) !== SCHEMA_ACKNOWLEDGEMENT_REQUIRED &&
    (operation.state.phase === 'downloading' || operation.state.phase === 'verifying_bundle')
  );
}
