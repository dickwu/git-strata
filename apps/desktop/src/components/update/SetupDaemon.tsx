'use client';

import { Alert, Button, Checkbox, Modal, Space } from 'antd';
import { useState } from 'react';
import { asGuiError, inTauri, reason } from '@/lib/tauri';
import type { GuiError, UpdateCommandArgs, UpdateCommandResults, UpdateState } from '@/lib/types';
import { CliLink } from './CliLink';

interface SchemaWarning {
  reviewId: string;
  target: number;
  from: number | null;
}

function readWarning(error: GuiError): SchemaWarning | null {
  if (
    reason(error) !== 'setup_schema_acknowledgement' ||
    !error.detail ||
    typeof error.detail !== 'object'
  ) {
    return null;
  }
  const detail = error.detail as {
    review_id?: unknown;
    target_schema?: unknown;
    from_schema?: unknown;
  };
  if (typeof detail.review_id !== 'string' || typeof detail.target_schema !== 'number') return null;
  return {
    reviewId: detail.review_id,
    target: detail.target_schema,
    from: typeof detail.from_schema === 'number' ? detail.from_schema : null,
  };
}

function progressText(phase: UpdateState['phase']): string {
  switch (phase) {
    case 'starting_daemon':
      return 'Starting the daemon…';
    case 'waiting_daemon':
      return 'Waiting for the bundled daemon to answer…';
    case 'waiting_permission':
      return 'Waiting for macOS permission (Files and Folders).';
    default:
      return 'Preparing daemon setup…';
  }
}

export function SetupDaemon({ onReady }: { onReady?: () => void } = {}) {
  const [busy, setBusy] = useState(false);
  const [phase, setPhase] = useState<UpdateState['phase']>('idle');
  const [error, setError] = useState<string | null>(null);
  const [warning, setWarning] = useState<SchemaWarning | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [done, setDone] = useState(false);

  async function setup(review?: SchemaWarning) {
    setBusy(true);
    setError(null);
    setPhase('idle');
    setWarning(null);
    try {
      if (!inTauri()) throw new Error('Open GIT STRATA to set up the daemon.');
      const { Channel, invoke } = await import('@tauri-apps/api/core');
      const onProgress = new Channel<UpdateState>();
      onProgress.onmessage = (update) => setPhase(update.phase);
      const args: UpdateCommandArgs['daemon_setup'] = {
        onProgress,
        schemaAcknowledgement: review?.target,
      };
      await invoke<UpdateCommandResults['daemon_setup']>('daemon_setup', {
        ...args,
        setupReviewId: review?.reviewId,
      });
    } catch (cause) {
      const problem = asGuiError(cause);
      const migration = readWarning(problem);
      setWarning(migration);
      setAcknowledged(false);
      if (!migration && reason(problem) !== 'setup_cancelled') {
        setError(problem.message || 'Daemon setup failed.');
      }
      return;
    } finally {
      setBusy(false);
    }
    setWarning(null);
    setDone(true);
    onReady?.();
  }

  return (
    <Space orientation="vertical" align="start">
      {done ? (
        <>
          <output>The bundled daemon is running.</output>
          <CliLink />
        </>
      ) : (
        <Button type="primary" loading={busy} onClick={() => void setup()}>
          Set up daemon
        </Button>
      )}
      {busy && <output>{progressText(phase)}</output>}
      {error && <Alert type="error" showIcon title={error} />}
      <Modal
        title="Confirm store migration"
        open={warning !== null}
        destroyOnHidden
        okText="Set up daemon"
        cancelText="Cancel"
        okButtonProps={{ disabled: !acknowledged }}
        onCancel={() => {
          setWarning(null);
          setAcknowledged(false);
        }}
        onOk={() => {
          if (warning && acknowledged) void setup(warning);
        }}
      >
        {warning && (
          <>
            <p>
              {warning.from === null
                ? `The current store version could not be verified. The daemon may upgrade the store to v${warning.target}.`
                : `The daemon will upgrade the store from v${warning.from} to v${warning.target}.`}
              {' The daemon writes a verified backup in store/backups/ before migrating. To use a version that cannot open the upgraded store, restore that backup by hand.'}
            </p>
            <Checkbox
              checked={acknowledged}
              disabled={busy}
              onChange={(event) => setAcknowledged(event.target.checked)}
            >
              I understand
            </Checkbox>
          </>
        )}
      </Modal>
    </Space>
  );
}
