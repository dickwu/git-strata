/**
 * Poll intervals for P1 (no `watch` yet; gui.md §5.3.3 table, fallback rows).
 *
 * Legacy guard: a daemon without the `api` key in `daemon_status` (everything before P2,
 * including the c98794f build that leaked descriptors on 10-27) gets `status{all:true}` far
 * less often — that call runs drift and open-file checks for every session. `false` = off.
 */
export interface PollInput {
  visible: boolean;
  /** Last daemon_status answered. */
  up: boolean;
  /** daemon_status had no `api` key (or none has answered yet). */
  legacy: boolean;
}

export interface PollPlan {
  daemonStatus: number | false;
  sessions: number | false;
  session: number | false;
  deps: number | false;
  du: number | false;
}

const S = 1000;

export function pollPlan({ visible, up, legacy }: PollInput): PollPlan {
  if (!up) {
    // Only look for the daemon coming back.
    return {
      daemonStatus: visible ? 5 * S : 30 * S,
      sessions: false,
      session: false,
      deps: false,
      du: false,
    };
  }
  if (visible) {
    return {
      daemonStatus: legacy ? 5 * S : 2 * S,
      sessions: legacy ? 30 * S : 5 * S,
      session: legacy ? 30 * S : 10 * S,
      deps: 10 * S,
      du: 60 * S,
    };
  }
  return {
    daemonStatus: 10 * S,
    sessions: legacy ? 120 * S : 30 * S,
    session: false,
    deps: 30 * S,
    du: false,
  };
}
