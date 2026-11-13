import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { QueryClient, QueryClientProvider, type UseQueryResult } from '@tanstack/react-query';
import { bytes } from '@/lib/copy';
import { daemonCopy } from '@/lib/daemonCopy';
import { pollPlan } from '@/lib/poll';
import type { Chip, WarningView } from '@/lib/statusBar';
import type { DaemonStatus, DuReply, StatusReply } from '@/lib/types';
import { DaemonScreen } from './DaemonScreen';
import {
  DaemonStatusContent,
  WarningList,
  type DaemonModalSection,
  type DaemonStatusContentProps,
} from './DaemonStatusModal';
import { errorText } from './ErrorNote';

const dir = import.meta.dir;

// A closed antd Modal keeps its content mounted unless destroyOnHidden is set,
// so a Tooltip that was open inside it (e.g. after Esc while hovering) stays on
// screen with nothing under it. Every Modal here must unmount its content.
// Props are prettier-formatted one per line, so the tag ends at a line holding
// only `>` (an arrow's `=>` in a prop never does).
describe('modals', () => {
  const files = readdirSync(dir).filter((f) => f.endsWith('.tsx'));

  test('every <Modal> sets destroyOnHidden', () => {
    const modals = files.flatMap((f) => {
      const src = readFileSync(join(dir, f), 'utf8');
      return [...src.matchAll(/<Modal\b[\s\S]*?\n\s*\/?>\n/g)].map((m) => ({ f, tag: m[0] }));
    });
    expect(modals.map((m) => m.f)).toContain('DaemonStatusModal.tsx');
    for (const m of modals)
      expect({ f: m.f, ok: /\bdestroyOnHidden\b/.test(m.tag) }).toEqual({ f: m.f, ok: true });
  });
});

// The daemon modal and the Daemon screen against the 2026-11-13 09:27 incident (status bar
// design §0): a catch-up move held tool's repo lock (LongHold, no session on 0.2.2), a size
// scan ran with 140 queued, and `total.allocated` summed clones to 67 TB.
const fixture = <T>(name: string): T =>
  JSON.parse(
    readFileSync(join(dir, '../../src-tauri/tests/fixtures/p1', `${name}.json`), 'utf8')
  ) as T;

const T0 = 1_794_511_672; // 09:27:52 America/Winnipeg, in seconds
const since = { secs_since_epoch: T0, nanos_since_epoch: 0 };
const NOW = (T0 + 150) * 1000;
const GiB = 1024 ** 3;
const SECTIONS: DaemonModalSection[] = ['warnings', 'activity', 'storage', 'scan', 'daemon'];

const incidentStatus = (): DaemonStatus => ({
  ...fixture<DaemonStatus>('daemon_status'),
  holds: [{ repo: 'tool', method: 'catch_up', session: null, since }],
  queues: [{ session: 's-eqyyt2k7', holder: 'catch_up', since, queued: ['sync'] }],
  repo_waiting: { tool: 2, web: 0 },
  warnings: [{ LongHold: { repo: 'tool', method: 'catch_up', since } }],
});

const incidentDu = (): DuReply => {
  const du = fixture<DuReply & { store: { trash: Record<string, object> } }>('du');
  const trash = du.store.trash;
  return {
    ...du,
    total: { ...du.total, allocated: 67e12, logical: 66e12, private: 43 * GiB },
    store: {
      ...du.store,
      trash: {
        ...trash,
        private: 33 * GiB,
        entries: 1796,
        eligible: { ...trash.eligible, entries: 1796 },
        young: { ...trash.young, entries: 0 },
      },
    },
    scan: {
      running: true,
      started: '2026-11-13T15:27:00Z',
      done: 412,
      queued: 140,
      full: false,
      failed: [{ unit: 'store/trash/1794511000000-ab', error: 'No such file or directory' }],
      last_full: null,
    },
  } as DuReply;
};

const answered = <T>(data: T) =>
  ({
    data,
    isSuccess: true,
    isError: false,
    isPending: false,
    error: null,
    dataUpdatedAt: NOW,
  }) as unknown as UseQueryResult<T>;

const content = (p: Partial<DaemonStatusContentProps> = {}) =>
  renderToStaticMarkup(
    createElement(DaemonStatusContent, {
      status: answered(incidentStatus()),
      du: answered(incidentDu()),
      sessions: undefined,
      now: NOW,
      socket: '/tmp/daemon.sock',
      ...p,
    })
  );

