'use client';

import { useEffect, useState, type ReactNode } from 'react';
import { UnorderedListOutlined } from '@ant-design/icons';
import { Tooltip } from 'antd';
import type { UseQueryResult } from '@tanstack/react-query';
import { copy } from '@/lib/copy';
import { useDu } from '@/lib/queries';
import {
  capChips,
  observeDaemonStart,
  observeScanRun,
  retainsReply,
  statusBarModel,
  type Chip,
  type ChipTarget,
  type DaemonStart,
  type StatusBarInput,
  type StatusBarModel,
  type SpaceLevel,
} from '@/lib/statusBar';
import { asGuiError, reason } from '@/lib/tauri';
import type { DaemonStatus, DepsList, StatusReply, UpdateTarget } from '@/lib/types';
import {
  isActive,
  isExpectedDaemonRestartError,
  restartingDaemon,
  useAppVersion,
  useUpdates,
} from '@/lib/update';
import { DaemonStatusModal, type DaemonModalLanding } from './DaemonStatusModal';
import { ThemeSwitch } from './ThemeSwitch';
import { UpdateControls, type UpdateDialog } from './update/UpdateControls';
import type {
  CancelAfterStopConsent,
  InstallEligibility,
  RecoveryActions,
} from './update/UpdateDialogs';
import { VersionsBlock, type VersionDetails } from './update/VersionsBlock';
import css from './StatusBar.module.css';

/** A section of the daemon status modal that a click can open at. */
export type ModalSection = DaemonModalLanding;

const s = copy.statusBar;

export interface StatusBarHandlers {
  /** Open the daemon status modal, at `section` when one is given. */
  onOpen: (section?: ModalSection) => void;
  /** Show what waits for a person: the Waiting list on the Sessions screen. */
  onWaiting: () => void;
  onSession: (id: string) => void;
}

/** Follow a chip's target (design §3.5). A chip without one opens the modal, which lists every chip. */
export function followTarget(target: ChipTarget | undefined, h: StatusBarHandlers): void {
  if (target?.kind === 'waiting') h.onWaiting();
  else if (target?.kind === 'session') h.onSession(target.session);
  else h.onOpen(target?.section);
}

type DaemonHealth = NonNullable<StatusBarInput['health']> & {
  dataUpdatedAt: number;
  errorUpdatedAt: number;
  acceptErrors?: number;
  lastTimeoutAt?: number;
};

/** Consume each query completion once; joined refusals extend only an observed timeout. */
export function observeDaemonHealth(
  previous: DaemonHealth,
  status: Pick<UseQueryResult<DaemonStatus>, 'data' | 'dataUpdatedAt' | 'error' | 'errorUpdatedAt'>
): DaemonHealth {
  if (
    previous.dataUpdatedAt === status.dataUpdatedAt &&
    previous.errorUpdatedAt === status.errorUpdatedAt
  )
    return previous;
  const next = {
    ...previous,
    dataUpdatedAt: status.dataUpdatedAt,
    errorUpdatedAt: status.errorUpdatedAt,
  };
  if (status.data !== undefined && status.dataUpdatedAt !== previous.dataUpdatedAt) {
    next.timeouts = 0;
    next.lastTimeoutAt = undefined;
    next.lastOkAt = status.dataUpdatedAt;
    next.prevAcceptErrors = previous.acceptErrors;
    const counter = status.data.fds?.accept_errors;
    next.acceptErrors =
      typeof counter === 'number' && Number.isFinite(counter) ? counter : undefined;
  }
  if (status.error != null && status.errorUpdatedAt !== previous.errorUpdatedAt) {
    const error = asGuiError(status.error);
    const why = reason(error);
    if (
      (why === 'daemon_timeout' ||
        (why === 'request_in_flight' && next.lastTimeoutAt !== undefined)) &&
      (next.lastTimeoutAt === undefined || status.errorUpdatedAt - next.lastTimeoutAt >= 4000)
    ) {
      next.timeouts++;
      next.lastTimeoutAt = status.errorUpdatedAt;
    } else if (error.code === 'Busy' && why === 'too_many_open_files') {
      next.timeouts = 0;
      next.lastTimeoutAt = undefined;
    }
  }
  return next;
}

