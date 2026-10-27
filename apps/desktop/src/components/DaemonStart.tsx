'use client';

import { Alert, App, Button } from 'antd';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { copy } from '@/lib/copy';
import { downVariant, phaseText, startErrorText } from '@/lib/daemonStart';
import { call } from '@/lib/tauri';
import type { LegacyAgent } from '@/lib/types';
import { useDaemonStart } from '@/stores/daemonStart';
import { Inline } from './Inline';
import css from './DaemonStart.module.css';

const s = copy.start;

/** Which launch agent is there; asked only while the down notice is on screen. */
export function useLegacyAgent() {
  return useQuery<LegacyAgent>({
    queryKey: ['legacyAgent'],
    queryFn: () => call<LegacyAgent>('legacy_agent'),
    retry: false,
    staleTime: 0,
  });
}

/**
 * The down notice body (design §6.1, S0): a Start daemon button that kickstarts or
 * bootstraps the launchd agent and waits for its socket, with the terminal command kept as
 * a fallback. No plist means setup is needed, and no button. The GUI never stops the daemon.
 */
export function DaemonStart({ agent }: { agent: LegacyAgent | undefined }) {
  const { running, progress, error, start } = useDaemonStart();
  const qc = useQueryClient();
  const { message } = App.useApp();
  const err = error ? startErrorText(error) : null;
  const setup = downVariant(agent) === 'setup' || Boolean(err?.setup);
  const plist = err?.setup ?? agent?.plist ?? '';

  const onStart = async () => {
    const done = await start();
    if (done) {
      message.success(s.done);
      await qc.invalidateQueries();
    }
  };

  if (setup) {
    return (
      <div className={css.body}>
        <span>
          <Inline text={s.setup.hint(plist)} />
        </span>
        <span className={css.fallback}>
          <Inline text={s.fallback} />
        </span>
      </div>
    );
  }

  return (
    <div className={css.body}>
      <div className={css.row}>
        <Button type="primary" onClick={onStart} loading={running} disabled={running}>
          {err ? s.retry : s.button}
        </Button>
        <span className={css.progress} role="status" aria-live="polite">
          {running && (
            <>
              <span className={css.pulse} aria-hidden />
              {progress ? phaseText(progress) : s.phase.kickstart}
            </>
          )}
        </span>
      </div>
      {err && !running && (
        <div className={css.error} role="alert">
          <span className={css.errorTitle}>
            <Inline text={err.title} />
          </span>
          {err.hint && (
            <span>
              <Inline text={err.hint} />
            </span>
          )}
          {err.output && <pre className={css.output}>{err.output}</pre>}
        </div>
      )}
      <span className={css.fallback}>
        <Inline text={s.fallback} />
      </span>
    </div>
  );
}

/** The whole daemon-down notice: the title follows the variant, the body is above. */
export function DaemonDownAlert() {
  const agent = useLegacyAgent();
  const err = useDaemonStart((x) => x.error);
  const setup = downVariant(agent.data) === 'setup' || Boolean(err && startErrorText(err).setup);
  return (
    <Alert
      type="warning"
      showIcon
      title={setup ? s.setup.title : copy.daemon.down}
      description={<DaemonStart agent={agent.data} />}
    />
  );
}