/** What shows before anything is expanded: the markup without its <details> blocks. */
const shown = (html: string) => html.replace(/<details[\s\S]*?<\/details>/g, '');

/** Text as React writes it into markup. */
const esc = (s: string) =>
  s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#x27;');

/** The chip sections in document order. */
const sectionsIn = (html: string) => [...html.matchAll(/data-section="(\w+)"/g)].map((m) => m[1]);

const TIMEOUT = {
  code: 'Unavailable',
  message: 'timed out',
  detail: { reason: 'daemon_timeout', seconds: 10 },
};

/** The daemon_status query after polls stopped answering: react-query keeps the last reply. */
const notAnswering = (data: DaemonStatus) =>
  ({
    ...answered(data),
    isSuccess: false,
    isError: true,
    error: TIMEOUT,
  }) as unknown as UseQueryResult<DaemonStatus>;

/**
 * Two live sessions, one of them with files that reported no APFS private size (du
 * `exact: false`): their sum and the data root's total are inexact too (du.rs `Agg::merge`).
 */
const inexactDu = (): DuReply => {
  const du = incidentDu();
  const row = (session: string, logical: number, priv: number, exact: boolean) => ({
    ...du.sessions!.list[0],
    session,
    logical,
    allocated: logical,
    private: priv,
    savings: logical - priv,
    exact,
  });
  return {
    ...du,
    total: { ...du.total, exact: false },
    sessions: {
      all: { ...du.sessions!.all, logical: 350 * GiB, private: 150 * GiB, exact: false },
      list: [
        row('s-inexact', 250 * GiB, 130 * GiB, false),
        row('s-exact', 100 * GiB, 20 * GiB, true),
      ],
    },
    savings: {
      live: { ...du.savings!.live, logical: 350 * GiB, private: 150 * GiB, savings: 200 * GiB },
      cumulative: { ...du.savings!.cumulative, sessions: 9, savings: 500 * GiB },
    },
  } as DuReply;
};

