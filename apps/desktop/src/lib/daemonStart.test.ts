import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { copy } from './copy';
import { downVariant, phaseText, startErrorText } from './daemonStart';
import type { LegacyAgent } from './types';

const err = (detail: Record<string, unknown>, message = 'm') => ({ code: 'Busy', message, detail });
const agent = (state: LegacyAgent['state']): LegacyAgent => ({
  state,
  label: 'com.gwd.git-stratad',
  plist: '/Users/x/Library/LaunchAgents/com.gwd.git-stratad.plist',
  command: 'git-strata daemon start',
});

describe('Start daemon copy', () => {
  test('the button and its toast share a name', () => {
    expect(copy.start.button).toBe('Start daemon');
    expect(copy.start.done).toBe('Daemon started');
  });
  test('the terminal command stays as a fallback; the old "comes later" hint is gone', () => {
    expect(copy.start.fallback).toContain('`git-strata daemon start`');
    expect('downHint' in copy.daemon).toBe(false);
  });
  test('sentence case, no arrows, no all-caps labels', () => {
    const all = JSON.stringify(copy.start);
    expect(all).not.toContain('→');
    for (const w of all.match(/\b[A-Z]{4,}\b/g) ?? []) expect(['README']).toContain(w);
  });
});

describe('downVariant', () => {
  test('no plist is setup-needed; loaded or installed offers Start', () => {
    expect(downVariant(agent('not_installed'))).toBe('setup');
    expect(downVariant(agent('installed'))).toBe('start');
    expect(downVariant(agent('loaded'))).toBe('start');
    expect(downVariant(undefined)).toBe('start');
  });
});

describe('phaseText', () => {
  test('each progress phase has its line; TCC names the prompt', () => {
    expect(phaseText({ phase: 'launching', action: 'kickstart' })).toBe(copy.start.phase.kickstart);
    expect(phaseText({ phase: 'launching', action: 'bootstrap' })).toBe(copy.start.phase.bootstrap);
    expect(phaseText({ phase: 'waiting' })).toBe(copy.start.phase.waiting);
    expect(phaseText({ phase: 'waiting_store', tcc: false })).toBe(copy.start.phase.store);
    expect(phaseText({ phase: 'waiting_store', tcc: true })).toContain('removable-volume prompt');
    expect(phaseText({ phase: 'recovering' })).toBe(copy.start.phase.recovering);
    expect(phaseText({ phase: 'ready' })).toBe('Daemon started');
  });
});

describe('startErrorText', () => {
  test('plist_missing becomes the setup variant, with the path', () => {
    const t = startErrorText(err({ reason: 'plist_missing', plist: '/p/x.plist' }));
    expect(t.setup).toBe('/p/x.plist');
    expect(t.title).toContain('`/p/x.plist`');
    expect(t.hint).toContain('Install and daemon');
  });
  test('launchctl_failed carries its stderr and exit code', () => {
    const t = startErrorText(
      err({
        reason: 'launchctl_failed',
        step: 'bootstrap',
        status: 5,
        stderr: 'Bootstrap failed: 5',
      })
    );
    expect(t.title).toBe('launchctl bootstrap failed with exit code 5.');
    expect(t.output).toBe('Bootstrap failed: 5');
    expect(t.hint).toBeTruthy();
  });
  test('a launchctl that did not finish says so', () => {
    const t = startErrorText(err({ reason: 'launchctl_failed', step: 'kickstart', status: null }));
    expect(t.title).toBe('launchctl kickstart did not finish.');
    expect(t.output).toBeUndefined();
  });
  test('tcc_pending tells you to approve the removable-volume prompt', () => {
    const t = startErrorText(err({ reason: 'tcc_pending', seconds: 30, store_error: 'EPERM' }));
    expect(t.title).toContain('removable volume');
    expect(t.hint).toContain('Approve the removable-volume prompt');
    expect(t.output).toBe('EPERM');
  });
  test('store_unavailable and start_timeout say what happened and what to do', () => {
    const s = startErrorText(err({ reason: 'store_unavailable', store_error: 'ENOENT' }));
    expect(s.title).toBe(copy.start.errors.store);
    expect(s.output).toBe('ENOENT');
    const t = startErrorText(err({ reason: 'start_timeout', seconds: 30 }));
    expect(t.title).toContain('30 seconds');
    expect(t.hint).toContain('daemon.err.log');
  });
  test('in-flight, wrong window and unknown errors', () => {
    expect(startErrorText(err({ reason: 'start_in_flight' })).title).toBe(
      copy.start.errors.inFlight
    );
    expect(startErrorText(err({ reason: 'not_main_window' })).title).toBe(
      copy.start.errors.notMain
    );
    expect(startErrorText(err({ reason: 'other' }, 'boom')).title).toBe('boom');
    expect(startErrorText('weird').title).toBe('weird');
  });
});

describe('back-end contract', () => {
  // The reasons and phases above are the ones src-tauri/src/service/mod.rs emits.
  const rs = readFileSync(join(import.meta.dir, '../../src-tauri/src/service/mod.rs'), 'utf8');
  test('every start reason is emitted by the back end', () => {
    for (const r of [
      'plist_missing',
      'launchctl_failed',
      'tcc_pending',
      'store_unavailable',
      'start_timeout',
      'start_in_flight',
      'not_main_window',
    ])
      expect(rs).toContain(`"reason": "${r}"`);
  });
  test('the command names match the registered ones', () => {
    const lib = readFileSync(join(import.meta.dir, '../../src-tauri/src/lib.rs'), 'utf8');
    expect(lib).toContain('commands::service::legacy_agent');
    expect(lib).toContain('commands::service::daemon_start');
  });
});
