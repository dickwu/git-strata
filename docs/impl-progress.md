# git-strata implementation progress

Branch: `impl/v1`. Ledger for the implementation run. design.md wins over module docs. Rulings live in `docs/impl-notes.md`.

## Reading

- Read: `docs/README.md`, `docs/design.md`, `docs/types.md`, `docs/config.md` (through §4 and appendix A as the default file), `docs/test-matrix.md`, module headings, `.superpowers/sdd/2026-10-22-module-docs/module-rulings.md` addenda, `docs/plans/2026-10-22-module-docs.md` (docs-phase plan, already done).
- Each module doc is read in full immediately before that module's tasks. Test-matrix rows are applied through the module §7 tables, not re-derived.

## Tasks

| Unit | Status | Tests |
| --- | --- | --- |
| `strata-types` (ids, errors, config V1–V11, design §6.7, default inter config) | done | `cargo test -p strata-types` → 35 passed |
| apfs §8 tasks | Tasks 1–6 done (errno/volume, clone, rooted open + seams, rooted destinations/rename/remove/fsync/meta, remove_tree_rooted/clone_from_rooted, Attrs/bulk parser/attrs_of/gen count); Tasks 7–8 done (getattrlistbulk walk/walk_with/list_dir; case_fold_key/case_fold_eq, Unicode 17 table); Task 9 done (FSEvents watcher, flush = FlushAsync + drain); Task 10 done (budgets: walk 30k 0.23 s, clone 100k 0.74–0.91 s; 4-way parallel clone ≈ serial; real-layout spike not run, inter off-limits) — apfs complete | |
| gitio §8 tasks | not started | |
| store §8 tasks | not started | |
| overlay §8 tasks | not started | |
| deps §8 tasks | not started | |
| session §8 tasks | not started | |
| publish §8 tasks | not started | |
| search §8 tasks | not started | |
| confirm §8 tasks | not started | |
| daemon §8 tasks | not started | |
| cli §8 tasks | not started | |
| `skills/git-strata/SKILL.md` | not started | |

## Blocked

None.
