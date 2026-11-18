import { reason } from './tauri';
import type { UpdateState } from './types';
import { SCHEMA_ACKNOWLEDGEMENT_REQUIRED, type UpdateOperation } from './update';

export const DAEMON_LOG = '~/Library/Application Support/git-strata/daemon.err.log';
export const SKILL_COMMAND = 'npx skills update git-strata -g';

export function updateCopy(
  state: UpdateState,
  appVersion?: string,
  targetVersion?: string,
  kind: UpdateOperation['kind'] = 'install'
): string {
  switch (state.phase) {
    case 'idle':
      return 'Check for updates';
    case 'checking':
      return 'Checking for updates…';
    case 'up_to_date':
      return 'GIT STRATA is up to date';
    case 'available':
      return `Update to ${state.target.version}`;
    case 'check_failed':
      return "Couldn't check for updates";
    case 'confirming':
      return 'Checking update requirements…';
    case 'downloading':
      return `Downloading ${targetVersion ?? 'the update'}${state.pct == null ? '…' : ` · ${state.pct}%`}`;
    case 'verifying_bundle':
      return 'Verifying the downloaded app…';
    case 'draining':
      return 'Waiting for the daemon to finish its work…';
    case 'stopping':
      return 'Waiting for the daemon to stop…';
    case 'installing':
      return 'Installing the update…';
    case 'starting_daemon':
      return 'Starting the daemon…';
    case 'relaunching':
      return 'Reopening GIT STRATA…';
    case 'reconciling':
      return 'Finishing the update…';
    case 'waiting_daemon':
      return 'Waiting for the daemon to respond…';
    case 'done':
      return kind === 'restart'
        ? 'Daemon restarted'
        : `Updated to ${targetVersion ?? appVersion ?? 'the new version'}`;
    case 'daemon_failed':
      return kind === 'restart'
        ? "Daemon didn't come back after the restart"
        : "Daemon didn't come back after the update";
    case 'waiting_permission':
      return 'Waiting for macOS permission (Files and Folders)';
    case 'cancelled':
      return kind === 'restart' ? 'Restart cancelled' : 'Update cancelled';
    case 'not_starting':
      return 'Nothing installed; the daemon is not starting';
    case 'install_failed_unchanged':
    case 'bootout_failed':
      return `Update failed; still on ${appVersion ?? 'the previous version'}`;
    case 'restored':
      return `Update failed; restored ${state.from}`;
    case 'restore_failed':
      return 'GIT STRATA could not restore itself';
    case 'cancel_blocked':
      return 'Install a matching or newer GIT STRATA';
    case 'fence_uncertain':
      return 'Could not confirm the daemon stopped';
  }
}

export function operationErrorCopy(operation: UpdateOperation): string | undefined {
  const error = operation.error;
  if (!error) return undefined;
  const why = reason(error);
  if (why === 'below_min_updatable_from') return 'Update by hand';
  if (
    why !== SCHEMA_ACKNOWLEDGEMENT_REQUIRED &&
    (operation.state.phase === 'downloading' || operation.state.phase === 'verifying_bundle')
  ) {
    return `Couldn't download ${operation.target?.version ?? 'the update'} (${error.message})`;
  }
  return (
    error.message ||
    (operation.kind === 'restart' ? "Couldn't restart the daemon" : "Couldn't install the update")
  );
}