describe('daemon modal content', () => {
  test('incident: nothing red; raw JSON and allocated bytes only behind Details', () => {
    const html = content();
    expect(html).not.toContain('data-sev="red"');
    expect(html).toMatch(/data-kind="LongHold"[\s\S]*?<details[\s\S]*?secs_since_epoch/);
    expect(shown(html)).not.toContain('secs_since_epoch');
    expect(shown(html)).not.toContain('{&quot;');
    expect(html).toContain(bytes(67e12));
    expect(shown(html)).not.toContain(bytes(67e12));
    expect(shown(html)).not.toContain(bytes(66e12));
    expect(html).not.toMatch(/on disk/i);
  });

  test('incident: storage is the private "at least"; trash names the human gc command', () => {
    const html = content();
    expect(html).toContain(`git-strata alone holds at least ${bytes(43 * GiB)}`);
    expect(html).toContain('1,796 trash entries older than 1 h can be emptied by');
    expect(html).toContain('<code class="inline-code">git-strata gc</code> (a human command).');
    expect(html).toContain('counted once per file; overcounts clones');
    expect(html).not.toMatch(/\bfrees?\b/i);
  });

  test('incident: the scan in counts; measuring errors as plain history', () => {
    const html = content();
    expect(html).toContain('Measuring storage · 412 done, 140 queued');
    expect(html).toContain('Recent measuring errors');
    expect(html).toContain('store/trash/1794511000000-ab');
  });

  test('incident: activity names the hold as daemon work and the queue by its session', () => {
    const html = content();
    expect(html).toContain('Daemon work: auto-rebase');
    expect(html).toContain('data-background="true"');
    expect(html).toContain('s-eqyyt2k7');
    expect(html).toContain('2.5 min');
    expect(html).toContain('Requests waiting: tool 2');
    expect(html).not.toContain('data-background="false"');
  });

  test('every chip target has its section; Now lists all chips, overflow too, as plain text', () => {
    const chips: Chip[] = [
      {
        id: 'a',
        sev: 'blue',
        rank: 5,
        text: 'Auto-rebase · <b>tool</b>',
        tooltip: 'Moves <i>s</i>',
      },
      { id: 'b', sev: 'muted', rank: 6, text: 'Measuring storage · 412 done, 140 queued' },
      { id: 'c', sev: 'amber', rank: 4, text: 'third' },
      { id: 'd', sev: 'amber', rank: 4, text: 'past the bar' },
    ];
    const html = content({ chips, section: 'scan' });
    for (const s of SECTIONS) expect(html).toContain(`data-section="${s}"`);
    expect(html).toContain('>Now<');
    expect(html).toContain('Auto-rebase · &lt;b&gt;tool&lt;/b&gt;');
    expect(html).toContain('Moves &lt;i&gt;s&lt;/i&gt;');
    expect(html).toContain('past the bar');
    expect(html).not.toContain('<b>');
    expect(content()).not.toContain('>Now<');
  });

  test('quiet daemon: no warnings or activity sections; storage and status stay', () => {
    const html = content({ status: answered(fixture<DaemonStatus>('daemon_status')) });
    expect(html).not.toContain('data-section="warnings"');
    expect(html).not.toContain('data-section="activity"');
    expect(html).toContain('data-section="storage"');
    expect(html).toContain('data-section="daemon"');
  });

  test('an unreachable daemon shows no stale holds or warnings', () => {
    const down = {
      ...answered(incidentStatus()),
      isSuccess: false,
      isError: true,
      error: { code: 'daemon_down', message: 'down', detail: null },
    } as unknown as UseQueryResult<DaemonStatus>;
    const html = content({ status: down });
    expect(html).not.toContain('data-section="warnings"');
    expect(html).not.toContain('data-section="activity"');
    expect(html).not.toContain('git-strata alone holds');
  });

  // Codex impl-r1 B3: with exact false, allocated sizes stood in for private ones, so "held
  // alone (at least)" can overstate and "up to X saved" understate: no bound is shown.
  test('inexact sessions: no "at least" or "up to" figure the sizes cannot support', () => {
    const html = content({ du: answered(inexactDu()) });
    const s = daemonCopy.savings;
    expect(html).toContain(s.liveUnavailable(2));
    expect(html).toContain(s.heldAloneUnavailable(bytes(350 * GiB)));
    expect(html).toMatch(/Saved across 9 sessions recorded since [^<]*: unavailable/);
    expect(html).toContain(esc(s.inexact));
    expect(html).not.toMatch(/held only by the sessions \(at least\)/i);
    expect(html).not.toMatch(/>Up to<|Up to [\d.]+ [KMGT]?B saved/);
    // The live bounds, the inexact session's and the cumulative one appear nowhere.
    for (const n of [150, 200, 130, 120, 500]) expect(html).not.toContain(bytes(n * GiB));
    // Per session: the exact one keeps both bounds; the inexact one says unavailable twice.
    const row = (id: string) =>
      html.match(new RegExp(`<tr[^>]*data-row-key="${id}"[\\s\\S]*?</tr>`))?.[0] ?? '';
    expect(row('s-exact')).toContain(bytes(20 * GiB));
    expect(row('s-exact')).toContain(bytes(80 * GiB));
    expect(row('s-inexact')).toContain(bytes(250 * GiB)); // full size is exact either way
    expect(row('s-inexact').match(/>unavailable</g)).toHaveLength(2);
    // Exact rows first: the inexact one has no saving to rank by.
    expect(html.indexOf('data-row-key="s-exact"')).toBeLessThan(
      html.indexOf('data-row-key="s-inexact"')
    );
    // Storage says the same: the total is unavailable, not a lower bound.
    expect(html).toContain(daemonCopy.storage.headlineUnavailable);
    expect(html).not.toContain('git-strata alone holds at least');
  });

  test('exact sessions keep their bounds; cumulative accuracy remains unknown', () => {
    const html = content();
    expect(html).toMatch(/>Up to</);
    expect(html).not.toMatch(/Up to [\d.]+ [KMGT]?B saved across/);
    expect(html).toMatch(/Saved across 2 sessions recorded since [^<]*: unavailable/);
    expect(html).toContain(esc(daemonCopy.savings.cumulativeAccuracyUnavailable));
    expect(html).toMatch(/held only by the sessions \(at least\)/);
    expect(html).not.toContain(esc(daemonCopy.savings.inexact));
    expect(html).not.toContain('>unavailable<');
  });

  test('historical inexact savings stay hidden when only exact sessions remain', () => {
    const du = inexactDu();
    const remaining = du.sessions!.list[1];
    du.total = {
      ...du.total,
      logical: remaining.logical,
      private: remaining.private,
      exact: true,
    } as DuReply['total'];
    du.sessions = { all: { ...remaining, units: 1 }, list: [remaining] };
    du.savings!.live = {
      ...du.savings!.live,
      logical: remaining.logical,
      private: remaining.private,
      savings: remaining.savings,
    };
    Object.assign(du.savings!.cumulative, {
      removed: 8,
      unsized: 2,
      timed: 2,
      create_ms: 4000,
      timed_logical: 16e9,
    });
    const html = content({ du: answered(du) });
    const s = daemonCopy.savings;
    expect(html).not.toContain(bytes(500 * GiB));
    expect(html).not.toMatch(/Up to [\d.]+ [KMGT]?B saved across/);
    expect(html).toMatch(/Saved across 9 sessions recorded since [^<]*: unavailable/);
    expect(html).toContain(esc(s.cumulativeAccuracyUnavailable));
    expect(html).toContain(s.cumulativeNote(8, 2));
    expect(html).toContain(bytes(80 * GiB)); // Exact live/session upper bound.
    expect(html).toContain(s.heldAlone(bytes(20 * GiB), bytes(100 * GiB)));
    expect(html).toContain(s.measuredValue('2.0 s', 2));
    expect(html).toContain(s.estimatedValue('4.0 s'));
    expect(html).not.toContain(esc(s.inexact));
  });

  test('leftover directories: the count from status{all}, a dash before it answers', () => {
    const cell = (html: string) =>
      html
        .match(/Leftover directories<\/span>[\s\S]*?<td[^>]*>([\s\S]*?)<\/td>/)?.[1]
        .replace(/<[^>]+>/g, '');
    const all = fixture<StatusReply>('status_all');
    const leftovers = [
      { kind: 'RemovedSession', session: 's-old', path: '/x/sessions/s-old' },
      { kind: 'BaseStaging', repo: 'tool', path: '/x/bases/tool/.staging/ab', export_temps: [] },
    ];
    expect(cell(content({ sessions: { ...all, leftovers } }))).toBe('2');
    expect(cell(content({ sessions: all }))).toBe('0');
    expect(cell(content())).toBe('—');
  });
});

