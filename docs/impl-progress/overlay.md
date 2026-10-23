# strata-overlay implementation progress

Branch `impl/overlay` (worktree `git-strata-wt/overlay`), from `impl/v1` b70b04e.

## Tasks (overlay.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 overlay shape rules and pairing | done | 195a6da |
| 2 .lww rules in the LayerFS glob dialect | done | 7b18fe7 |
| 3 base manifests and race-safe reads | done | d1fd21a |
| 4 membership, classification and the cold scan | done | d5ea948 |
| 5 deterministic rename pairing | done | (this commit) |
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
- Task 3: `Base` keeps the spec's field name `gen`, spelled `r#gen` because
  `gen` is a reserved keyword in edition 2024 (`base.r#gen`).
- Task 3: X1 is only the root `.git` (§5.1 "根下的 `.git`"); a nested `.git`
  is the scan's EmbeddedRepo case. X1-X2 match with the volume fold;
  `Fold::of` falls back to case-insensitive (the more excluding reading)
  when `volume_caps` fails.
- Task 3: the §5.2 read returns Gone when the path stops being a
  file/symlink between A1 and the read (open fails with
  NoSuchPath/IsDirectory/NotFile/NotDirectory and re-attrs agree), retrying
  once when re-attrs show a different file; the caller treats Gone as "not
  on disk this scan" and re-dirties it. Gen count unknown on either A1 or A2
  → read once, Unstable, never cached.
- Task 3: test seams are a process-wide registry keyed by the exact root
  path; faults are one-shot and several may be armed. `release_fault`
  releases the paused thread, or disarms all armed faults when none is
  paused. Extra seam `wait_fault(root)` (blocks up to 30 s for a pause) so
  tests do not poll. Without `test-seams` the hooks compile to nothing.
- Task 3: `build_manifest` hashes in parallel (rayon); size is the number of
  bytes hashed. A FIFO/socket/device under an excluded dir is never seen
  (the walk does not enter X), so it is not Unsupported.
- Task 4: `compute` rejects a tracker whose root is not `ws.root`
  (InvalidInput, reason `wrong_tracker`), besides the spec's `wrong_repo`.
- Task 4: a path that goes Gone mid-scan (§5.2) counts as absent this scan,
  is recorded unstable and is rescanned next time; no warning is emitted.
- Task 4: a FIFO/socket/device inside an ignored or embedded directory is
  silent unless its path is a base (M) or tracked key; elsewhere it gets
  SpecialFile. EmbeddedRepo is warned only when the parent is not already
  restricted. The embedded check (`d/.git` exists) runs before
  `is_ignored`, so an ignored embedded repo is still not entered.
- Task 4: stats: `dirs_listed` = directories walked + the start;
  `entries_seen` = entries the filter kept.
- Task 4: the FIFO test bounds the scan at 10 s rather than asserting a 1 s
  timing, since §7 asserts workload, never timing.
- Task 4: the case-sensitive-volume half of "excluded paths follow disk
  identity" is a separate #[ignore] test (needs `hdiutil attach` of a
  Case-sensitive APFS image; from this agent shell attach fails with
  "Permission denied", so it is for a human to run).
- Task 4: no rename pairing yet, so a case-only rename shows as Deleted +
  new path until Task 5 (`rename.rs`).
- Task 5: case-only pairing groups N once by `case_fold_key` when either
  fold folds (volume case-insensitive or `core.ignorecase`); both folds that
  fold use the same key, and a byte fold can never pair a v in M with an n
  not in M, so one grouping is exactly "equal under either fold". Case-only
  pairing does not look at type (a file renamed to a symlink of another case
  still pairs), as §5.3 step 1 says "不看内容".
- Task 5: exact-content pairing skips a candidate n that is a directory
  prefix of v or vice versa (`d/f` → file `d`, `f` → `f/x`): §5.3 rules those
  type swaps as Deleted + Added/Untracked, so they are never Renamed even
  when the content is equal (extra test
  `type_swap_with_equal_content_is_not_a_rename`). A base entry whose mode
  is neither file nor symlink (gitlink) never pairs.
- Task 5: the hard-link "moved link name" case pairs by path order: with
  `other.txt` linked as `z/other-link.txt` and moved to `a/other.txt`, the
  rename lands on `a/other.txt` (smallest path), `z/other-link.txt` stays
  Untracked.
- Task 5: the case-sensitive-volume half of
  `rename_case_only_recorded_as_rename` is a separate #[ignore] test; the
  disk-image helper moved to `tests/common` (`Image::case_sensitive`).
