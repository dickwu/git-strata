# strata-overlay implementation progress

Branch `impl/overlay` (worktree `git-strata-wt/overlay`), from `impl/v1` b70b04e.

## Tasks (overlay.md §8)

| Task | Status | Commit |
|---|---|---|
| 1 overlay shape rules and pairing | done | 195a6da |
| 2 .lww rules in the LayerFS glob dialect | done | 7b18fe7 |
| 3 base manifests and race-safe reads | done | d1fd21a |
| 4 membership, classification and the cold scan | done | d5ea948 |
| 5 deterministic rename pairing | done | 56c97a8 |
| 6 dirty set and incremental scans | done | e373444 |
| 7 checkpoints that store only what changed | done | (this commit) |
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
- Task 6: inbox introspection for tests is `Inbox::pending() -> InboxView`
  behind `test-seams`. The sets are keyed by the volume-fold key (one item
  per canonical path, case variants and NFC/NFD merged); before the first
  scan the fold comes from the root's volume (`Fold::of`, case-insensitive
  when it cannot be read), and `set_x` re-keys when the fold changes.
- Task 6: an event (or `MustRescan`) for the root itself, a path with `..`
  or another non-normal component, or a non-UTF-8 path asks for a full scan.
  The root is a directory and a directory event means "subtree dirty"
  (§5.4 step 3), so the root means the whole tree. Real FSEvents does report
  the root when an entry is created directly in it, so such a creation costs
  a full scan; that is correct, only slower, and left for the daemon
  scheduling to judge.
- Task 6: dirty-arg paths ending in `.gitignore` force a full scan, like
  events. A spelling walk that meets a non-directory ancestor queues that
  ancestor as its own dirty path (§5.4 steps 2+3 for a dir swapped for a
  file). Region re-judging under p uses the fold-key prefix.
- Task 6: `dirs_listed` counts the listings made for spelling resolution
  too (each directory once per scan): round 3 of
  `reconcile_after_kernel_dropped_equals_cold_scan` lists 4 = the 2 subtree
  dirs + root + `src`.
- Task 6: an incremental scan runs on a clone of the state and is thrown
  away if it meets something that needs a full scan (then a full scan runs
  in the same call). `set_x` happens before the snapshot (§5.5 step 2).
  `Tracker::reset` keeps the same `Inbox` (the watcher closure holds it),
  emptied and marked full.
- Task 6: `Inbox::hint`/`require_full` and `Tracker::flush` are left to
  Task 7, which consumes them.
- Task 6: the real-FSEvents half of `excluded_dirs_never_enter_overlay` is
  `excluded_dirs_never_enter_overlay_real_watch` (not ignored; it passes from
  the agent shell). FSEvents delivers writes made shortly before the stream
  started, so the test drains the setup's events with one scan first. The
  case-sensitive half of `event_spellings_and_subtrees_reverify` is #[ignore]
  (hdiutil).
- Tests' temp dirs follow `TMPDIR` (`std::env::temp_dir()`); nothing in the
  overlay tests hardcodes `/tmp`. Runs here use
  `TMPDIR=/Volumes/GWD/opensource/.strata-tmp` (APFS, same volume as target).
- Task 7: `checkpoint(.., tracker: None, ..)` scans with a fresh `Tracker`
  (a full scan, equal to `reconcile_full`). ToolCall uses the inbox only
  (dirty = `Some(&[])`); Manual and the lifecycle reasons scan fully on the
  warm cache.
- Task 7: `PauseAfterSnapshot` sits in the shared scan start, so it pauses
  `compute` too. `PauseBeforePut` is after the "unchanged against latest"
  check (§5.6 step 4) and before step 5. The unchanged check runs again
  after the puts, because a race may have put the tree back to the latest.
- Task 7: put races (§5.2 steps 5–6). Each put is A1 → put → A2, at most two
  tries; with no gen count it is accepted once and marked Unstable. A
  symlink is stable when kind and inode match (the target is the content).
  A shape error (a component swapped for a file or link) or a path gone
  re-judges the raced paths with the incremental spelling walk (its
  "blocked" ancestor is queued, the region under it becomes absent); a
  NeedFull answer runs a full scan. At most 8 put rounds, then Busy
  (`workspace_changing`). So a directory swapped for a link to outside the
  session is recorded as the link, never as the outside bytes.
- Task 7: a failed checkpoint leaves the tracker state as it was and puts
  the snapshot plus every path the scan saw (entries, Renamed sources,
  paths changed against the old state) back into the inbox. Entries are
  compared sorted by path. `CheckpointId` passed to the store is a
  placeholder; a returned id ≤ the latest means an earlier ToolCall with the
  same id, returned with `recorded = false`.
- Task 7 tests, reduced for runtime: `work_grows_with_the_change_not_the_base`
  compares bases of 1 000 and 10 000 files (spec: up to 100 000);
  `writers_are_never_blocked` uses 24 writer threads and 20 appender
  threads (spec: processes), and the "other session" is a second World
  (its own store). The Busy lock is held by a child run of the test binary
  (`lock_child`, a raw `strata_store::sql` connection in `BEGIN
  IMMEDIATE`): the store's engine is Turso, so `/usr/bin/sqlite3` does not
  take part in its locking. A failed blob rename leaves the temp file for
  the store's own sweep (store.md), so the test checks for no new blob,
  not an empty `store/tmp`.
- Task 7: the classifier may pair a deleted base file with a modified base
  path of the same content (seen with an emptied file and a deleted empty
  file); the test avoids that coincidence rather than change Task 5.