// Claude impl-r1 NB10 / Codex NB2: a chip opens the modal at a section (statusBar.ts
// `ChipTarget`). The scroll itself is `sectionLanding` (daemonPanel.test.ts); here, the
// sections it lands on.
describe('daemon modal sections', () => {
  const quiet = () => answered(fixture<DaemonStatus>('daemon_status'));
  const chip = (section: DaemonModalSection, text = 'chip'): Chip => ({
    id: section,
    sev: 'amber',
    rank: 4,
    text,
    target: { kind: 'modal', section },
  });

  test('Now comes first, then every section a chip can target, in order', () => {
    const html = content({ chips: [chip('activity')], section: 'activity' });
    expect(sectionsIn(html)).toEqual(SECTIONS);
    expect(html.indexOf('>Now<')).toBeGreaterThan(-1);
    expect(html.indexOf('>Now<')).toBeLessThan(html.indexOf('data-section='));
  });

  test('warnings[] not empty: the Warnings section is there, whatever the entries are', () => {
    const lists: unknown[][] = [
      ['StorePoisoned'],
      [{ FetchFailed: { repo: 'tool' } }],
      [{ LongHold: { repo: 'tool', method: 'catch_up', since } }],
      [{ LongHold: { repo: 'web', method: 'publish', since } }],
      [{ FutureVariant: {} }],
      ['plain', null],
    ];
    for (const warnings of lists) {
      const status = answered({ ...fixture<DaemonStatus>('daemon_status'), warnings });
      expect(sectionsIn(content({ status, section: 'warnings' }))).toContain('warnings');
    }
    expect(sectionsIn(content({ status: quiet(), section: 'warnings' }))).not.toContain('warnings');
  });

  test('an activity warning alone still gives the activity chip its section', () => {
    // Warnings, holds and queues are sampled at different moments (design §11.3).
    const status = answered({ ...incidentStatus(), holds: [], queues: [], repo_waiting: {} });
    const html = content({ status, section: 'activity' });
    expect(sectionsIn(html)).toContain('activity');
    expect(html).toContain(esc(daemonCopy.activity.background));
    expect(html).not.toContain(daemonCopy.activity.holds);
  });

  test('a chip whose section is not there opens at the top: the Now list, no error', () => {
    const chips = [chip('warnings', 'Only in the Now list'), chip('activity')];
    const html = content({ status: quiet(), chips, section: 'warnings' });
    expect(sectionsIn(html)).toEqual(['storage', 'scan', 'daemon']);
    expect(html).toContain('Only in the Now list');
    expect(html.indexOf('>Now<')).toBeLessThan(html.indexOf('data-section='));
  });
});

