import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryClientProvider, type UseQueryResult } from '@tanstack/react-query';
import { renderToStaticMarkup } from 'react-dom/server';
import type { DaemonStatus, DuReply, UpdateState, UpdateTarget } from '@/lib/types';
import { releaseNotesUrl, schemaWarning, type UpdateOperation } from '@/lib/update';
import { DAEMON_LOG, operationErrorCopy, updateCopy } from '@/lib/updateCopy';
import { DaemonStatusContent } from '../DaemonStatusModal';
import {
  ConfirmUpdateContent,
  retryableDownload,
  SchemaAcknowledgementContent,
  UpdateProgressContent,
  UpdateSettingsContent,
  type InstallEligibility,
} from './UpdateDialogs';
import { VersionsBlock } from './VersionsBlock';

const target: UpdateTarget = { version: '0.3.1', store_schema: 8 };
const since = '2026-11-17T20:00:00Z';
const idle = { phase: 'idle' } as const;
const noop = () => {};
const confirm = {
  appVersion: '0.3.0',
  daemonVersion: '0.3.0',
  activeSessions: 2,
  eligibility: { version: target.version, kind: 'eligible' } satisfies InstallEligibility,
  onLater: noop,
  onSkip: noop,
  onUpdate: noop,
};
const progress = { operation: null, onKeepWaiting: noop, appVersion: '0.3.0' };

test('a planned stop hides the Daemon panel down banner and keeps independent errors', () => {
  const down = { code: 'Busy', message: 'Unavailable', detail: { reason: 'daemon_down' } };
  const failed = { code: 'Corrupt', message: 'The data store needs attention', detail: null };
  const modal = (error: unknown, restarting: boolean) =>
    renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <DaemonStatusContent
          status={{ isError: true, error } as UseQueryResult<DaemonStatus>}
          du={{ isError: false } as UseQueryResult<DuReply>}
          now={Date.parse(since)}
          sessions={undefined}
          socket={undefined}
          restarting={restarting}
        />
      </QueryClientProvider>
    );
  expect(modal(down, false)).toContain('Start daemon');
  const expectedRestart = modal(down, true);
  expect(expectedRestart).not.toContain('Start daemon');
  expect(expectedRestart).toContain('Daemon restarting…');
  for (const reason of ['daemon_timeout', 'request_in_flight', 'starting']) {
    const error = { code: 'Busy', message: 'Daemon unavailable', detail: { reason, seconds: 15 } };
    expect(modal(error, false)).toContain('role="alert"');
    expect(modal(error, true)).not.toContain('role="alert"');
  }
  expect(modal(failed, true)).toContain('The data store needs attention');
});

const phases = {
  idle: [idle, 'Check for updates'],
  checking: [{ phase: 'checking' }, 'Checking for updates…'],
  up_to_date: [{ phase: 'up_to_date' }, 'GIT STRATA is up to date'],
  available: [{ phase: 'available', target }, 'Update to 0.3.1'],
  check_failed: [{ phase: 'check_failed' }, "Couldn't check for updates"],
  confirming: [{ phase: 'confirming' }, 'Checking update requirements…'],
  downloading: [{ phase: 'downloading', pct: 42 }, 'Downloading 0.3.1 · 42%'],
  verifying_bundle: [{ phase: 'verifying_bundle' }, 'Verifying the downloaded app…'],
  draining: [
    { phase: 'draining', inflight: [], background: [] },
    'Waiting for the daemon to finish its work…',
  ],
  stopping: [{ phase: 'stopping' }, 'Waiting for the daemon to stop…'],
  stop_detail: [{ phase: 'stop_detail', slow: false }, 'Waiting for the daemon to stop…'],
  installing: [{ phase: 'installing' }, 'Installing the update…'],
  starting_daemon: [{ phase: 'starting_daemon' }, 'Starting the daemon…'],
  relaunching: [{ phase: 'relaunching' }, 'Reopening GIT STRATA…'],
  reconciling: [{ phase: 'reconciling' }, 'Finishing the update…'],
  waiting_daemon: [{ phase: 'waiting_daemon', since }, 'Waiting for the daemon to respond…'],
  done: [{ phase: 'done' }, 'Updated to 0.3.1'],
  daemon_failed: [
    { phase: 'daemon_failed', reason: 'store_unavailable' },
    "Daemon didn't come back after the update",
  ],
  waiting_permission: [
    { phase: 'waiting_permission' },
    'Waiting for macOS permission (Files and Folders)',
  ],
  cancelled: [{ phase: 'cancelled' }, 'Update cancelled'],
  not_starting: [{ phase: 'not_starting' }, 'Nothing installed; the daemon is not starting'],
  install_failed_unchanged: [
    { phase: 'install_failed_unchanged' },
    'Update failed; still on 0.3.0',
  ],
  restored: [{ phase: 'restored', from: '0.3.0' }, 'Update failed; restored 0.3.0'],
  restore_failed: [{ phase: 'restore_failed' }, 'GIT STRATA could not restore itself'],
  cancel_blocked: [{ phase: 'cancel_blocked' }, 'Install a matching or newer GIT STRATA'],
  bootout_failed: [{ phase: 'bootout_failed' }, 'Update failed; still on 0.3.0'],
  fence_uncertain: [{ phase: 'fence_uncertain' }, 'Could not confirm the daemon stopped'],
} satisfies { [P in UpdateState['phase']]: [Extract<UpdateState, { phase: P }>, string] };

