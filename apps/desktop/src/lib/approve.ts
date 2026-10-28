/**
 * deps_approve outcome → what the review modal shows (gui-approve design §4.4). Pure, so the
 * headline rules are tested against fixture replies (approve.test.ts):
 *
 * - headline priority Unknown > errors > skipped > moved;
 * - any Unknown (a row, the whole call, a lost reply, Stop waiting) hides every way back to
 *   Approve in this modal: a fresh modal re-reads state first;
 * - a failed move (a skipped row with `code` but no `detail.kind`) shows its code and detail;
 * - Busy lock_wait names the holder.
 */
import { copy } from './copy';
import type { ApproveReply, ApproveRow, GuiError } from './types';

export type Tone = 'success' | 'neutral' | 'warning' | 'error' | 'unknown';

export interface RowView {
  session: string;
  state: 'moved' | 'skipped' | 'failed' | 'unknown';
  /** Glyph plus word, never colour alone. */
  label: string;
  /** Plain text; agent-originated strings are never rendered as markup. */
  detail: string | null;
  conflicts: number;
}

export interface Verdict {
  tone: Tone;
  title: string;
  notes: string[];
  rows: RowView[];
  /** Offer "Review again" (re-read, then Approve can prompt again). False after any Unknown. */
  again: boolean;
  /** A replacement proposal to offer "Review proposal N" for. */
  replacement: number | null;
}

export type Outcome =
  | { kind: 'reply'; reply: ApproveReply }
  | { kind: 'error'; error: GuiError }
  /** The human pressed Stop waiting; the call may still finish. */
  | { kind: 'detached' };

const r = copy.review.result;

const obj = (v: unknown): Record<string, unknown> | null =>
  v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

const reasonOf = (detail: unknown): string | null => str(obj(detail)?.reason);

/** `{repo: "web"}` → "repo web"; anything else as plain JSON. */
export function lockText(lock: unknown): string {
  const o = obj(lock);
  if (o) {
    const parts = Object.entries(o).map(([k, v]) => `${k} ${typeof v === 'string' ? v : JSON.stringify(v)}`);
    if (parts.length > 0) return parts.join(', ');
  }
  return str(lock) ?? 'a lock';
}

/** Busy lock_wait's holder: method and session, either may be null. */
export function holderText(detail: unknown): string {
  const d = obj(detail) ?? {};
  const parts = [str(d.holder), str(d.holder_session)].filter((x): x is string => x !== null);
  return parts.length > 0 ? parts.join(', ') : r.unknownHolder;
}

/** A failed move's or an error's detail as one line of plain text. */
export function detailText(code: string | null, detail: unknown): string | null {
  const d = obj(detail);
  if (!d) return detail == null ? null : String(detail);
  if (str(d.message)) return d.message as string;
  if (d.reason === 'lock_wait') return r.lockWait(holderText(d), lockText(d.lock));
  if (str(d.path)) {
    const holders = Array.isArray(d.holders) ? d.holders.map((h) => (typeof h === 'string' ? h : JSON.stringify(h))) : [];
    return holders.length > 0 ? `${d.path} (${holders.join(', ')})` : (d.path as string);
  }
  const text = JSON.stringify(d);
  return text === '{}' ? code : text;
}

function skipText(detail: Record<string, unknown>): string {
  const kind = String(detail.kind);
  const base = r.skip[kind] ?? kind;
  switch (kind) {
    case 'OwnChange':
      return typeof detail.proposal === 'number' ? `${base} (${r.ownProposal(detail.proposal)})` : base;
    case 'OpenFiles': {
      const files = Array.isArray(detail.files) ? detail.files : [];
      const list = files
        .map(obj)
        .filter((f): f is Record<string, unknown> => f !== null)
        .map((f) => r.openFile(String(f.path ?? '?'), String(f.command ?? '?'), (f.pid as number) ?? '?'));
      return list.length > 0 ? `${base}: ${list.join('; ')}` : base;
    }
    case 'Error': {
      const e = obj(detail.error);
      return e ? `${String(e.code ?? 'Error')}: ${String(e.message ?? '')}`.trim() : base;
    }
    default:
      return base;
  }
}

export function rowView(row: ApproveRow): RowView {
  if (row.kind === 'moved') {
    return {
      session: row.session,
      state: 'moved',
      label: r.rowMoved,
      detail: null,
      conflicts: row.report?.conflicts?.length ?? 0,
    };
  }
  const d = obj(row.detail);
  const code = row.code ?? null;
  if (code === 'Unknown' || reasonOf(row.detail) === 'socket_closed') {
    const more = detailText(code, row.detail);
    return {
      session: row.session,
      state: 'unknown',
      label: r.rowUnknown,
      detail: more ? `${more}. ${r.checkStatus}` : r.checkStatus,
      conflicts: 0,
    };
  }
  if (d && typeof d.kind === 'string') {
    // A SkipReason. `Error` carries a code and is a failure, not a planned skip.
    const failed = d.kind === 'Error';
    return {
      session: row.session,
      state: failed ? 'failed' : 'skipped',
      label: failed ? r.rowFailed : r.rowSkipped,
      detail: skipText(d),
      conflicts: 0,
    };
  }
  // A failed move: `code` plus the error's own detail.
  const more = detailText(code, row.detail);
  return {
    session: row.session,
    state: 'failed',
    label: r.rowFailed,
    detail: code ? (more && more !== code ? `${code}: ${more}` : code) : more,
    conflicts: 0,
  };
}

const ORDER: Record<RowView['state'], number> = { unknown: 0, failed: 1, skipped: 2, moved: 3 };

