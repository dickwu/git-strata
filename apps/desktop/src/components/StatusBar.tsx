'use client';

import { useEffect, useState, type ReactNode } from 'react';
import { Tooltip } from 'antd';
import type { UseQueryResult } from '@tanstack/react-query';
import { copy } from '@/lib/copy';
import { useDu } from '@/lib/queries';
import {
  statusBarModel,
  type Chip,
  type ChipTarget,
  type StatusBarInput,
  type StatusBarModel,
} from '@/lib/statusBar';
import { asGuiError, reason } from '@/lib/tauri';
import type { DaemonStatus, DepsList, StatusReply } from '@/lib/types';
import { DaemonStatusModal } from './DaemonStatusModal';
import { ThemeSwitch } from './ThemeSwitch';
import css from './StatusBar.module.css';

/** A section of the daemon status modal that a click can open at. */
export type ModalSection = Extract<ChipTarget, { kind: 'modal' }>['section'];

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

/** Consume each existing query completion once; clock ticks and ordinary refusals add no evidence. */
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
    if (
      reason(error) === 'daemon_timeout' &&
      (next.lastTimeoutAt === undefined || status.errorUpdatedAt - next.lastTimeoutAt >= 4000)
    ) {
      next.timeouts++;
      next.lastTimeoutAt = status.errorUpdatedAt;
    } else if (error.code === 'Busy' && reason(error) === 'too_many_open_files') {
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
  return (
    <Tooltip key={chip.id} title={chipTip(chip)} placement="top" mouseEnterDelay={0.35}>
      <button
        type="button"
        className={css.chip}
        data-sev={chip.sev}
        data-chip={chip.id}
        aria-haspopup={dialog ? 'dialog' : undefined}
        aria-label={s.label(chip.text, action(chip.target))}
        onClick={() => followTarget(chip.target, h)}
      >
        <span className={css.mark} aria-hidden />
        <span className={css.text}>{chip.text}</span>
      </button>
    </Tooltip>
  );
}

/**
 * The bar itself, from a finished model (design §3.5): daemon trigger, at most three chips and
 * "+N", then the right-hand facts and the theme switch. No hooks and no state, so tests render
 * it from hand-built models. The chips area takes the free width, so the right side stays put
 * when chips come and go.
 */
export function StatusBarView({
  model,
  children,
  ...h
}: StatusBarHandlers & {
  model: StatusBarModel;
  /** Rendered last inside the footer (the modal). */
  children?: ReactNode;
}) {
  const { daemon, chips, overflow, right } = model;
  return (
    <footer className="statusbar" role="status">
      <button
        type="button"
        className={css.trigger}
        onClick={() => h.onOpen()}
        aria-haspopup="dialog"
        aria-label={s.label(daemon.label, s.show.daemon)}
        data-testid="daemon-trigger"
      >
        <span className="dot" data-state={daemon.state} />
        <span className={css.name}>{daemon.label}</span>
        {daemon.detail && (
          <span className={css.detail} title={daemon.detail}>
            {daemon.detail}
          </span>
        )}
      </button>
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
            {right.free.text}
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
        {right.sessions !== null && <span>{copy.status.sessions(right.sessions)}</span>}
        <ThemeSwitch />
      </div>
      {children}
    </footer>
  );
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
}: {
  status: UseQueryResult<DaemonStatus>;
  sessions: UseQueryResult<StatusReply>;
  deps: DepsList | undefined;
  onWaiting: () => void;
  onSession: (id: string) => void;
}) {
  const [now, setNow] = useState(() => Date.now());
  const [health, setHealth] = useState<DaemonHealth>({
    timeouts: 0,
    lastOkAt: 0,
    dataUpdatedAt: 0,
    errorUpdatedAt: 0,
  });
  // Adjust only when the query completes, before rendering its model. No extra effect or poll.
  const observedHealth = observeDaemonHealth(health, status);
  if (observedHealth !== health) setHealth(observedHealth);
  // null while closed; otherwise the section the click asked for (none: the top).
  const [modal, setModal] = useState<{ section?: ModalSection } | null>(null);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(t);
  }, []);
  // Live only while the last daemon_status answered.
  const live = status.isSuccess || (status.data !== undefined && !status.isError);
  // du answers from the daemon's cache (daemon.md §5.12); the same key the daemon screen polls.
  const du = useDu(live ? 60_000 : false);
  const model = statusBarModel(
    {
      daemon: {
        pending: status.isPending,
        data: status.data,
        error: status.isError ? status.error : null,
        updatedAt: status.dataUpdatedAt,
      },
      sessions: sessions.data,
      sessionsUpdatedAt: sessions.dataUpdatedAt,
      deps,
      health: observedHealth,
      // A du reply cached from before the daemon went away is not shown (as before).
      du: live ? du.data : undefined,
    },
    now
  );
  return (
    <StatusBarView
      model={model}
      onOpen={(section) => setModal({ section })}
      onWaiting={onWaiting}
      onSession={onSession}
    >
      <DaemonStatusModal
        open={modal !== null}
        onClose={() => setModal(null)}
        section={modal?.section}
        chips={[...model.chips, ...model.overflow]}
        status={status}
        du={du}
        sessions={sessions.data}
        now={now}
      />
    </StatusBarView>
  );
}
