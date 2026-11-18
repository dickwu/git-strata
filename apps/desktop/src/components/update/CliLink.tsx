'use client';

import { Alert, Button, Descriptions, Space } from 'antd';
import { useState } from 'react';
import { asGuiError, inTauri } from '@/lib/tauri';
import type { UpdateCommandResults } from '@/lib/types';

interface CliLabel {
  path: string;
  label: string;
}

export function CliLink({
  labels,
  onInstalled,
}: {
  labels?: readonly CliLabel[];
  onInstalled?: () => void;
} = {}) {
  const [busy, setBusy] = useState(false);
  const [installed, setInstalled] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function install() {
    setBusy(true);
    setInstalled(false);
    setError(null);
    try {
      if (!inTauri()) throw new Error('Open GIT STRATA to install its command-line tools.');
      const { invoke } = await import('@tauri-apps/api/core');
      await invoke<UpdateCommandResults['cli_link_install']>('cli_link_install');
    } catch (cause) {
      setError(asGuiError(cause).message || 'The command links could not be installed.');
      return;
    } finally {
      setBusy(false);
    }
    setInstalled(true);
    onInstalled?.();
  }

  return (
    <Space orientation="vertical" align="start">
      {labels && labels.length > 0 && (
        <Descriptions
          size="small"
          column={1}
          items={labels.map(({ path, label }) => ({ key: path, label: path, children: label }))}
        />
      )}
      <Button loading={busy} onClick={() => void install()}>
        Install command-line tool
      </Button>
      {installed && <output>git-strata and git-stratad are linked to this app.</output>}
      {error && <Alert type="error" showIcon title={error} />}
    </Space>
  );
}
