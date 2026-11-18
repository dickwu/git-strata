'use client';

import { useEffect, useState } from 'react';
import { App, Button, Modal } from 'antd';
import { inTauri } from '@/lib/tauri';
import type { DaemonStatus, GuiError, UpdateState, UpdateTarget } from '@/lib/types';
import { isProgress, schemaAcknowledgementRequest, useUpdates } from '@/lib/update';
import { useUpdateSettings } from '@/lib/updateSettings';
import {
  CancelAfterStopDialog,
  ConfirmUpdateDialog,
  DaemonRestartNotice,
  retryableDownload,
  SchemaAcknowledgementDialog,
  UpdateProgressDialog,
  UpdateSettingsContent,
  type CancelAfterStopConsent,
  type InstallEligibility,
  type RecoveryActions,
} from './UpdateDialogs';

export type UpdateDialog = 'confirm' | 'restart' | 'settings';

export function UpdateControls({
  dialog,
  onDialog,
  appVersion,
  daemon,
  activeSessions,
  offer,
  onSkip,
  eligibility,
  onOpenVersions,
  recovery,
  updateSkill,
  cancelConsent,
}: {
  dialog: UpdateDialog | null;
  onDialog: (dialog: UpdateDialog | null) => void;
  appVersion?: string;
  daemon?: DaemonStatus;
  activeSessions?: number;
  offer?: UpdateTarget;
  onSkip: (version: string) => void;
  eligibility?: InstallEligibility;
  onOpenVersions: () => void;
  recovery?: RecoveryActions;
  updateSkill?: () => Promise<string>;
  cancelConsent?: CancelAfterStopConsent;
}) {
  const { message } = App.useApp();
  const {
    snapshot,
    snapshotSince,
    snapshotDismissed,
    lastCheck,
    checkError,
    fromTray,
    operation,
    restartStatus,
    cancelPending,
    cancelError,
    refresh,
    check,
    install,
    restart,
    confirmSchema,
    cancel,
    dismiss,
  } = useUpdates((state) => state);
  const { automatic, connect, saving, error: settingsError, setAutomatic } = useUpdateSettings();
  const [dismissedOperation, setDismissedOperation] = useState<string>();
  useEffect(() => {
    if (!inTauri()) return;
    const disconnectSettings = connect();
    const visible = () => {
      if (document.visibilityState === 'visible') void refresh();
    };
    visible();
    const timer = setInterval(visible, 5000);
    let gone = false;
    let stopListeners: (() => void)[] = [];
    void import('@tauri-apps/api/event').then(async ({ listen }) => {
      const listeners = await Promise.all([
        listen('strata://update-state', visible),
        listen('strata://schema-pending', () => void refresh()),
        listen<{ Ok: UpdateState } | { Err: GuiError }>(
          'strata://update-check-result', ({ payload }) => fromTray(payload)
        ),
      ]);
      if (gone) listeners.forEach((stop) => stop());
      else stopListeners = listeners;
    }).catch(() => { /* Visible polling still reads native state. */ });
    document.addEventListener('visibilitychange', visible);
    return () => {
      gone = true;
      stopListeners.forEach((stop) => stop());
      disconnectSettings();
      clearInterval(timer);
      document.removeEventListener('visibilitychange', visible);
    };
  }, [connect, fromTray, refresh]);

  useEffect(() => {
    if (checkError && dialog !== 'settings') {
      void message.error(`Couldn't check for updates. ${checkError.message}`);
    }
  }, [checkError, dialog, message]);

  const state = operation?.state ?? snapshot;
  const since = operation?.since ?? snapshotSince;
  const progressKey = `${state.transaction_id ?? since}:${state.phase}`;
  const showProgress = operation
    ? progressKey !== dismissedOperation
    : isProgress(snapshot) && !snapshotDismissed;
  const close = () => {
    // A failed read can leave a native wait on screen past its deadline. Closing the
    // presentation still leaves that operation in the store and under native ownership.
    setDismissedOperation(operation ? progressKey : undefined);
    dismiss();
    onDialog(null);
  };
  if (cancelConsent) return <CancelAfterStopDialog consent={cancelConsent} />;

  const schemaRequest = schemaAcknowledgementRequest(operation);
  if (operation && !operation.running && schemaRequest) {
    return (
      <SchemaAcknowledgementDialog
        {...schemaRequest}
        previousVersion={operation.kind === 'restart' ? daemon?.version : appVersion}
        action={operation.kind === 'restart' ? 'Restart daemon' : 'Update'}
        onLater={close}
        onConfirm={(acknowledgement) => {
          onDialog(null);
          void confirmSchema(acknowledgement);
        }}
      />
    );
  }

  if (showProgress) {
    return (
      <UpdateProgressDialog
        state={state}
        since={since}
        operation={operation}
        restartStatus={restartStatus}
        appVersion={appVersion}
        cancelPending={cancelPending}
        cancelError={cancelError}
        onDismiss={close}
        onCancel={() => void cancel()}
        recovery={operation?.kind !== 'restart' ? recovery : undefined}
        updateSkill={updateSkill}
        onRetry={
          retryableDownload(operation) && offer?.version === operation?.target?.version
            ? () => {
                close();
                onDialog('confirm');
              }
            : undefined
        }
      />
    );
  }

  if (dialog === 'confirm' && offer) {
    return (
      <ConfirmUpdateDialog
        appVersion={appVersion}
        daemonVersion={daemon?.version}
        liveSchema={daemon?.store_schema}
        activeSessions={activeSessions}
        target={offer}
        eligibility={eligibility}
        missingDrain={daemon && !daemon.features?.includes('daemon.drain')}
        onOpenVersions={onOpenVersions}
        onLater={() => onDialog(null)}
        onSkip={() => {
          onSkip(offer.version);
          onDialog(null);
        }}
        onUpdate={(ack) => {
          setDismissedOperation(undefined);
          onDialog(null);
          void install(offer, ack);
        }}
      />
    );
  }

  if (dialog === 'restart') {
    if (!daemon?.features?.includes('daemon.drain')) {
      const close = () => onDialog(null);
      return (
        <Modal
          title="Restart daemon"
          open
          onCancel={close}
          footer={<Button onClick={close}>Close</Button>}
        >
          <p>This daemon can’t be restarted safely from the app.</p>
        </Modal>
      );
    }
    return (
      <Modal
        title="Restart daemon"
        open
        okText="Restart daemon"
        cancelText="Later"
        onCancel={() => onDialog(null)}
        onOk={() => {
          setDismissedOperation(undefined);
          onDialog(null);
          void restart();
        }}
      >
        <DaemonRestartNotice />
      </Modal>
    );
  }

  if (dialog === 'settings') {
    const checkState =
      lastCheck?.phase === 'checking' || lastCheck?.phase === 'check_failed'
        ? lastCheck
        : snapshot.phase === 'available' && !offer
          ? ({ phase: 'idle' } as const)
          : snapshot;
    return (
      <Modal title="Update settings" open footer={null} onCancel={() => onDialog(null)}>
        <UpdateSettingsContent
          state={checkState}
          automatic={automatic}
          saving={saving}
          error={settingsError ?? checkError?.message}
          onAutomaticChange={
            automatic === undefined ? undefined : (enabled) => void setAutomatic(enabled)
          }
          onCheck={() => void check()}
          onUpdate={offer ? () => onDialog('confirm') : undefined}
        />
      </Modal>
    );
  }
  return null;
}