function fromReply(id: number, reply: ApproveReply, subset: boolean): Verdict {
  const rows = (reply.results ?? []).map(rowView).sort((a, b) => ORDER[a.state] - ORDER[b.state]);
  const n = (s: RowView['state']) => rows.filter((x) => x.state === s).length;
  const state = reply.proposal?.state?.kind ?? 'Unknown';
  const unknown = rows.filter((x) => x.state === 'unknown').map((x) => x.session);
  const base = { rows, replacement: null };
  if (unknown.length > 0) {
    return { ...base, tone: 'unknown', title: r.unknownRows(unknown), notes: [r.unknownNote], again: false };
  }
  const failed = n('failed');
  const skipped = n('skipped');
  if (failed > 0) {
    const notes = skipped > 0 ? [r.skipped(skipped, id)] : [];
    return { ...base, tone: 'error', title: r.failed(failed, id), notes, again: true };
  }
  if (skipped > 0) {
    return { ...base, tone: 'warning', title: r.skipped(skipped, id), notes: [], again: true };
  }
  if (state === 'Applied' || state === 'AwaitingPublish') {
    return { ...base, tone: 'success', title: r.applied(n('moved')), notes: [], again: false };
  }
  if (state === 'Pending' && subset) {
    return { ...base, tone: 'success', title: r.subset(id), notes: [], again: true };
  }
  // Pending with nothing skipped on a full approve, or a state the GUI does not know: say it.
  return {
    ...base,
    tone: 'neutral',
    title: r.applied(n('moved')),
    notes: [r.state(id, state)],
    again: state === 'Pending',
  };
}

function stateName(v: unknown): string {
  const o = obj(v);
  return str(o?.kind) ?? str(v) ?? 'unknown';
}

function fromError(id: number, e: GuiError): Verdict {
  const why = reasonOf(e.detail);
  const d = obj(e.detail) ?? {};
  const base = { rows: [] as RowView[], notes: [] as string[], replacement: null as number | null };
  if (e.code === 'Unknown' || why === 'socket_closed') {
    return { ...base, tone: 'unknown', title: r.unknown, notes: [r.unknownNote], again: false };
  }
  switch (e.code) {
    case 'NotConfirmed':
      if (why === 'prompt_in_progress')
        return { ...base, tone: 'warning', title: r.notApproved, notes: [r.promptOpen], again: true };
      if (why === 'plan_changed')
        return { ...base, tone: 'neutral', title: r.planChanged, again: true };
      return {
        ...base,
        tone: 'neutral',
        title: r.notApproved,
        notes: [r.notConfirmed[why ?? ''] ?? r.other(e.message, why ?? e.code)],
        again: true,
      };
    case 'NoSuchProposal': {
      const st = obj(d.state);
      const kind = stateName(d.state);
      const by =
        (typeof obj(d.replacement)?.id === 'number' ? (obj(d.replacement)!.id as number) : null) ??
        (typeof st?.by === 'number' ? (st.by as number) : null);
      if (kind === 'Superseded' && by !== null)
        return { ...base, tone: 'neutral', title: r.replaced(id, by), again: false, replacement: by };
      if (kind === 'Applied' || kind === 'AwaitingPublish')
        return { ...base, tone: 'neutral', title: r.alreadyApproved, again: false };
      return { ...base, tone: 'neutral', title: r.notPending(id, kind), again: false };
    }
    case 'HeadMoved':
      return { ...base, tone: 'warning', title: r.headMoved, again: false };
    case 'ReadOnly':
      return { ...base, tone: 'error', title: why === 'shutting_down' ? r.shuttingDown : r.other(e.message, why ?? e.code), again: true };
    case 'Busy':
      if (why === 'lock_wait')
        return { ...base, tone: 'warning', title: r.lockWait(holderText(d), lockText(d.lock)), again: true };
      if (why === 'build_lock_busy') return { ...base, tone: 'warning', title: r.buildLock, again: true };
      if (why === 'daemon_down') return { ...base, tone: 'error', title: r.daemonDown, again: true };
      if (why === 'request_in_flight') return { ...base, tone: 'warning', title: r.inFlight, again: false };
      break;
    case 'Unsupported':
      return { ...base, tone: 'error', title: r.unsupported(why ?? e.message), again: false };
  }
  // Anything else: the daemon's own words, code and reason verbatim (gui.md:342, no guessing).
  return { ...base, tone: 'error', title: r.other(e.message, why ? `${e.code}, ${why}` : e.code), again: true };
}

/** `subset`: a session list was sent (S2). Always false in this slice (D2=b). */
export function verdict(id: number, outcome: Outcome, subset = false): Verdict {
  switch (outcome.kind) {
    case 'detached':
      return {
        tone: 'unknown',
        title: r.unknown,
        notes: [r.detachedNote, r.unknownNote],
        rows: [],
        again: false,
        replacement: null,
      };
    case 'error':
      return fromError(id, outcome.error);
    case 'reply':
      return fromReply(id, outcome.reply, subset);
  }
}

/** True when the install inputs differ (deps.md:787); null when the daemon did not send them. */
export function inputsChanged(p: { from_inputs?: string; to_inputs?: string }): boolean | null {
  if (!p.from_inputs || !p.to_inputs) return null;
  return p.from_inputs !== p.to_inputs;
}

/** The query keys refreshed after every outcome, lost replies and Stop waiting included. */
export const REFRESH_AFTER_APPROVE: ReadonlyArray<readonly string[]> = [['deps'], ['sessions'], ['session']];

/** When the modal adds "Still working" and Stop waiting: prompt 60 s + cancel grace 5 s + margin. */
export const STILL_WORKING_MS = 70_000;
