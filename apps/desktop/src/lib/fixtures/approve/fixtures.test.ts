/**
 * The approve fixtures against the core's own serializers, read from the Rust sources in this
 * repo: a fixture that drifts from the daemon's wire shape fails here, not in a review.
 */
import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const crates = join(import.meta.dir, '../../../../../../crates');
const rs = (p: string) => readFileSync(join(crates, p), 'utf8');

/** Variant names of `pub enum <name> { ... }` (unit or struct variants). */
function variants(src: string, name: string): string[] {
  const m = src.match(new RegExp(`pub enum ${name} \\{([\\s\\S]*?)\\n\\}`));
  if (!m) throw new Error(`enum ${name} not found`);
  const body = m[1].replace(/\/\/.*$/gm, '').replace(/\{[^}]*\}/g, '');
  return [...body.matchAll(/^\s*([A-Z]\w*)\s*,?\s*$/gm)].map((x) => x[1]);
}

/** Field names of `pub struct <name> { ... }`, split into required and optional. */
function fields(src: string, name: string): { required: string[]; optional: string[] } {
  const m = src.match(new RegExp(`pub struct ${name} \\{([\\s\\S]*?)\\n\\}`));
  if (!m) throw new Error(`struct ${name} not found`);
  const required: string[] = [];
  const optional: string[] = [];
  for (const f of m[1].matchAll(/pub (\w+): ([^,\n]+),/g))
    (f[2].startsWith('Option<') ? optional : required).push(f[1]);
  return { required, optional };
}

const codesSrc = rs('strata-types/src/error.rs');
const CODES = new Set(
  codesSrc
    .slice(codesSrc.indexOf('codes! {') + 8, codesSrc.indexOf('}', codesSrc.indexOf('codes! {')))
    .split(/[\s,]+/)
    .filter(Boolean)
);
const SKIPS = new Set(variants(rs('strata-deps/src/rollout.rs'), 'SkipReason'));
const HOLDS = new Set(variants(rs('strata-deps/src/open_files.rs'), 'Hold'));
const OPEN_FILE = fields(rs('strata-deps/src/open_files.rs'), 'OpenFile');
const BASE_GEN = fields(rs('strata-types/src/model.rs'), 'BaseGen');
const MOVE_REPORT = fields(rs('strata-session/src/move_workspace.rs'), 'MoveReport');

type J = Record<string, unknown>;
const dir = import.meta.dir;
const fixtures = readdirSync(dir)
  .filter((f) => f.endsWith('.json'))
  .map((f) => [f, JSON.parse(readFileSync(join(dir, f), 'utf8'))] as [string, J]);

function keysOk(o: J, shape: { required: string[]; optional: string[] }, where: string) {
  for (const k of shape.required) expect([where, k, k in o]).toEqual([where, k, true]);
  for (const k of Object.keys(o))
    expect([where, k, [...shape.required, ...shape.optional].includes(k)]).toEqual([
      where,
      k,
      true,
    ]);
}

function openFile(f: J, where: string) {
  keysOk(f, OPEN_FILE, where);
  expect([where, HOLDS.has(String(f.hold))]).toEqual([where, true]);
  expect(typeof f.pid).toBe('number');
}

describe('approve fixtures match the core wire (handlers.rs apply_step)', () => {
  test('the core sources parsed', () => {
    expect(CODES.has('NoSuchProposal')).toBe(true);
    expect([...SKIPS].sort()).toEqual(
      ['AlreadyThere', 'Drift', 'Error', 'NotReady', 'OpenFiles', 'OwnChange', 'Untracked'].sort()
    );
    expect([...HOLDS].sort()).toEqual(['Cwd', 'Fd', 'Mapped']);
    expect(BASE_GEN.required).toEqual(['repo', 'tree', 'deps']);
    expect(MOVE_REPORT.optional.sort()).toEqual(['checkpoint', 'old_tree']);
  });

  for (const [name, fx] of fixtures.filter(([n]) => n !== 'errors.json'))
    test(name, () => {
      for (const row of fx.results as J[]) {
        const where = `${name}:${String(row.session)}`;
        if (row.kind === 'moved') {
          expect(Object.keys(row).sort()).toEqual(['kind', 'report', 'session']);
          const rep = row.report as J;
          keysOk(rep, MOVE_REPORT, where);
          for (const side of ['from', 'to']) keysOk(rep[side] as J, BASE_GEN, `${where}.${side}`);
          continue;
        }
        expect([where, row.kind]).toEqual([where, 'skipped']);
        const d = row.detail as J;
        if (typeof d.kind === 'string') {
          // A planned skip: a SkipReason, coded only for OpenFiles, Drift and Error.
          expect([where, SKIPS.has(d.kind)]).toEqual([where, true]);
          const want =
            d.kind === 'OpenFiles' || d.kind === 'Drift'
              ? d.kind
              : d.kind === 'Error'
                ? (d.error as J).code
                : undefined;
          expect([where, row.code]).toEqual([where, want]);
          if (d.kind === 'OpenFiles') for (const f of d.files as J[]) openFile(f, where);
          if (d.kind === 'Error') expect(CODES.has(String((d.error as J).code))).toBe(true);
        } else {
          // A failed move: the error's code plus its detail (or {message}).
          expect([where, CODES.has(String(row.code))]).toEqual([where, true]);
          if (row.code === 'OpenFiles') for (const f of d.holders as J[]) openFile(f, where);
        }
      }
    });

  test('errors.json codes are core codes', () => {
    const errors = fixtures.find(([n]) => n === 'errors.json')![1];
    for (const [k, e] of Object.entries(errors))
      expect([k, CODES.has(String((e as J).code))]).toEqual([k, true]);
  });
});