function action(target: ChipTarget | undefined): string {
  if (target?.kind === 'waiting') return s.show.waiting;
  if (target?.kind === 'session') return s.openSession(target.session);
  return s.show[target?.section ?? 'daemon'];
}

/** Visible label plus freshness or error detail, for hover and the accessible name. */
export function triggerCopy(daemon: StatusBarModel['daemon']): string {
  if (!daemon.detail) return daemon.label;
  // Details sometimes already end in a period; the accessible name appends ". Show …".
  return `${daemon.label} · ${daemon.detail.replace(/\.+$/, '')}`;
}

function triggerTip(daemon: StatusBarModel['daemon']) {
  return (
    <div className={css.tip}>
      <div className={css.tipHead}>{daemon.label}</div>
      {daemon.detail && <div>{daemon.detail}</div>}
    </div>
  );
}

/** The full text (the chip may be cut short) and the model's explanation; plain text (G6). */
function chipTip(chip: Chip) {
  return (
    <div className={css.tip}>
      <div className={css.tipHead}>{chip.text}</div>
      {chip.tooltip && <div>{chip.tooltip}</div>}
    </div>
  );
}

function chipButton(chip: Chip, h: StatusBarHandlers) {
  const dialog = chip.target === undefined || chip.target.kind === 'modal';
  const versions = chip.id === 'version';
  return (
    <Tooltip key={chip.id} title={chipTip(chip)} placement="top" mouseEnterDelay={0.35}>
      <button
        type="button"
        className={css.chip}
        data-sev={chip.sev}
        data-chip={chip.id}
        aria-haspopup={dialog ? 'dialog' : undefined}
        aria-label={s.label(chip.text, versions ? 'Show versions' : action(chip.target))}
        onClick={() => (versions ? h.onOpen('versions') : followTarget(chip.target, h))}
      >
        <span className={css.mark} aria-hidden />
        <span className={css.text}>{chip.text}</span>
      </button>
    </Tooltip>
  );
}

interface Versions {
  app?: string;
  daemon?: string;
  restarting: boolean;
  independentError?: boolean;
}

const RESTARTING_LABEL = 'Daemon restarting…';

/** Replace the drift chip with a restart action and refill the visible slots when it is suppressed. */
export function versionedStatusBar(model: StatusBarModel, versions: Versions): StatusBarModel {
  const chips = [...model.chips, ...model.overflow];
  const index = chips.findIndex((chip) => chip.id === 'version');
  const remaining = chips.filter((chip) => chip.id !== 'version');
  if (
    !versions.restarting &&
    versions.app &&
    versions.daemon &&
    versions.app !== versions.daemon &&
    (model.daemon.state === 'up' || index >= 0)
  ) {
    const chip: Chip = {
      id: 'version',
      sev: 'amber',
      rank: 4,
      text: 'Restart daemon',
      tooltip: `App ${versions.app} · daemon ${versions.daemon}`,
      target: { kind: 'modal', section: 'daemon' },
    };
    if (index < 0) remaining.push(chip);
    else remaining.splice(index, 0, chip);
  }
  return {
    ...model,
    ...capChips(remaining),
    daemon:
      versions.restarting && !versions.independentError
        ? { state: 'pending', label: RESTARTING_LABEL, detail: null }
        : model.daemon,
  };
}

/**
 * The bar itself, from a finished model (design §3.5): daemon trigger, at most three chips and
 * "+N", then the right-hand facts and the theme switch. No hooks and no state, so tests render
 * it from hand-built models. The chips area takes the free width, so the right side stays put
 * when chips come and go. A healthy trigger (`state === 'up'`) is only the status dot; its
 * label and freshness sit in the tooltip and accessible name. Other states keep the
 * short label in the bar so they are not missed.
 */