describe('warning list', () => {
  const w = (o: Partial<WarningView>): WarningView => ({
    kind: 'X',
    cls: 'notice',
    sev: 'amber',
    text: 'text',
    raw: null,
    ...o,
  });
  const sevs = (items: WarningView[]) =>
    [...renderToStaticMarkup(createElement(WarningList, { items })).matchAll(/data-sev="(\w+)"/g)]
      .map((m) => m[1])
      .filter((_, i) => i % 2 === 0); // the dot and the text carry the same value

  test('red only for the error class; a LongHold never, whatever it is classified as', () => {
    expect(
      sevs([
        w({ kind: 'StorePoisoned', cls: 'error', sev: 'red' }),
        w({ kind: 'LongHold', cls: 'error', sev: 'red' }),
        w({ kind: 'LongHold', cls: 'activity', sev: 'blue' }),
        w({ kind: 'LongHold', cls: 'notice', sev: 'amber' }),
        w({ kind: 'CatalogGrowth', cls: 'notice', sev: 'muted' }),
      ])
    ).toEqual(['red', 'amber', 'blue', 'amber', 'muted']);
  });

  test('agent-written text stays plain text', () => {
    const html = renderToStaticMarkup(
      createElement(WarningList, { items: [w({ text: '<img src=x>', raw: { a: '<b>' } })] })
    );
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img src=x&gt;');
  });

  test('the module CSS paints var(--danger) only for the red severity', () => {
    const src = readFileSync(join(dir, 'DaemonStatusModal.module.css'), 'utf8').replace(
      /\/\*[\s\S]*?\*\//g,
      ''
    );
    const danger = [...src.matchAll(/([^{}]+)\{([^}]*)\}/g)]
      .filter((m) => m[2].includes('var(--danger)'))
      .map((m) => m[1].trim());
    expect(danger.length).toBeGreaterThan(0);
    for (const sel of danger)
      for (const part of sel.split(',')) expect(part.trim()).toMatch(/\[data-sev=['"]red['"]\]$/);
  });
});

describe('daemon screen', () => {
  const screen = (status: UseQueryResult<DaemonStatus>) => {
    const client = new QueryClient();
    client.setQueryData(['du'], incidentDu());
    client.setQueryData(['socket'], '/tmp/daemon.sock');
    return renderToStaticMarkup(
      createElement(
        QueryClientProvider,
        { client },
        createElement(DaemonScreen, {
          status,
          plan: { ...pollPlan({ visible: true, up: true, legacy: true }), du: false },
          onBack: () => {},
        })
      )
    );
  };

  test('incident: storage as the private bound, the LongHold as one plain line', () => {
    const html = screen(answered(incidentStatus()));
    expect(html).toContain(`git-strata alone holds at least ${bytes(43 * GiB)}`);
    expect(html).not.toContain(bytes(67e12));
    expect(html).not.toMatch(/on disk/i);
    expect(html).toContain('data-kind="LongHold"');
    expect(html).not.toContain('data-sev="red"');
    expect(shown(html)).not.toContain('secs_since_epoch');
  });

  // Claude impl-r1 NB7: classified from the cached reply, the hold would keep aging and turn
  // amber at 30 min while the daemon is not answering (the modal already guarded this).
  test('daemon not answering: nothing classified or aged from the cached reply', () => {
    const html = screen(notAnswering(incidentStatus()));
    expect(html).toContain(esc(errorText(TIMEOUT).title));
    expect(html).not.toContain('data-kind=');
    expect(html).not.toContain('data-sev=');
    expect(html).not.toContain('git-strata alone holds');
    expect(html).not.toContain('<span class="mono">tool</span>'); // repo_waiting {tool: 2}
    // The same reply, answered: warnings, waiting and storage are back.
    const live = screen(answered(incidentStatus()));
    expect(live).toContain('data-kind="LongHold"');
    expect(live).toContain('<span class="mono">tool</span>');
  });
});
