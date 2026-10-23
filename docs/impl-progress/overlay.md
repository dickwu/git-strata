# strata-overlay implementation progress

Branch `impl/overlay` (worktree `git-strata-wt/overlay`), from `impl/v1` b70b04e.

## Tasks (overlay.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 overlay shape rules and pairing | done | 195a6da |
| 2 .lww rules in the LayerFS glob dialect | done | (this commit) |
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
- Task 2: a leading `/` anchors the pattern to the repo root even when the
  rest has no `/` (`/top.bin` matches `top.bin`, not `x/top.bin`), as in
  LayerFS where every pattern starts at `/`.
- Task 2: a trailing `/**` needs at least one name below (`a/**` matches
  `a/b`, not `a` itself); `**/` and consecutive `**/**/` may be zero
  directories; any other `**` matches any run including `/`. `\` escapes the
  next character; there are no `[...]` classes (not in the LayerFS dialect
  as quoted in §5.10).
- Task 2: patterns are NFC-normalised when parsed (paths already are), so a
  decomposed pattern matches; matching is byte-exact otherwise.
- Task 2: `parse` ignores `%include` lines (it has no tree to read them
  from); `load` splices included rules in place. The include path is a repo
  path (leading `/` dropped) read with `apfs::open_rooted_file`, so a
  symlinked rules file or a symlinked parent reads as empty (NotFile /
  NotDirectory), like a missing one, a directory or non-UTF-8 text. Other
  read errors are returned.