export function StatusBarView({
  model,
  versions,
  offer,
  onUpdate,
  updateDisabled,
  children,
  ...h
}: StatusBarHandlers & {
  model: StatusBarModel;
  versions?: Versions;
  offer?: UpdateTarget;
  onUpdate?: () => void;
  updateDisabled?: boolean;
  /** Rendered last inside the footer (the modal). */
  children?: ReactNode;
}) {
  const { daemon, chips, overflow, right } = model;
  const app = versions?.app ?? '?';
  const daemonVersion = versions?.daemon ?? '?';
  const restarting = versions?.restarting === true;
  const plannedOutage = restarting && daemon.label === RESTARTING_LABEL;
  const drift = Boolean(versions?.app && versions.daemon && versions.app !== versions.daemon);
  const pair = `App ${app} · daemon ${restarting ? 'restarting…' : daemonVersion}`;
  const update = offer && `Update to ${offer.version}`;
  const sessionCount = right.sessions === null ? null : copy.status.sessions(right.sessions);
  return (
    <footer className={`statusbar ${css.bar}`} role="status">
      <Tooltip title={triggerTip(daemon)} placement="top" mouseEnterDelay={0.35}>
        <button
          type="button"
          className={css.trigger}
          onClick={() => h.onOpen()}
          aria-haspopup="dialog"
          aria-label={s.label(triggerCopy(daemon), s.show.daemon)}
          data-testid="daemon-trigger"
          data-restarting={plannedOutage || undefined}
        >
          <span className="dot" data-state={daemon.state} aria-hidden />
          {daemon.state !== 'up' && !plannedOutage && (
            <span className={css.name}>{daemon.label}</span>
          )}
        </button>
      </Tooltip>
      <div className={css.chips} data-testid="status-chips">
        {chips.map((c) => chipButton(c, h))}
        {overflow.length > 0 && (
          <Tooltip
            title={
              <ul className={css.tipList}>
                {overflow.map((c) => (
                  <li key={c.id}>{c.text}</li>
                ))}
              </ul>
            }
            placement="top"
            mouseEnterDelay={0.35}
          >
            <button
              type="button"
              className={css.more}
              aria-haspopup="dialog"
              aria-label={s.moreLabel(overflow.length)}
              onClick={() => h.onOpen()}
              data-testid="status-more"
            >
              {s.more(overflow.length)}
            </button>
          </Tooltip>
        )}
      </div>
      <div className={css.right}>
        {versions && (
          <Tooltip
            title={
              <div className={css.tip}>
                <div className={css.tipHead}>{pair}</div>
                {daemon.state === 'up' && !restarting && !versions.daemon && (
                  <div>This daemon does not report its version</div>
                )}
              </div>
            }
            placement="top"
            mouseEnterDelay={0.35}
            trigger={['hover', 'focus']}
          >
            <button
              type="button"
              className={`${css.trigger} ${css.version}`}
              data-sev={restarting ? 'blue' : drift ? 'amber' : 'muted'}
              data-testid="status-version"
              aria-haspopup="dialog"
              aria-label={s.label(pair, 'Show versions')}
              onClick={() => h.onOpen('versions')}
            >
              <span className={css.versionFull}>{pair}</span>
              {!restarting && (
                <>
                  <span className={css.versionShort}>
                    {versions.app && versions.app === versions.daemon
                      ? app
                      : `${app} · ${daemonVersion}`}
                  </span>
                  <span className={css.versionApp}>
                    {drift && <span className={css.driftDot} aria-hidden="true" />}
                    {app}
                  </span>
                </>
              )}
            </button>
          </Tooltip>
        )}
        {update && (
          <Tooltip
            title={update}
            placement="top"
            mouseEnterDelay={0.35}
            trigger={['hover', 'focus']}
          >
            <button
              type="button"
              className={css.update}
              aria-haspopup="dialog"
              aria-label={update}
              data-testid="status-update"
              disabled={updateDisabled}
              onClick={onUpdate}
            >
              <span className={css.updateText}>{update}</span>
              <span className={css.updateArrow} aria-hidden="true">
                ↑
              </span>
            </button>
          </Tooltip>
        )}
        {right.free && (
          <button
            type="button"
            className={`${css.trigger} ${css.free}`}
            data-sev={right.free.sev}
            aria-haspopup="dialog"
            aria-label={s.label(right.free.text, s.show.storage)}
            onClick={() => h.onOpen('storage')}
            data-testid="status-free"
          >
            <span className={css.freeText}>{right.free.text}</span>
          </button>
        )}
        {right.saved && (
          <Tooltip title={right.saved.tooltip} placement="top" mouseEnterDelay={0.35}>
            <button
              type="button"
              className={`${css.trigger} ${css.saved}`}
              aria-haspopup="dialog"
              aria-label={s.label(right.saved.text, s.show.storage)}
              onClick={() => h.onOpen('storage')}
              data-testid="status-saved"
            >
              {right.saved.text}
            </button>
          </Tooltip>
        )}
        {sessionCount !== null && (
          <>
            <span className={css.sessionsFull}>{sessionCount}</span>
            <span className={css.sessionsShort} title={sessionCount}>
              <UnorderedListOutlined aria-hidden="true" />
              <span aria-hidden="true">{right.sessions}</span>
              <span className="sr-only">{sessionCount}</span>
            </span>
          </>
        )}
        <ThemeSwitch />
      </div>
      {children}
    </footer>
  );
}