describe('update progress', () => {
  for (const [state, copy] of Object.values(phases)) {
    test(state.phase, () => expect(updateCopy(state, '0.3.0', '0.3.1')).toBe(copy));
  }

  test('drain lists admitted and background work with cancel and no force action', () => {
    const html = renderToStaticMarkup(
      <UpdateProgressContent
        {...progress}
        onCancel={noop}
        state={{
          phase: 'draining',
          inflight: [{ method: 'publish', repo: '<repo>', phase: 'confirming', started_at: since }],
          background: [
            {
              repo: 'git-strata',
              method: 'catch_up',
              session: null,
              since: { secs_since_epoch: 0, nanos_since_epoch: 0 },
            },
          ],
        }}
      />
    );
    expect(html).toContain(
      'publish · &lt;repo&gt; · Waiting for someone to answer a Touch ID prompt'
    );
    expect(html).toContain('catch_up · git-strata');
    expect(html).toContain('>Cancel<');
    expect(html).not.toContain('Force');
  });

  test('slow stop exposes Keep waiting and Cancel', () => {
    const html = renderToStaticMarkup(
      <UpdateProgressContent {...progress} state={{ phase: 'stopping' }} slowStop onCancel={noop} />
    );
    expect(html).toContain('The daemon is taking long to stop');
    expect(html).toContain('>Keep waiting<');
    expect(html).toContain('>Cancel<');
  });

  test('a snapshot-only drain shows Cancel and disables it while cancellation is pending', () => {
    const html = renderToStaticMarkup(
      <UpdateProgressContent
        {...progress}
        state={phases.draining[0]}
        onCancel={noop}
        cancelPending
      />
    );
    expect(html).toContain('Cancelling…');
    expect(html).toMatch(/<button[^>]+disabled=""[^>]*><span>Cancel<\/span>/);
  });

  test('daemon failure exposes diagnostics, the reason and manual recovery', () => {
    const html = renderToStaticMarkup(
      <UpdateProgressContent {...progress} state={phases.daemon_failed[0]} />
    );
    expect(html).toContain('role="alert"');
    expect(html).toContain(DAEMON_LOG);
    expect(html).toContain('Copy diagnostics');
    expect(html).toContain('store_unavailable');
    expect(html).toContain('store/backups/');
    expect(html).toContain('GitHub releases');
    expect(html).not.toContain('Install update again');
    expect(html).not.toContain('Start daemon');
    expect(html).not.toContain('Roll back to');

    const eligible = renderToStaticMarkup(
      <UpdateProgressContent
        {...progress}
        state={phases.daemon_failed[0]}
        recovery={{ start: noop, rollback: { version: '0.3.0', run: noop } }}
      />
    );
    expect(eligible).toContain('Start daemon');
    expect(eligible).toContain('Roll back to 0.3.0');
  });

  test('a failed download includes the target and can be retried', () => {
    const operation: UpdateOperation = {
      kind: 'install',
      target,
      state: { phase: 'verifying_bundle' },
      since: 0,
      running: false,
      error: { code: 'InvalidInput', message: 'signature mismatch', detail: null },
    };
    const html = renderToStaticMarkup(
      <UpdateProgressContent
        {...progress}
        state={operation.state}
        operation={operation}
        onRetry={noop}
      />
    );
    expect(html).toContain('Couldn&#x27;t download 0.3.1 (signature mismatch)');
    expect(html).toContain('>Retry<');
    expect(retryableDownload(operation)).toBe(true);
    const manual: UpdateOperation = {
      ...operation,
      error: {
        code: 'Unsupported',
        message: 'Update by hand',
        detail: {
          reason: 'below_min_updatable_from',
          url: 'https://github.com/dickwu/git-strata/releases/download/v0.3.1/Git-Strata_0.3.1_arm64.dmg',
        },
      },
    };
    expect(retryableDownload(manual)).toBe(false);
    const manualHtml = renderToStaticMarkup(
      <UpdateProgressContent {...progress} state={manual.state} operation={manual} />
    );
    expect(manualHtml).toContain('Update by hand');
    expect(manualHtml).toContain('Download the disk image');
    expect(manualHtml).not.toContain('>Retry<');
  });

  test('an uncertain fence keeps diagnostics without suggesting a disk image reinstall', () => {
    const html = renderToStaticMarkup(
      <UpdateProgressContent {...progress} state={phases.fence_uncertain[0]} />
    );
    expect(html).toContain('launch agent may still be disabled');
    expect(html).toContain('Copy diagnostics');
    expect(html).not.toContain('GitHub releases');
  });

  test('a successful install offers the skill action when its fixed runner is connected', () => {
    const done = phases.done[0];
    const html = renderToStaticMarkup(
      <UpdateProgressContent {...progress} state={done} updateSkill={async () => '<output>'} />
    );
    expect(html).toContain('>Update git-strata skill<');
    expect(html).toContain('npx skills update git-strata -g');
    const fallback = renderToStaticMarkup(<UpdateProgressContent {...progress} state={done} />);
    expect(fallback).toContain('Update the git-strata skill from Terminal');
    expect(fallback).not.toContain('>Update git-strata skill<');
  });
});

