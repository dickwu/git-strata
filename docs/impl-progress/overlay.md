# strata-overlay implementation progress

Branch `impl/overlay` (worktree `git-strata-wt/overlay`), from `impl/v1` b70b04e.

## Tasks (overlay.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 overlay shape rules and pairing | done | (this commit) |
| 2 .lww rules in the LayerFS glob dialect | todo | |
| 3 base manifests and race-safe reads | todo | |
| 4 membership, classification and the cold scan | todo | |
| 5 deterministic rename pairing | todo | |
| 6 dirty set and incremental scans | todo | |
| 7 checkpoints that store only what changed | todo | |
| 8 three-way rebase with .lww, and projection | todo | |
| 9 a restore that checkpoints first | todo | |
| 10 restore scenes, verification, cross-base projection | todo | |

## Ambiguity decisions

- Task 1: `validate` also refuses a live entry whose mode is not one of
  100644/100755/120000 (rule "V2"), not only mode 0; §4.3 V2 lists the three
  allowed modes.
- Task 1: `paired` groups entry paths and rename sources with a union-find
  over (a) the two halves of each rename and (b) every pair where one member
  is a directory prefix of another (the type-swap group). A directory that is
  only an implied parent (not an entry, not a from) is not a member, so
  `paired` of it is empty.
- Task 1: `get` uses binary search, with a linear fallback so an overlay that
  `validate` would refuse still answers.