const SKIP_KEY = 'git-strata.skip-update-version';

function skippedUpdate(): string | null {
  try {
    return typeof window === 'undefined' ? null : window.localStorage.getItem(SKIP_KEY);
  } catch {
    return null;
  }
}

/** Matches the tray's lifecycle count. A done mark does not close a session. */
export function activeSessionCount({ sessions }: StatusReply): number {
  return sessions.filter(({ status }) => status.state === 'Active' || status.state === 'Creating')
    .length;
}

/**
 * The bottom bar (S1): builds the model from the replies the app already polls, re-reads the
 * clock every 5 s for elapsed times, and owns the daemon status modal.
 */
export function StatusBar({
  status,
  sessions,
  deps,
  onWaiting,
  onSession,
  installEligibility,
  versionDetails,
  recovery,
  updateSkill,
  cancelConsent,
}: {
  status: UseQueryResult<DaemonStatus>;
  sessions: UseQueryResult<StatusReply>;
  deps: DepsList | undefined;
  onWaiting: () => void;
  onSession: (id: string) => void;
  installEligibility?: InstallEligibility;
  versionDetails?: VersionDetails;
  recovery?: RecoveryActions;
  updateSkill?: () => Promise<string>;
  cancelConsent?: CancelAfterStopConsent;
}) {
  const [now, setNow] = useState(() => Date.now());
  const [spaceLevel, setSpaceLevel] = useState<SpaceLevel>('muted');
  const [health, setHealth] = useState<DaemonHealth>({
    timeouts: 0,
    lastOkAt: 0,
    dataUpdatedAt: 0,
    errorUpdatedAt: 0,
  });
  // Adjust only when the query completes, before rendering its model. No extra effect or poll.
  const observedHealth = observeDaemonHealth(health, status);
  if (observedHealth !== health) setHealth(observedHealth);
  const [scanSince, setScanSince] = useState<number | undefined>(undefined);
  const observedScan = observeScanRun(scanSince, status.data, status.dataUpdatedAt);
  if (observedScan !== scanSince) setScanSince(observedScan);
  const [daemonStart, setDaemonStart] = useState<DaemonStart | undefined>(undefined);
  const observedStart = observeDaemonStart(daemonStart, status.data);
  if (observedStart !== daemonStart) setDaemonStart(observedStart);
  // null while closed; otherwise the section the click asked for (none: the top).
  const [modal, setModal] = useState<{ section?: ModalSection } | null>(null);
  const [updateDialog, setUpdateDialog] = useState<UpdateDialog | null>(null);
  const [skipped, setSkipped] = useState(skippedUpdate);
  const appVersion = useAppVersion();
  const snapshot = useUpdates((state) => state.snapshot);
  const checking = useUpdates((state) => state.lastCheck?.phase === 'checking');
  const hasOperation = useUpdates((state) => state.operation !== null);
  const busy = hasOperation || checking || isActive(snapshot);
  const restarting = useUpdates(restartingDaemon);
  const latestOffer = snapshot.phase === 'available' ? snapshot.target : snapshot.latest_offer;
  const offer = latestOffer?.version !== skipped ? latestOffer : undefined;
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(t);
  }, []);
  // Live only while the last daemon_status answered.
  const live = status.isSuccess || (status.data !== undefined && !status.isError);
  // du answers from the daemon's cache (daemon.md §5.12); the same key the daemon screen polls.
  const du = useDu(live ? 60_000 : false);
  // The ticker lags a reply that arrived since its last tick; that reply's time has passed.
  const at = Math.max(now, status.dataUpdatedAt);
  const daemon = {
    pending: status.isPending,
    data: status.data,
    error: status.isError ? status.error : null,
    updatedAt: status.dataUpdatedAt,
  };
  const baseModel = statusBarModel(
    {
      daemon,
      sessions: sessions.data,
      sessionsUpdatedAt: sessions.dataUpdatedAt,
      deps,
      health: observedHealth,
      // A du reply cached from before the daemon went away is not shown (as before).
      du: live ? du.data : undefined,
      scanSince: observedScan,
      daemonRestarted: observedStart?.restarted === true,
    },
    at,
    spaceLevel
  );
  const current = live || retainsReply(daemon, at, observedHealth) ? status.data : undefined;
  const independentError = status.isError && !isExpectedDaemonRestartError(status.error);
  const versions = { app: appVersion, daemon: current?.version, restarting, independentError };
  const model = versionedStatusBar(baseModel, versions);
  if (model.spaceLevel !== undefined && model.spaceLevel !== spaceLevel)
    setSpaceLevel(model.spaceLevel);
  return (
    <>
      <StatusBarView
        model={model}
        versions={versions}
        offer={offer}
        updateDisabled={busy}
        onUpdate={busy ? undefined : () => setUpdateDialog('confirm')}
        onOpen={(section) => setModal({ section })}
        onWaiting={onWaiting}
        onSession={onSession}
      >
        <DaemonStatusModal
          open={modal !== null}
          onClose={() => setModal(null)}
          section={modal?.section}
          chips={[...model.chips, ...model.overflow]}
          daemon={model.daemon}
          status={status}
          du={du}
          sessions={sessions.data}
          now={at}
          restarting={restarting}
          versions={
            <VersionsBlock
              {...versionDetails}
              appVersion={appVersion}
              status={current}
              now={at}
              restarting={restarting}
              onRestart={
                !busy && live && appVersion && current?.version && current.version !== appVersion
                  ? () => {
                      setModal(null);
                      setUpdateDialog('restart');
                    }
                  : undefined
              }
              onSettings={() => {
                setModal(null);
                setUpdateDialog('settings');
              }}
            />
          }
        />
      </StatusBarView>
      <UpdateControls
        dialog={updateDialog}
        onDialog={setUpdateDialog}
        appVersion={appVersion}
        daemon={live ? status.data : undefined}
        activeSessions={sessions.isSuccess ? activeSessionCount(sessions.data) : undefined}
        offer={offer}
        eligibility={installEligibility}
        onOpenVersions={() => {
          setUpdateDialog(null);
          setModal({ section: 'versions' });
        }}
        recovery={recovery}
        updateSkill={updateSkill}
        cancelConsent={cancelConsent}
        onSkip={(version) => {
          setSkipped(version);
          try {
            window.localStorage.setItem(SKIP_KEY, version);
          } catch {
            // The version remains skipped for this app window.
          }
        }}
      />
    </>
  );
}
