import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { isValidElement, type ReactElement, type ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { contrast, mix } from '../lib/contrast';
import { statusBarModel, type Chip, type StatusBarModel } from '../lib/statusBar';
import type { DaemonStatus, DuReply } from '../lib/types';
import { TOKENS, type ThemeName } from '../lib/tokens';
import {
  StatusBarView,
  followTarget,
  observeDaemonHealth,
  type ModalSection,
  type StatusBarHandlers,
} from './StatusBar';
import { showWaiting } from './Shell';
import css from './StatusBar.module.css';

type Call = ['open', ModalSection | undefined] | ['waiting'] | ['session', string];

function recorder() {
  const calls: Call[] = [];
  const h: StatusBarHandlers = {
    onOpen: (section) => calls.push(['open', section]),
    onWaiting: () => calls.push(['waiting']),
    onSession: (id) => calls.push(['session', id]),
  };
  return { calls, h };
}

type El = ReactElement<Record<string, unknown> & { children?: ReactNode; title?: ReactNode }>;

/** Every element of a tree as written (components are not rendered), depth first. */
function elements(node: ReactNode, out: El[] = []): El[] {
  if (Array.isArray(node)) for (const n of node) elements(n, out);
  else if (isValidElement(node)) {
    const el = node as El;
    out.push(el);
    elements(el.props.children, out);
  }
  return out;
}

const buttons = (tree: ReactNode) => elements(tree).filter((e) => e.type === 'button');

/** The tooltip wrapped around the chip with this id. */
function tipOf(tree: ReactNode, id: string): ReactNode {
  const child = (e: El) => e.props.children;
  const tip = elements(tree).find((e) => {
    const c = child(e);
    return isValidElement(c) && (c as El).props['data-chip'] === id;
  });
  return tip?.props.title;
}

const running: StatusBarModel['daemon'] = {
  state: 'up',
  label: 'Daemon running',
  detail: 'Updated just now',
};
const right: StatusBarModel['right'] = {
  free: null,
  saved: { text: 'up to 1.2 TB saved', tooltip: 'At most this much: shared blocks count once.' },
  sessions: 7,
};
const shown: Chip[] = [
  {
    id: 'attention',
    sev: 'red',
    rank: 1,
    text: 'Daemon needs attention: 2',
    target: { kind: 'modal', section: 'warnings' },
  },
  { id: 'waiting', sev: 'amber', rank: 2, text: '2 waiting for you', target: { kind: 'waiting' } },
  {
    id: 'conflicts',
    sev: 'purple',
    rank: 3,
    text: '1 repo needs an agent',
    target: { kind: 'waiting' },
  },
];
const hidden: Chip[] = [
  {
    id: 'warning:LongHold:staff-api',
    sev: 'amber',
    rank: 4,
    text: 'staff-api locked by publish for 2 min',
    target: { kind: 'modal', section: 'warnings' },
  },
  {
    id: 'activity:catch_up:tool',
    sev: 'blue',
    rank: 5,
    text: 'Auto-rebase work · tool · s-eqyyt2k7 · 2 min · 2 waiting',
    target: { kind: 'modal', section: 'activity' },
  },
];
const crowded: StatusBarModel = { daemon: running, chips: shown, overflow: hidden, right };
const calm: StatusBarModel = { daemon: running, chips: [], overflow: [], right };

const render = (model: StatusBarModel) =>
  renderToStaticMarkup(<StatusBarView model={model} {...recorder().h} />);
const all = (html: string, attr: string) =>
  [...html.matchAll(new RegExp(`${attr}="([^"]*)"`, 'g'))].map((m) => m[1]);

describe('StatusBarView', () => {
  test('inexact savings from the model are absent in the rendered bar', () => {
    const du = JSON.parse(
      readFileSync(join(import.meta.dir, '../lib/fixtures/statusBar/incident-0927.json'), 'utf8')
    ).du as DuReply;
    du.savings = {
      live: { logical: 4096, private: 1024, savings: 3072, complete: true, as_of: null },
      cumulative: {
        since: null,
        sessions: 0,
        sized: 0,
        unsized: 0,
        removed: 0,
        logical: 0,
        private: 0,
        savings: 0,
      },
    };
    Object.assign(du.total, { exact: false });
    const model = statusBarModel(
      {
        daemon: {
          pending: false,
          data: {
            holds: [],
            warnings: [],
            reconcile: { runs: 0, last: null },
            repo_waiting: {},
            verify: null,
          },
          error: null,
          updatedAt: 1000,
        },
        sessions: undefined,
        deps: undefined,
        du,
      },
      1000
    );
    const html = render(model);
    expect(html).not.toContain('status-saved');
    expect(html).not.toContain('up to');
  });
  test('three chips in model order, then +N for the rest', () => {
    const html = render(crowded);
    expect(all(html, 'data-chip')).toEqual(['attention', 'waiting', 'conflicts']);
    expect(all(html, 'data-sev')).toEqual(['red', 'amber', 'purple']);
    expect(html).toContain('>+2</button>');
    // The accessible name starts with the visible "+2" (label in name).
    expect(html).toContain('aria-label="+2 more. Show all in daemon status"');
    // Chips past the third are only in the "+2" tooltip and the modal, never drawn in the bar.
    expect(html).not.toContain('locked by publish');
    expect(html).not.toContain('Auto-rebase work');
  });

  test('no chips: no +N, and the right side renders exactly as with chips', () => {
    const quiet = render(calm);
    const busy = render(crowded);
    expect(quiet).toContain('data-testid="status-chips"');
    expect(quiet).not.toContain('data-chip=');
    expect(quiet).not.toContain('status-more');
    const tail = (html: string) => html.slice(html.indexOf(css.right));
    expect(css.right).toBeTruthy();
    expect(tail(quiet)).toBe(tail(busy));
  });

  test('layout: the right group never shrinks; the trigger detail gives way before chips', () => {
    const src = readFileSync(join(import.meta.dir, 'StatusBar.module.css'), 'utf8');
    const rule = (sel: string) => {
      const i = src.indexOf(`\n${sel} {`);
      expect(i, sel).toBeGreaterThanOrEqual(0);
      return src.slice(i, src.indexOf('}', i));
    };
    expect(rule('.right')).toContain('flex: none;');
    expect(rule('.chips')).toContain('flex: 1 1 auto;');
    expect(rule('.chips')).toContain('overflow: hidden;');
    expect(rule('.trigger')).toContain('flex: 0 1000 auto;');
    // The detail's floor is one letter, so the trigger never shrinks past its dot and label.
    expect(rule('.trigger > .detail')).toContain('overflow-wrap: anywhere;');
    expect(rule('.trigger > .detail')).toContain('-webkit-line-clamp: 1;');
    // Chip words are cut with an ellipsis; the tooltip carries the whole text.
    expect(rule('.text')).toContain('text-overflow: ellipsis;');
  });

  test('chip text, aria-label and tooltip stay plain text (G6)', () => {
    const chip: Chip = {
      id: 'activity:catch_up:<b>tool</b>',
      sev: 'blue',
      rank: 5,
      text: 'Auto-rebase work · <b>tool</b> · 2 min',
      tooltip: '<img src=x onerror="alert(1)"> moving',
      target: { kind: 'modal', section: 'activity' },
    };
    const model: StatusBarModel = { ...calm, chips: [chip] };
    const html = render(model);
    expect(html).toContain('>Auto-rebase work · &lt;b&gt;tool&lt;/b&gt; · 2 min</span>');
    expect(html).toContain(
      'aria-label="Auto-rebase work · &lt;b&gt;tool&lt;/b&gt; · 2 min. Show activity in daemon status"'
    );
    expect(html).not.toContain('<b>');
    const tip = renderToStaticMarkup(
      <>{tipOf(StatusBarView({ model, ...recorder().h }), chip.id)}</>
    );
    expect(tip).toContain('Auto-rebase work · &lt;b&gt;tool&lt;/b&gt; · 2 min');
    expect(tip).toContain('&lt;img src=x onerror=&quot;alert(1)&quot;&gt; moving');
    expect(tip).not.toContain('<img');
    const src = readFileSync(join(import.meta.dir, 'StatusBar.tsx'), 'utf8');
    expect(src).not.toContain('dangerouslySetInnerHTML');
  });

  test('right side on a 0.2.2 daemon: no free space; saved, sessions and theme in order', () => {
    const html = render(crowded);
    expect(html).not.toContain('status-free');
    expect(html).toContain('>up to 1.2 TB saved</button>');
    expect(html).toContain('aria-label="up to 1.2 TB saved. Show storage in daemon status"');
    const order = ['status-saved', '>7 sessions<', 'data-testid="theme-switch"'].map((k) =>
      html.indexOf(k)
    );
    expect(order.every((i) => i > 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });

  test('right side: free space first when reported, coloured by severity; null facts hidden', () => {
    const html = render({
      ...calm,
      right: { free: { text: '48 GB free', sev: 'amber' }, saved: null, sessions: null },
    });
    expect(html).toMatch(/data-sev="amber"[^>]*>48 GB free<\/button>/);
    expect(html).not.toContain('status-saved');
    expect(html).not.toContain('session');
    expect(html).toContain('data-testid="theme-switch"');
    expect(render({ ...calm, right: { ...right, sessions: 1 } })).toContain('>1 session<');
  });

  test('daemon trigger: dot state, label and detail from the model', () => {
    const html = render({
      ...calm,
      daemon: { state: 'down', label: 'Daemon unreachable', detail: null },
    });
    expect(html).toContain('class="dot" data-state="down"');
    expect(html).toContain('aria-label="Daemon unreachable. Show daemon status"');
    expect(html).not.toContain(css.detail);
    const up = render(calm);
    expect(up).toContain('data-state="up"');
    expect(up).toContain('title="Updated just now">Updated just now</span>');
  });

  test('clicks: chips follow their target; +N and the trigger open the modal; storage facts open storage', () => {
    const { calls, h } = recorder();
    const model: StatusBarModel = {
      daemon: running,
      chips: [
        {
          id: 'activity',
          sev: 'blue',
          rank: 5,
          text: 'Auto-rebase work · tool · 2 min',
          target: { kind: 'modal', section: 'activity' },
        },
        {
          id: 'waiting',
          sev: 'amber',
          rank: 2,
          text: '2 waiting for you',
          target: { kind: 'waiting' },
        },
        {
          id: 'stuck',
          sev: 'amber',
          rank: 4,
          text: 'Auto-rebase stuck: tool · s-abc (open files)',
          target: { kind: 'session', session: 's-abc' },
        },
      ],
      overflow: [{ id: 'scan', sev: 'muted', rank: 6, text: 'Measuring storage · 412 done' }],
      right: { ...right, free: { text: '1.2 TB free', sev: 'muted' } },
    };
    const tree = StatusBarView({ model, ...h });
    const press = (pick: (b: El) => boolean) => {
      const b = buttons(tree).find(pick);
      expect(b).toBeDefined();
      (b!.props.onClick as () => void)();
    };
    for (const id of ['activity', 'waiting', 'stuck']) press((b) => b.props['data-chip'] === id);
    for (const id of ['status-more', 'daemon-trigger', 'status-free', 'status-saved'])
      press((b) => b.props['data-testid'] === id);
    expect(calls).toEqual([
      ['open', 'activity'],
      ['waiting'],
      ['session', 's-abc'],
      ['open', undefined],
      ['open', undefined],
      ['open', 'storage'],
      ['open', 'storage'],
    ]);
    // Session and waiting targets are not dialogs; modal ones say so.
    const pop = (id: string) =>
      buttons(tree).find((b) => b.props['data-chip'] === id)!.props['aria-haspopup'];
    expect([pop('activity'), pop('waiting'), pop('stuck')]).toEqual([
      'dialog',
      undefined,
      undefined,
    ]);
  });

  test('followTarget: a chip without a target opens the modal at the top', () => {
    const { calls, h } = recorder();
    followTarget(undefined, h);
    followTarget({ kind: 'modal', section: 'scan' }, h);
    expect(calls).toEqual([
      ['open', undefined],
      ['open', 'scan'],
    ]);
  });
});

describe('daemonStatus query observations', () => {
  const data = (accept_errors: number): DaemonStatus => ({
    holds: [],
    warnings: [],
    reconcile: { runs: 0, last: null },
    repo_waiting: {},
    verify: null,
    fds: { open: 10, limit: 100, accept_errors },
  });
  const initial = () => ({ timeouts: 0, lastOkAt: 0, dataUpdatedAt: 0, errorUpdatedAt: 0 });
  const error = (why: string) =>
    Object.assign(new Error('probe failed'), { code: 'Busy', detail: { reason: why } });

  test('counts query timeouts once, coalesces within 4 s, and clears on a successful completion', () => {
    const query = {
      data: data(15),
      dataUpdatedAt: 1000,
      errorUpdatedAt: 0,
      error: null as Error | null,
    };
    let health = observeDaemonHealth(initial(), query);
    const model = (now: number) =>
      statusBarModel(
        {
          daemon: {
            pending: false,
            data: query.data,
            updatedAt: query.dataUpdatedAt,
            error: query.error,
          },
          health,
          sessions: undefined,
          deps: undefined,
          du: undefined,
        },
        now
      );
    expect(health).toMatchObject({ timeouts: 0, lastOkAt: 1000, prevAcceptErrors: undefined });
    query.error = error('daemon_timeout');
    query.errorUpdatedAt = 9000;
    health = observeDaemonHealth(health, query);
    expect(health.timeouts).toBe(1);
    expect(observeDaemonHealth(health, query)).toBe(health);
    expect(model(30_000).daemon.state).toBe('pending');
    query.errorUpdatedAt = 10_000;
    health = observeDaemonHealth(health, query);
    expect(health.timeouts).toBe(1);
    query.errorUpdatedAt = 17_000;
    health = observeDaemonHealth(health, query);
    expect(health.timeouts).toBe(2);
    expect(model(20_999).daemon.state).toBe('pending');
    expect(model(21_000).daemon.label).toBe("Daemon isn't responding");
    query.dataUpdatedAt = 22_000;
    query.error = null;
    health = observeDaemonHealth(health, query);
    expect(health).toMatchObject({ timeouts: 0, lastOkAt: 22_000 });
    expect(model(22_000).daemon.state).toBe('up');
    query.error = error('daemon_timeout');
    query.errorUpdatedAt = 50_000;
    health = observeDaemonHealth(health, query);
    expect(health.timeouts).toBe(1);
    expect(model(50_000).daemon.state).toBe('pending');
  });

  test.each(['request_in_flight', 'not_tauri', 'shed', 'too_many_open_files'])(
    '%s never counts as a health timeout',
    (why) => {
      const query = {
        data: data(15),
        dataUpdatedAt: 1000,
        errorUpdatedAt: 5000,
        error: error(why),
      };
      expect(observeDaemonHealth(initial(), query).timeouts).toBe(0);
    }
  );

  test('accept_errors compares successful replies across errors and counter resets', () => {
    const query = {
      data: data(15),
      dataUpdatedAt: 1000,
      errorUpdatedAt: 0,
      error: null as Error | null,
    };
    let health = observeDaemonHealth(initial(), query);
    const chips = () => {
      const m = statusBarModel(
        {
          daemon: {
            pending: false,
            data: query.data,
            updatedAt: query.dataUpdatedAt,
            error: query.error,
          },
          health,
          sessions: undefined,
          deps: undefined,
          du: undefined,
        },
        Math.max(query.dataUpdatedAt, query.errorUpdatedAt)
      );
      return [...m.chips, ...m.overflow];
    };
    expect(chips()).toEqual([]);
    query.error = error('request_in_flight');
    query.errorUpdatedAt = 5000;
    health = observeDaemonHealth(health, query);
    expect(health.prevAcceptErrors).toBeUndefined();
    query.data = data(16);
    query.dataUpdatedAt = 10_000;
    query.error = null;
    health = observeDaemonHealth(health, query);
    expect(health.prevAcceptErrors).toBe(15);
    expect(chips().map((c) => c.id)).toEqual(['fds']);
    expect(observeDaemonHealth(health, query)).toBe(health);
    query.error = error('request_in_flight');
    query.errorUpdatedAt = 12_000;
    health = observeDaemonHealth(health, query);
    expect(chips().map((c) => c.id)).toEqual(['fds']);
    query.error = null;
    query.data = data(0);
    query.dataUpdatedAt = 15_000;
    health = observeDaemonHealth(health, query);
    expect(health.prevAcceptErrors).toBe(16);
    expect(chips()).toEqual([]);
    query.data = data(1);
    query.dataUpdatedAt = 20_000;
    health = observeDaemonHealth(health, query);
    expect(health.prevAcceptErrors).toBe(0);
    expect(chips().map((c) => c.id)).toEqual(['fds']);
  });
});

describe('Waiting chip navigation', () => {
  test.each(['missing', 'hidden', 'open', 'no-button'])(
    'navigates first, focuses the Waiting button and opens only a closed popover (%s)',
    (state) => {
      const calls: string[] = [];
      let frame: FrameRequestCallback | undefined;
      const oldFrame = Object.getOwnPropertyDescriptor(globalThis, 'requestAnimationFrame');
      const oldDocument = Object.getOwnPropertyDescriptor(globalThis, 'document');
      Object.defineProperty(globalThis, 'requestAnimationFrame', {
        configurable: true,
        value: (cb: FrameRequestCallback) => {
          frame = cb;
          return 1;
        },
      });
      Object.defineProperty(globalThis, 'document', {
        configurable: true,
        value: {
          querySelector: (selector: string) =>
            selector === 'button.waiting-note'
              ? state === 'no-button'
                ? null
                : { focus: () => calls.push('focus'), click: () => calls.push('click') }
              : state === 'missing'
                ? null
                : { getClientRects: () => ({ length: state === 'open' ? 1 : 0 }) },
        },
      });
      try {
        showWaiting((screen) => calls.push(screen));
        expect(calls).toEqual(['sessions']);
        frame!(0);
        expect(calls).toEqual(
          state === 'no-button'
            ? ['sessions']
            : state === 'open'
              ? ['sessions', 'focus']
              : ['sessions', 'focus', 'click']
        );
      } finally {
        if (oldFrame) Object.defineProperty(globalThis, 'requestAnimationFrame', oldFrame);
        else Reflect.deleteProperty(globalThis, 'requestAnimationFrame');
        if (oldDocument) Object.defineProperty(globalThis, 'document', oldDocument);
        else Reflect.deleteProperty(globalThis, 'document');
      }
    }
  );
});

// Chip colours: the tray badge palette (gui.md §5.8), readable as 12px text on the chip tint.
describe('status bar chip colours', () => {
  const globals = readFileSync(join(import.meta.dir, '../app/globals.css'), 'utf8');
  const block = (sel: string) => {
    const i = globals.indexOf(sel);
    return globals.slice(i, globals.indexOf('}', i));
  };
  const blocks: Record<ThemeName, string> = {
    light: block(':root {'),
    dark: block(":root[data-theme='dark'] {"),
  };
  const read = (b: string, name: string) =>
    new RegExp(`${name}:\\s*(#[0-9a-fA-F]{6})`).exec(b)?.[1]?.toLowerCase();
  // --danger has one value for both themes; red chips paint mark, edge and tint with it.
  const danger = read(blocks.light, '--danger');
  const moduleCss = readFileSync(join(import.meta.dir, 'StatusBar.module.css'), 'utf8');

  test('the chip CSS tints with the percentages checked below', () => {
    expect(danger).toBe('#d4493a');
    expect(moduleCss).toContain('color-mix(in srgb, var(--ink) 10%, transparent)');
    expect(moduleCss).toContain('color-mix(in srgb, var(--ink) 16%, transparent)');
  });

  for (const theme of ['light', 'dark'] as ThemeName[]) {
    for (const sev of ['red', 'amber', 'purple', 'blue']) {
      test(`${theme}: ${sev} text >= 4.5:1 on its tint, mark >= 3:1 on the bar`, () => {
        const text = read(blocks[theme], `--sev-${sev}`);
        expect(text, `--sev-${sev} in ${theme}`).toBeDefined();
        const ink = sev === 'red' ? danger! : text!;
        const bg = TOKENS[theme].bgApp;
        for (const pct of [0, 10, 16])
          expect(contrast(text!, mix(ink, pct, bg))).toBeGreaterThanOrEqual(4.5);
        expect(contrast(ink, bg)).toBeGreaterThanOrEqual(3);
      });
    }
  }

  test('the tokens are the tray badge colours (Down/Alert, Waiting, Conflict, Syncing)', () => {
    const rs = readFileSync(join(import.meta.dir, '../../src-tauri/src/tray_palette.rs'), 'utf8');
    const tray = new Map<string, string>();
    const re = /\(Icon::(\w+)[^,]*, (true|false)\) => \[0x(\w\w), 0x(\w\w), 0x(\w\w)\]/g;
    for (const m of rs.matchAll(re))
      tray.set(
        `${m[1]}:${m[2] === 'true' ? 'dark' : 'light'}`,
        `#${m[3]}${m[4]}${m[5]}`.toLowerCase()
      );
    const icon = { red: 'Down', amber: 'Waiting', purple: 'Conflict', blue: 'Syncing' };
    for (const theme of ['light', 'dark'] as ThemeName[])
      for (const [sev, name] of Object.entries(icon))
        expect(read(blocks[theme], `--sev-${sev}`), `${sev} ${theme}`).toBe(
          tray.get(`${name}:${theme}`)
        );
  });
});
