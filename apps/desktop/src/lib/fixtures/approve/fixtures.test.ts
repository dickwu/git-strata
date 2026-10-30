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

interface Shape {
  required: string[];
  optional: string[];
  /** Rust type of each field, with Option<> removed. */
  types: Record<string, string>;
}

/** Fields of `pub struct <name> { ... }`: required, optional, and each one's Rust type. */
function fields(src: string, name: string): Shape {
  const m = src.match(new RegExp(`pub struct ${name} \\{([\\s\\S]*?)\\n\\}`));
  if (!m) throw new Error(`struct ${name} not found`);
  const required: string[] = [];
  const optional: string[] = [];
  const types: Record<string, string> = {};
  for (const f of m[1].matchAll(/pub (\w+): ([^,\n]+),/g)) {
    const opt = f[2].match(/^Option<(.+)>$/);
    (opt ? optional : required).push(f[1]);
    types[f[1]] = opt ? opt[1] : f[2];
  }
  return { required, optional, types };
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
const proposalSrc = rs('strata-deps/src/proposal.rs');
const PROPOSAL = fields(proposalSrc, 'Proposal');
const ORIGINS = new Set(variants(proposalSrc, 'ProposalOrigin'));
const STATES = new Set(variants(proposalSrc, 'ProposalState'));

/** The id forms in strata-types/src/ids.rs (hex_id! lengths, SessionId, RepoName). */
const idsSrc = rs('strata-types/src/ids.rs');
const HEX_LEN: Record<string, number> = Object.fromEntries(
  [...idsSrc.matchAll(/^hex_id!\((\w+), (\d+),/gm)].map((m) => [m[1], Number(m[2])])
);
const NUM_IDS = new Set([...idsSrc.matchAll(/^num_id!\((\w+),/gm)].map((m) => m[1]));

type J = Record<string, unknown>;
const dir = import.meta.dir;
const fixtures = readdirSync(dir)
  .filter((f) => f.endsWith('.json'))
  .map((f) => [f, JSON.parse(readFileSync(join(dir, f), 'utf8'))] as [string, J]);

/** Whether `v` deserializes as Rust type `t` (the types these fixtures use; others fail). */
function typeOk(t: string, v: unknown, where: string): boolean {
  const vec = t.match(/^Vec<(.+)>$/);
  if (vec) return Array.isArray(v) && v.every((x, i) => typeOk(vec[1], x, `${where}[${i}]`));
  if (t in HEX_LEN)
    return typeof v === 'string' && v.length === HEX_LEN[t] && /^[0-9a-f]+$/.test(v);
  if (NUM_IDS.has(t)) return Number.isInteger(v) && (v as number) > 0;
  switch (t) {
    case 'SessionId':
      return typeof v === 'string' && /^s-[a-z2-7]{8}$/.test(v);
    case 'RepoName':
      return typeof v === 'string' && /^[A-Za-z0-9._-]+$/.test(v) && !/^\.\.?$/.test(v);
    case 'RepoPath':
      return typeof v === 'string' && v !== '' && !v.startsWith('/');
    case 'PathBuf':
      return typeof v === 'string' && v.startsWith('/');
    case 'String':
      return typeof v === 'string';
    case 'i32':
      return Number.isInteger(v);
    case 'SystemTime':
      return typeof v === 'string' && !Number.isNaN(Date.parse(v)) && /Z$|[+-]\d\d:\d\d$/.test(v);
    case 'Hold':
      return HOLDS.has(String(v));
    case 'BaseGen':
      return shapeOk(v as J, BASE_GEN, where);
    case 'ProposalOrigin': {
      const o = v as J;
      if (!ORIGINS.has(String(o?.kind))) return false;
      return o.kind === 'Session' ? typeOk('SessionId', o.session, where) : true;
    }
    case 'ProposalState':
      return STATES.has(String((v as J)?.kind));
    default:
      throw new Error(`${where}: no check for Rust type ${t}`);
  }
}

/** Keys and every field's value against a core struct. */
function shapeOk(o: J, shape: Shape, where: string): boolean {
  keysOk(o, shape, where);
  for (const [k, v] of Object.entries(o))
    expect([`${where}.${k}`, typeOk(shape.types[k], v, `${where}.${k}`)]).toEqual([
      `${where}.${k}`,
      true,
    ]);
  return true;
}

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
  shapeOk(f, OPEN_FILE, where);
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
    expect(HEX_LEN).toMatchObject({ TreeOid: 40, CommitOid: 40, DepsSetId: 64 });
    expect(NUM_IDS.has('ProposalId')).toBe(true);
    expect(PROPOSAL.required).toContain('created');
  });

  for (const [name, fx] of fixtures.filter(([n]) => n !== 'errors.json'))
    test(name, () => {
      for (const row of fx.results as J[]) {
        const where = `${name}:${String(row.session)}`;
        expect([where, typeOk('SessionId', row.session, where)]).toEqual([where, true]);
        if (row.kind === 'moved') {
          expect(Object.keys(row).sort()).toEqual(['kind', 'report', 'session']);
          const rep = row.report as J;
          shapeOk(rep, MOVE_REPORT, where);
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
      // The reply's proposal is the whole Proposal (handlers.rs: json_of(&after)).
      shapeOk(fx.proposal as J, PROPOSAL, `${name}:proposal`);
    });

  test('errors.json codes are core codes; a replacement is a whole Proposal', () => {
    const errors = fixtures.find(([n]) => n === 'errors.json')![1];
    for (const [k, e] of Object.entries(errors)) {
      expect([k, CODES.has(String((e as J).code))]).toEqual([k, true]);
      const r = ((e as J).detail as J | null)?.replacement;
      if (r) shapeOk(r as J, PROPOSAL, `${k}.replacement`);
    }
  });
});