describe('update confirmation and settings', () => {
  test('notes are plain text and only the matching HTTPS release link is offered', () => {
    const notes_url = 'https://github.com/dickwu/git-strata/releases/tag/v0.3.1';
    const html = renderToStaticMarkup(
      <ConfirmUpdateContent
        {...confirm}
        liveSchema={8}
        target={{ ...target, notes: '<b>hello</b>\n[bad](javascript:alert(1))', notes_url }}
      />
    );
    expect(html).toContain('&lt;b&gt;hello&lt;/b&gt;\n[bad](javascript:alert(1))');
    expect(html).not.toContain('<b>hello</b>');
    expect(html).not.toContain('href="javascript:');
    expect(html).toContain(`href="${notes_url}"`);
    expect(html).toContain('2 active sessions');
    expect(html).toContain('CLI follows the app');
    expect(html).toContain('>Later<');
    expect(html).toContain('Skip this version');
    expect(
      releaseNotesUrl({ ...target, notes_url: 'https://example.test/releases/tag/v0.3.1' })
    ).toBeUndefined();
  });

  test('a rising or unknown live schema requires acknowledgement', () => {
    expect(schemaWarning(8, 7)).toBe(true);
    expect(schemaWarning(8, undefined)).toBe(true);
    expect(schemaWarning(8, 8)).toBe(false);
    expect(schemaWarning(7, 8)).toBe(false);
    const known = renderToStaticMarkup(
      <ConfirmUpdateContent {...confirm} liveSchema={7} target={target} />
    );
    expect(known).toContain('This update upgrades the store from v7 to v8');
    expect(known).toContain('store/backups/');
    expect(known).toContain('I understand');
    expect(known).toMatch(/<button[^>]+disabled=""[^>]*><span>Update<\/span>/);
    const unknown = renderToStaticMarkup(<ConfirmUpdateContent {...confirm} target={target} />);
    expect(unknown).toContain('The current store schema is unknown');
    expect(unknown).toContain('I understand');
    const unchanged = renderToStaticMarkup(
      <ConfirmUpdateContent {...confirm} liveSchema={8} target={target} />
    );
    expect(unchanged).not.toContain('I understand');
    expect(unchanged).not.toContain('disabled');
  });

  test('a native below-minimum offer shows only a verified disk-image link', () => {
    const url = 'https://github.com/dickwu/git-strata/releases/download/v0.3.1/Git-Strata_0.3.1_arm64.dmg';
    const manual = { ...target, min_updatable_from: '0.3.0', manual_download_url: url };
    const html = renderToStaticMarkup(
      <ConfirmUpdateContent {...confirm} appVersion="0.2.3" target={manual} />
    );
    expect(html).toContain('Update by hand');
    expect(html).toContain(`href="${url}"`);
    expect(html).not.toContain('>Update<');
    expect(html).not.toContain('I understand');

    const untrusted = { ...manual, manual_download_url: 'https://example.test/installer.dmg' };
    const refused = renderToStaticMarkup(<ConfirmUpdateContent {...confirm} target={untrusted} />);
    expect(refused).toContain('Update by hand');
    expect(refused).not.toContain(untrusted.manual_download_url);
    expect(refused).not.toContain('>Update<');
  });

  test('only a verified in-app offer reaches confirmation', () => {
    const render = (eligibility?: InstallEligibility, missingDrain = false) =>
      renderToStaticMarkup(
        <ConfirmUpdateContent
          {...confirm}
          target={target}
          eligibility={eligibility}
          missingDrain={missingDrain}
          onOpenVersions={noop}
        />
      );
    const formula = render({ version: target.version, kind: 'move' });
    expect(formula).toContain('Move the daemon into the app first');
    expect(formula).toContain('Open Daemon panel');
    expect(formula).not.toContain('>Update<');
    const fresh = render({ version: target.version, kind: 'setup' });
    expect(fresh).toContain('Set up the daemon first');
    expect(fresh).not.toContain('>Update<');
    expect(render()).not.toContain('>Update<');
    expect(render({ version: '0.3.2', kind: 'eligible' })).not.toContain('>Update<');
    expect(render(confirm.eligibility, true)).not.toContain('>Update<');
    expect(render(confirm.eligibility)).toContain('>Update<');
  });

  test('a verified schema refusal opens consent copy instead of suggesting a download retry', () => {
    const operation: UpdateOperation = {
      kind: 'install',
      target: { ...target, store_schema: 7 },
      state: { phase: 'verifying_bundle' },
      since: 0,
      running: false,
      error: {
        code: 'NotConfirmed',
        message: 'Confirm the store upgrade',
        detail: { reason: 'schema_acknowledgement_required', from_schema: 7, to_schema: 8 },
      },
    };
    expect(retryableDownload(operation)).toBe(false);
    expect(operationErrorCopy(operation)).toBe('Confirm the store upgrade');
    const html = renderToStaticMarkup(
      <SchemaAcknowledgementContent
        from={7}
        to={8}
        previousVersion="0.3.0"
        action="Update"
        onLater={noop}
        onConfirm={noop}
      />
    );
    expect(html).toContain('This update upgrades the store from v7 to v8');
    expect(html).toContain('I understand');
    expect(html).toContain('>Later<');
    expect(html).toMatch(/<button[^>]+disabled=""[^>]*><span>Update<\/span>/);
  });

  test('the settings control is default-on and the manual action remains available', () => {
    const html = renderToStaticMarkup(<UpdateSettingsContent state={idle} onCheck={noop} />);
    expect(html).toContain('Check for updates automatically');
    expect(html).toContain('checked=""');
    expect(html).toContain('GitHub receives your IP address');
    expect(html).toContain('Check for updates…');
    const controlled = renderToStaticMarkup(
      <UpdateSettingsContent
        state={{ phase: 'check_failed' }}
        automatic={false}
        onAutomaticChange={noop}
        onCheck={noop}
      />
    );
    expect(controlled).not.toContain('disabled=""');
    expect(controlled).not.toContain('checked=""');
    expect(controlled).toContain('Couldn&#x27;t check for updates');
  });

  test('Versions shows the app, daemon, PID and uptime from its real fields', () => {
    const html = renderToStaticMarkup(
      <VersionsBlock
        appVersion="0.3.1"
        now={Date.parse(since) + 120_000}
        status={{
          version: '0.3.0',
          api: 2,
          pid: 357,
          started: since,
          holds: [],
          warnings: [],
          reconcile: { runs: 0, last: null },
          repo_waiting: {},
          verify: null,
        }}
        onRestart={noop}
        onSettings={noop}
        restarting={false}
      />
    );
    for (const text of [
      '0.3.1',
      '0.3.0 (api 2)',
      '357',
      '2.0 min',
      'Daemon program',
      'CLI on PATH',
      'Restart daemon',
    ]) {
      expect(html).toContain(text);
    }
  });

  test('Versions renders native program and CLI origin labels when supplied', () => {
    const program = '/Applications/GIT STRATA.app/Contents/MacOS/git-stratad';
    const html = renderToStaticMarkup(
      <VersionsBlock
        program={program}
        cli={[
          { path: '/opt/homebrew/bin/git-strata', label: 'this app' },
          { path: '/usr/local/bin/git-strata', label: 'Homebrew formula 0.3.0' },
        ]}
        actions={<button>Install CLI on PATH</button>}
        now={0}
        restarting={false}
        onSettings={noop}
      />
    );
    expect(html).toContain(program);
    expect(html).toContain('/opt/homebrew/bin/git-strata</code> · this app');
    expect(html).toContain('/usr/local/bin/git-strata</code> · Homebrew formula 0.3.0');
    expect(html).toContain('Install CLI on PATH');
  });
});
