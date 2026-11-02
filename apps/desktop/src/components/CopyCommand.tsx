'use client';

import { useState } from 'react';
import { Button } from 'antd';

/** Copies a suggested command; never executes it. */
export function CopyCommand({ command }: { command: string }) {
  const [result, setResult] = useState('Copy');
  return (
    <span className="copy-command">
      <code>{command}</code>
      <Button
        size="small"
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(command);
            setResult('Copied');
          } catch {
            setResult('Select command to copy');
          }
        }}
        aria-label={`Copy ${command}`}
      >
        {result}
      </Button>
      <span className="sr-only" role="status">
        {result === 'Copy' ? '' : result}
      </span>
    </span>
  );
}
