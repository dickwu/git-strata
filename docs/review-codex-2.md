# git-strata: second focused review

| Finding | Verdict | Fix completeness and file:line evidence | Regression test; would reverting the fix fail it? |
| --- | --- | --- | --- |
| H2 — displayed push plan | **Fixed** | The CLI sends the displayed tip for push (`crates/git-strata/src/confirm.rs:70`), publish (`confirm.rs:475`), and restore (`confirm.rs:188`). The daemon compares it under the repo lock: `crates/strata-daemon/src/handlers.rs:1043`, `crates/strata-publish/src/push.rs:353`, `crates/strata-daemon/src/resolve.rs:341`. Missing/null shown values cannot authorize an existing tip. This closes the original display-to-daemon-read window, including guarded publish; the token still names the repo/branch, as daemon.md D21 intentionally specifies. | **Yes, by source inspection.** `crates/strata-daemon/tests/daemon.rs:385` rejects a stale null tip before a new commit/send; `tests/resolve.rs:650` covers stale/absent restore and standalone-push tips. Removing the comparisons permits the sends those assertions prohibit. Both test functions were **socket-blocked**; the direct restore portion precedes the socket failure. |
| H3 — publishing repo's dependencies | **Fixed** | `crates/strata-publish/src/stage.rs:729` finds the layout session's sealed set and privately clones its dependency directories; `stage.rs:758` calls it for the target repo before applying/staging paths and running checks (`src/push.rs:366`). This covers both public stage and publish, which share build. Existing tracked dependency directories are preserved intentionally. Choosing the already sealed pinned set, rather than cold-installing under a repo lock, agrees with the revised publish.md §5.2 step 4. | **Yes.** `crates/strata-publish/tests/check.rs:152` requires an executable existing only in the sealed node_modules, verifies private-copy isolation and exclusion from the Git tree. It **passed**; without the clone its file/executable assertions fail. It covers the chosen pinned-set policy, not installation of a newly merged dependency set. |
| H5 — terminal sealing errors | **Fixed** | `crates/strata-deps/src/seal.rs:406` retries only outer `Busy` errors for builds. A terminal refusal records an interrupted failed attempt, retains staging, and returns the lock error (`seal.rs:415`); adoption retains its refusal path. The build-lock guard can therefore drop during shutdown. | **Yes.** `crates/strata-deps/tests/build.rs:986` supplies `ReadOnly/shutting_down`, asserts one refusal, failed/interrupted metadata, retained installation, and a later successful build. It **passed**. The fake lock caps refusals at 100 (`tests/common/mod.rs:393`), so old code fails the test rather than hanging it forever. This is an injected lock test, not a real daemon shutdown during installation. |
| M1 — hourly reconciliation/GC | **Fixed** | Production serve spawns and joins the reconciliation thread (`crates/strata-daemon/src/handlers.rs:312`), initializes existing tracked watchers (`handlers.rs:186`), full-scans free session queues and calls GC under each repo lock (`handlers.rs:205`), and checks stop during interval waits (`handlers.rs:331`). `crates/strata-daemon/src/startup.rs:68` supplies the hourly production interval; status records passes. No hourly checkpoint reason or row was added. | **Yes.** `crates/strata-daemon/tests/daemon.rs:511` uses 150 ms scheduling, finds an unannounced file, checks unchanged checkpoint counts, skipped busy sessions, GC results and shutdown. Without the loop its autonomous-scan assertion cannot succeed. **Socket-blocked** before scheduling assertions. Startup watcher availability itself is not independently asserted by that test. |
| M2 — current search membership | **Fixed** | `crates/strata-daemon/src/handlers.rs:266` replaces cache-only returns with a current full-walk compute under an immediately available queue place; busy/error paths use read-only cold reconciliation (`handlers.rs:279`). Both grep and files attach through this source (`handlers.rs:445`, `handlers.rs:666`). Search still rejects a changed base (`crates/strata-search/src/lib.rs:242`). | **Yes.** `crates/strata-daemon/tests/daemon.rs:566` checkpoints first, then creates, renames, deletes and changes ignore rules without checkpointing; it also checks the following checkpoint still records the changes. Reverting the fix makes its new-file membership assertion fail. **Socket-blocked**. The busy-queue cold fallback has no new dedicated integration assertion. |
| M3 — proposer switch via CLI | **Fixed** | `--switch` exists in `crates/git-strata/src/args.rs:194`; `src/cmds.rs:695` forwards it and exposes moved/skipped data. The daemon uses the existing proposer-only plan under session queue → repo lock (`crates/strata-daemon/src/handlers.rs:729`); origin, drift and open-file checks remain in `crates/strata-deps/src/rollout.rs:393`. No approval of other sessions is implied. | **Yes.** `crates/git-strata/tests/cli.rs:764` uses the actual CLI and checks OpenFiles, proposer movement, another session remaining pinned, and no movement without the flag. Old CLI argument parsing rejects `--switch`. **Socket-blocked**. The built `deps propose --help` independently lists the flag. |
| M4 — structured after-commit results | **Partially fixed** | Timeout/refusal errors with a top-level commit now become `detail.landed` (`crates/strata-daemon/src/handlers.rs:1011`), and the CLI includes them in commits/pushes (`crates/git-strata/src/confirm.rs:508`). However `handlers.rs:1077` guesses push state from only `detail.kind`: a known successful push followed by rebase/finish failure becomes null and vanishes from pushes. A failure to close the publication row can also nest away the known commit (`crates/strata-publish/src/push.rs:299`). **See N3.** Earlier-repo results can also escape decoration at a later prompt: **N6**. | **Yes for the repaired cases, incomplete coverage.** `crates/git-strata/tests/cli.rs:811` and `:840` assert real CLI→daemon timeout/refusal data against temporary bare remotes. Both would reject the old empty data result; both were **socket-blocked**. Neither covers failure after a definite push or failure while recording the error. |
| M5 — removal/resolution versus publishing | **Partially fixed** | Both rm variants now use `with_queue_not_publishing` (`crates/strata-daemon/src/handlers.rs:491`); resolve_move uses the same helper (`src/resolve.rs:108`). The flag is checked before and again under the queue, and the queue is released before retrying (`resolve.rs:75`). This closes the original removal race and preserves lock direction. The newly documented one-wait deadline does not cover queue acquisition/reacquisition: **N5**. | **Yes for the race.** `crates/strata-daemon/tests/daemon.rs:621` pauses publish in its lock-free post-commit gap and checks both plain/discard rm wait and rebase remains intact. Without the gate rm proceeds in that gap. **Socket-blocked**. No test exercises a flag/queue recheck race followed by deadline expiry. |
| M6 — uncertain standalone/restore pushes | **Partially fixed** | Standalone push and restore share retained-scene handling (`crates/strata-daemon/src/resolve.rs:190`, `src/handlers.rs:1058`, `resolve.rs:361`). Unknown errors retain staging and acquire scene detail/warnings; restart scans records (`src/startup.rs:386`). Store reopening now accepts the daemon scratch directory (`crates/strata-store/src/open.rs:30`). This repairs the original unconditional deletion and missing restart report. New settlement is scoped only by repo name/ancestry, so a different push target can erase an unresolved scene: **N4**. Definite restore also lacks the settlement call used by standalone push. | **Yes for retention/restart.** `crates/strata-daemon/tests/daemon.rs:667` checks timeout scene retention, restart reporting and later settlement; it would fail against the old cleanup. **Socket-blocked**. The restore recording-failure test at `tests/resolve.rs:465` **passed** and asserts scene retention too. Missing: standalone record-failure/restart, cross-target settlement, and successful restore settlement. |
| M7 — atomic restore recording | **Fixed** | Restore now calls `Git::push_recorded` with a compare-only chain update (`crates/strata-daemon/src/resolve.rs:359`). The recording transaction includes tracking, anchor and chain verification (`crates/strata-gitio/src/push.rs:183`); a failed CAS returns Unknown without a lone anchor advance. Normal publish's `push_published` delegates to the same primitive (`push.rs:164`). | **Yes.** `crates/strata-daemon/tests/resolve.rs:465` pauses after remote acceptance, changes tracking, then asserts Unknown, retained scene and unchanged anchor. It **passed**. Old plain push never reaches the recording seam and/or fails those atomicity assertions. Existing fast-forward restore and publish recording tests also passed. |
| M8 — no-op tool-call replay | **Fixed** | `crates/strata-overlay/src/checkpoint.rs:376` restricts no-row coalescing to Manual at both skip points; every new ToolCall reaches the store's existing id deduplication. CLI read-only lookup remains available without the daemon (`crates/git-strata/src/cmds.rs:270`). No schema migration is needed; Manual coalescing is retained. | **Yes.** `crates/strata-overlay/tests/checkpoint.rs:805` checks distinct no-op IDs and T1→T2→replay-T1; it **passed**. `crates/git-strata/tests/cli.rs:101` tests the same no-op replay with the daemon down; **socket-blocked** during setup. Both distinguish the old result. |
| L1 — skill snapshot/manual/visibility wording | **Fixed** | `skills/git-strata/SKILL.md:10` describes isolation plus cross-session reads; `:85` describes Manual coalescing; `:90` describes fresh BeforePublish/current selected-path content and no historical selector. `skills/git-strata/reference.md:31` and `docs/modules/cli.md:279` agree. Runtime evidence is `crates/git-strata/src/confirm.rs:484` and `crates/strata-overlay/src/checkpoint.rs:388`. | **No automated test protects these prose corrections.** Reverting documentation alone would not fail the runtime suite. Manual help/source comparison confirmed the corrected behavior and flags; the Manual behavior is also covered by the passing overlay test above. New conflict-loop promises have separate gaps in N1/N2/N6; they do not undo these three L1 corrections. |
| H4 — conflict/fix/re-run loop only | **Partially fixed** | The chosen per-repo design is retained. The daemon skips historically landed, unchanged batches and reports `already_published` (`crates/strata-daemon/src/handlers.rs:946`); the CLI preserves those results (`crates/git-strata/src/confirm.rs:500`). The simple exact-file loop is implemented. Directory selectors can silently skip new edits; a mixture of changed/unchanged paths still yields NoMatch (**N1**). Already-landed guarded repos still force a CLI TTY (**N2**), and prompt failures lose earlier results (**N6**). | **Yes for the basic loop.** `crates/strata-daemon/tests/daemon.rs:444` and `crates/git-strata/tests/cli.rs:725` check an earlier landed repo and later conflict, then a successful identical re-run without duplicate commits. Both would fail on the old NoMatch behavior; both were **socket-blocked**. They cover exact paths with push/confirmation disabled in the CLI fixture, not the selector/gate/error variants below. |

“Fixed” above is a source-level verdict for the named finding, with execution limits stated separately. Counterfactual test assessments come from reading assertions and the baseline diff; no fix was reverted and no mutation tests or new test files were created.

## New findings and remaining gaps in the scoped fixes

**Recommendation: REQUEST CHANGES. Critical 0 / High 1 / Medium 5 / Low 0 / Nit 0.** N1, N2, N4 and N5 concern newly added behavior/contracts; N3 and N6 are remaining holes in the requested fixes, rather than newly introduced lower-layer failures. These are source-backed cases; the sandbox failures are not evidence for them.

### High

#### N1. Re-run filtering can silently discard directory selections and cannot resume a partially changed batch

**Location:** `crates/strata-daemon/src/handlers.rs:949`, especially `:952` and `:961`; selection semantics at `crates/strata-publish/src/stage.rs:210`.

The new “untouched” check compares only exact entry paths and exact rename sources. Normal publish selection also expands descendants (`stage.rs:219`). The CLI preserves the supplied selector, rather than expanding it to files (`crates/git-strata/src/confirm.rs:363`).

A session can publish `--from S:src`, edit `src/a.ts`, then publish the same selector again. The new checkpoint contains `src/a.ts`, not an entry literally named `src`. The daemon therefore marks the batch untouched, finds the earlier Done row covering `src`, skips the repo and reports success/already_published. The newly selected edits never reach stage or commit. This needs neither concurrency nor a remote failure.

There is a second failure in the same all-or-nothing filtering: after a landed batch selected `a` and `b`, editing only `a` retains both selectors. Stage then rejects unchanged `b` with NoMatch (`stage.rs:229`), so the documented identical re-run stalls.

**Contract:** design.md §6.5's selected-path publication semantics; publish.md §5.3's batch selection; daemon.md D22 (`docs/modules/daemon.md:858`) says new edits still publish normally; `skills/git-strata/SKILL.md:132` promises that loop. The intentional per-repo choice does not require either failure.

**Fix:** share the actual path/descendant/rename selection rules with re-run classification. Remove only selectors proven to be already satisfied, retain changed selections, and preserve rename/type-pair atomicity. Never infer an unchanged directory from absence of an exact directory entry. Add tests for a second directory publish containing new edits, and for a multi-path landed batch with only one path edited before re-run. Existing re-run tests do not cover either case.

### Medium

#### N2. An already-landed guarded repo still prevents the agent from re-running the CLI

**Location:** `crates/git-strata/src/confirm.rs:444`, `:474`; daemon skip happens later at `crates/strata-daemon/src/handlers.rs:957`.

The CLI applies its TTY gate to all repos in the original selectors before the daemon can remove already-landed batches. Let guarded repo A land after a human-confirmed call, while unguarded repo B conflicts. After the agent fixes B, re-running the identical command from its normal non-TTY context returns NotTty solely because A is still selected. No second push of A is needed or performed.

**Contract:** daemon.md D22 explicitly says skipped repos need no confirmation. The new skill's conflict loop says the human is unnecessary unless assigned (`skills/git-strata/SKILL.md:116`). The CLI's existing early TTY rule remains correct for repos that really will push, but it was not integrated with the new skip policy. The CLI re-run test uses a fixture with push and confirm off (`crates/git-strata/tests/support/live.rs:61`), so it cannot detect this.

**Fix:** determine the remaining publication/confirmation plan through a shared read-only preparation step, bind it to checkpoint/plan identities, and gate only actual remaining pushes. If revalidation would introduce a guarded push, refuse and require a new human-reviewed call. Preserve real TTY/LocalAuthentication gates. Alternatively change the promised workflow to explicitly rebuild selectors for remaining repos; do not promise identical autonomous re-runs until supported. Add a guarded-A/unguarded-B CLI re-run test.

#### N3. After-commit results are still lost when failure is later than the push, or while recording the failure

**Location:** `crates/strata-daemon/src/handlers.rs:1077`; `crates/strata-publish/src/push.rs:519`, `:529`, `:299`; `crates/git-strata/src/confirm.rs:545`.

The M4 adapter recognizes only top-level kind=Unknown or kind=PushRefused. A successful recorded push followed by a rebase/cleanup/finish failure has no such kind. Although the lower layer knows Pushed and builds an outcome containing it, the adapter emits push=null, and the CLI omits this repo from data.pushes. The new helper comment equates null with “not sent”; here the remote has already accepted the commit. checks_ran is also replaced with null/zero.

There is another reachable hole: if closing a failed publication row itself fails, close_failed wraps the original error in detail.cause without copying its known commit to the outer detail (`push.rs:299`). The daemon only recognizes an outer detail.commit (`handlers.rs:1011`), so a post-commit timeout plus recording failure can still produce empty commits/pushes for the failing repo.

**Contract:** cli.md §4/§5 requires retaining after-commit results and real push outcomes; design.md §9.6 requires an accurate account of uncertainty. The intentional use of null for an unsent push is reasonable; inferring “unsent” from an unrelated later error is not.

**Fix:** propagate structured partial publication state independently of the error code/kind, including the known commit, checks count and push outcome through row-recording failures. Keep the overall error Unknown when final publication completion is uncertain. Extend the CLI tests to inject BeforeDone/BeforeRebase after a definite push and a failure to persist the error outcome. The existing passing lower-layer `crates/strata-publish/tests/push.rs:402` exercises BeforeDone and proves the remote has the commit, but never asserts the CLI envelope; the two new CLI tests only cover immediate timeout/refusal.

#### N4. A successful push to another target can erase an unresolved push scene

**Location:** `crates/strata-daemon/src/resolve.rs:202`, `:237`, `:258`; caller `crates/strata-daemon/src/handlers.rs:1065`.

The new scene record contains only repo name and commit. Settlement deletes every scene for that repo whose commit is an ancestor of the newly pushed commit; it checks no destination branch, remote push URL or repo identity.

For a repo without dependency directories, changing publish.branch is supported. An Unknown push of C to branch A can leave a scene; after a configuration change/restart, a definite push of C or a descendant to branch B deletes A's scene and warning. Success on B establishes nothing about the uncertain send to A. Changing origin's push URL has the same problem. The startup reader cannot recover the missing target from push.json.

**Contract:** design.md §6.7 permits distinct publish branches for repos without dependencies; §9.6 forbids discarding unresolved uncertain scenes. The new daemon.md §5.5 settlement decision (`docs/modules/daemon.md:402`) is intentionally implemented, but its repo-name/ancestry condition is insufficient to prove the original operation settled.

**Fix:** record stable repo identity, remote/destination identity and branch with each push intent. Settle only a proven successful push to that same target containing the intended commit, retaining legacy scenes with incomplete identities. Test an uncertain A push followed by a definite B push across restart. Also route definite restore success through the appropriate settlement helper: restore currently returns at `resolve.rs:380` without the standalone path's call, despite the new shared settlement promise.

#### N5. The publication wait helper does not enforce its documented overall deadline

**Location:** `crates/strata-daemon/src/resolve.rs:54`, `:75`; `crates/strata-daemon/src/locks.rs:87`, `:307`.

The helper's start time limits only time spent inside the “publishing is true” loop. The following with_sessions call receives a fresh full lock wait, not the remaining time. Thus almost one lock wait spent on the flag can be followed by another full queue wait before any removal/resolution starts.

After an under-queue recheck returns None, the next queue acquisition is worse: the request context already has took=true, so take_bound returns None and reacquisition can wait without a deadline. This does not invert lock order, but it contradicts the new bounded pre-write behavior for both rm and resolve_move.

**Contract:** the M5 addition to daemon.md §5.5 (`docs/modules/daemon.md:408`) and the helper's own comment promise one lock-wait bound for the entire wait, with Busy/publishing and no mutation on expiry.

**Fix:** carry one absolute deadline through every flag wait and pre-mutation queue acquisition/reacquisition, independently of the general “later acquisitions are unbounded” rule. Check shutdown and expiry before starting the guarded mutation. Preserve unbounded finishing only after the actual operation has begun writing. Add a short-deadline test that forces the flag/queue recheck race and then holds the queue past the remaining deadline. The new gap test verifies waiting, not this boundary.

#### N6. A later repo's confirmation refusal bypasses the partial-result wrapper

**Location:** `crates/strata-daemon/src/handlers.rs:970`, especially `:973`; result decoration only at `:1002`; CLI fallback at `crates/git-strata/src/confirm.rs:514`.

The new wrapper decorates errors returned by publish_bound, but require_validated still uses ? before that match. If repo A already landed and the human cancels repo B's prompt (or authentication is unavailable), the function returns immediately without published_before/already_published or the promised stopped-repo field. The CLI consequently returns ordinary empty error data even though A committed/pushed.

**Contract:** daemon.md D22 and cli.md §4's new partial-result promise (`docs/modules/cli.md:124`) apply to a failed later repo, including confirmation refusal; cli.md §6 explicitly says refusal of that repo does not roll back others (`:297`). This is an incomplete error-path repair, not an objection to per-repo publication.

**Fix:** decorate errors from the entire per-repo attempt, including pinned/config/context preparation and authentication, using the accumulated completed/skipped results. Keep authentication before that repo's commit. Add a two-repo test where A succeeds and B's scripted confirmation is cancelled, asserting retained A results and B's identity.

## Scope, method and intentional decisions

Reviewed on 2026-10-24: branch impl/v1 at `31b541eaf51814023b1810498dc5b26f1e46e10c`, against `35f2643`, with the first report `docs/review-codex.md` as the finding inventory. The scope is the intervening fixes and their callers/tests/docs, not another whole-workspace review.

design.md remains authoritative, with types.md, config.md, relevant module contracts, impl-notes and implementation ledgers used to interpret the fixes. H1's retained recovery design and H4's retained per-repo publication design are excluded from re-reporting. The pinned sealed-set cloning choice, no-row Manual behavior, one-row-per-new-ToolCall choice, and atomic restore recording were judged on their effects, not flagged merely for being implementation decisions.

No new lock inversion was established. Reconciliation releases each session queue before taking repo GC locks; sealing retries distinguish outer lock acquisition from inner mutation errors; restore now records chain verification/tracking/anchor in one Git transaction. The M5 finding concerns a deadline, not lock direction. No new Store schema/transaction algorithm was added in this range; the open-layout allowlist changed, and the full Store suite passed. These observations are bounded to the changed paths.

Independent review **unavailable**: both required code-reviewer and architect agents failed before reading files because their configured gpt-5.5 model is unsupported for this account. This document is the primary lane's review, not an independent architectural approval. A targeted Nowledge thread search for the prior review found no matching thread; current repository evidence determines these verdicts.

Only this review file was written by the review. The pre-existing untracked .omc/ was preserved. No refs, commits, push/fetch operations, production daemon start or launchctl action were performed against the checkout. Cargo and the requested tests generated their normal build outputs and temporary fixtures; fixture Git operations used temporary repositories/local remotes. No /Volumes/GWD/inter checkout was accessed.

## CLI and documentation audit

All **25** help calls exited 0 with empty stderr: root; new/status/checkpoint/restore/grep/files/diff; deps and its three subcommands; publish/push/assign/rm/gc; daemon and start/stop/status; resolve/chain; config and relax. These were help-only calls, not command actions.

The current argument/flag surface matches skills/git-strata/reference.md and cli.md §3, including deps propose --switch, optional hook IDs, repeatable --from, publish --assign, and recovery choices. The three L1 wording repairs agree with actual code. Behavioral contradictions introduced with the new conflict-loop guidance are N1/N2/N6; --help alone cannot validate them. The M4 null-push interpretation and M5 overall-deadline wording also need the repairs above.

H3's new “already sealed pinned set” explanation is consistent with its implementation and current workspace invariant. The older publish.md §5.2 step 1 still literally says to ensure that set, although publish does not call ensure there (`crates/strata-publish/src/push.rs:459`). Reconcile that procedural wording with the chosen validated-sealed-set policy; no additional reachable missing-install failure was established for a valid pinned generation.

## Validation

Both requested exact commands were attempted from the repo root with offline Cargo dependency resolution. Both stopped **before compilation/testing** because the configured /opt/homebrew/bin/sccache failed its rustc -vV probe with Operation not permitted (os error 1). No configuration file was changed. The complete commands were then run with only the compiler wrapper disabled for that process:

| Command/check | Observed result |
| --- | --- |
| `cargo build --workspace --offline` | Exit 101 at sccache; a repeat after the successful fallback was still blocked. |
| `env RUSTC_WRAPPER= cargo build --workspace --offline` | **Exit 0**, full workspace build completed. xcrun/linker confstr warnings remained. |
| `env -u TMPDIR cargo test --workspace --no-fail-fast --offline` | Exit 101 at sccache; no test counts for this initial attempt. |
| `env -u TMPDIR RUSTC_WRAPPER= cargo test --workspace --no-fail-fast --offline` | **Exit 101: 810 passed, 114 failed, 25 ignored**, across **127** unit/integration/doc-test targets; **35** targets failed. TMPDIR was unset. No build ran concurrently with this full suite. |
| Built CLI help | **25/25 passed**; no command action executed. |
| `git diff --check 35f2643..HEAD` | **Exit 0**. |

The full-suite totals include the passing strata-apfs compile-fail doctest and exclude child-process summaries printed inside parent tests (to avoid double-counting). Ignored helper/manual tests are counted as reported by the harness.

### Full-suite counts

| Crate | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| git-strata | 10 | 29 | 0 |
| strata-apfs | 98 | 42 | 7 |
| strata-confirm | 15 | 1 | 2 |
| strata-daemon | 67 | 31 | 0 |
| strata-deps | 60 | 3 | 1 |
| strata-gitio | 142 | 3 | 2 |
| strata-overlay | 79 | 4 | 6 |
| strata-publish | 66 | 1 | 0 |
| strata-search | 32 | 0 | 0 |
| strata-session | 73 | 0 | 4 |
| strata-store | 133 | 0 | 3 |
| strata-types | 35 | 0 | 0 |
| **Total** | **810** | **114** | **25** |

### Why the full suite failed

| Observed cause | Failed tests | Evidence/interpretation |
| --- | ---: | --- |
| Unix socket creation/bind denied | 72 | EPERM/no_permission in fake sockets, temporary daemons and special-file fixtures; includes fixture-child failures. These tests generally stop before their behavioral assertions. |
| Disk-image fixtures unavailable | 25 | hdiutil create of APFS/case-sensitive APFS/HFS+ sparse images fails in this sandbox, often without stderr. Cross-volume/full-volume cases cannot establish their fixtures. This is not evidence that ordinary APFS cloning is broken. |
| FSEvents service unavailable | 11 | FSEventStreamStart fails, including the watcher tests' independent canary streams. |
| LocalAuthentication service unavailable | 1 | la_non_interactive_round_trip_maps_codes gets Unsupported/cannot_evaluate, la_code 4099, rather than not_interactive. Authentication did not succeed. |
| Special mode-bit fixture precondition | 1 | special_mode_bits_are_normalized_and_warned expects 04755/02755 but observes 0755/0755 before restoration is tested. |
| Git diagnostic contamination | 2 | The sandbox's confstr warning breaks the missing-config oracle's empty-stderr expectation and the sibling-layout test's merged line-index expectations. |
| Inherited LC_ALL | 2 | Push/stage tests expect no inherited LC_ALL but see C.UTF-8. Both filtered reruns with LC_ALL unset passed. |
| **Total** | **114** | Environment/fixture failures are recorded as failed tests, not relabeled as passes or counted as product findings. |

A fresh read-only control, `env -u TMPDIR git config --file /dev/null --type=bool --get core.ignorecase`, returned exit 1 plus the same confstr diagnostic. This substantiates the config-oracle classification without changing configuration.

The two filtered reruns, performed after the full suite with `env -u TMPDIR -u LC_ALL RUSTC_WRAPPER=`, were:

- `cargo test -p strata-gitio --test push --offline push_sets_strata_publish_and_never_forces`: **1 passed, 0 failed**, 5 filtered out.
- `cargo test -p strata-gitio --test staging --offline stage_applies_clean_filters`: **1 passed, 0 failed**, 8 filtered out.

They do not change the full-suite totals. The 133 passing Store tests, restore CAS regression, sealed-dependency staging regression, terminal-seal regression and no-op ToolCall regression were executed in the full run. Socket-dependent CLI/daemon regression assertions remain unverified here.

### Every failed test, by target

Numbers are passed / failed / ignored for the full-run target. Cause groups below cover each failed test exactly once.

**git-strata / tests/cli.rs — 3 / 22 / 0**

- Unix socket denied: `binary_file_is_marked_not_rejected`, `conflict_json_is_not_windowed`, `daemon_start_stop_do_not_use_the_socket`, `daemon_up_read_does_not_open_sessions`, `deps_propose_switch_moves_only_the_proposer`, `diff_lists_content_changes_only`, `diff_omits_hunks_for_binary_and_ignores_inode`, `grep_skips_nul_and_keeps_valid_hits`, `history_is_read_from_the_store_alone`, `hook_checkpoint_is_idempotent_per_tool_call`, `hook_replay_of_a_no_op_tool_call_returns_its_own_row`, `hook_tool_call_id_comes_from_stdin_tool_use_id`, `listing_is_one_document`, `lossy_utf8_one_fffd_per_bad_sequence`, `no_pagination`, `path_order_is_repopath_ord`, `publish_conflict_fix_rerun_loop`, `publish_push_refused_keeps_commit_and_push_in_data`, `publish_push_timeout_keeps_commit_and_push_in_data`, `readonly_works_without_daemon`, `results_live_in_data_not_a_log`, `shape_is_this_doc_not_a_golden`.

**git-strata / tests/confirm_gate.rs — 1 / 5 / 0**

- Unix socket denied: `confirm_commands_refuse_non_tty`, `no_env_or_flag_bypass`, `non_tty_refused_before_prompt`, `resolve_and_chain_require_an_explicit_choice`, `superseded_proposal_is_not_auto_approved`.

**git-strata / tests/envelope.rs — 6 / 1 / 0**

- Unix socket denied: `every_command_emits_ok_envelope`.

**git-strata / tests/readonly_without_daemon.rs — 0 / 1 / 0**

- Unix socket denied: `cli_readonly_commands_work_without_daemon`.

**strata-apfs / tests/attrs.rs — 14 / 1 / 0**

- Unix socket denied: `attrs_of_reports_every_field`.

**strata-apfs / tests/case.rs — 0 / 2 / 0**

- Disk-image fixture (hdiutil): `case_fold_eq_matches_a_case_insensitive_volume`, `case_sensitive_volume_distinguishes_case_but_not_normalization`.

**strata-apfs / tests/clone.rs — 8 / 10 / 0**

- Unix socket denied: `clone_file_rejects_directories_and_special_files`, `clone_tree_clones_fifos_and_sockets_as_objects`.
- Disk-image fixture (hdiutil): `clone_dir_refuses_cross_volume`, `clone_file_refuses_cross_volume`, `clone_on_unsupported_volume_is_unsupported`, `clone_refusals_leave_no_residue`, `clone_then_small_write_allocates_only_the_touched_extent`, `clone_tree_error_precedence`, `clone_tree_leaves_a_nested_mount_as_an_empty_stub`, `clone_tree_on_a_full_volume_leaves_no_destination`.

**strata-apfs / tests/clone_from.rs — 3 / 3 / 0**

- Unix socket denied: `clone_from_rooted_checks_the_source_kind`, `clone_from_rooted_clones_files_and_directories`.
- Disk-image fixture (hdiutil): `clone_from_rooted_error_codes`.

**strata-apfs / tests/errno.rs — 2 / 1 / 0**

- Disk-image fixture (hdiutil): `classify_io_error_matches_real_syscalls`.

**strata-apfs / tests/remove_tree.rs — 4 / 4 / 1**

- Unix socket denied: `remove_tree_rooted_removes_a_protected_tree`, `remove_tree_rooted_takes_any_entry_kind`.
- Disk-image fixture (hdiutil): `remove_tree_rooted_refuses_before_touching_anything`, `remove_tree_rooted_reports_what_it_cannot_remove`.

**strata-apfs / tests/rooted.rs — 12 / 1 / 0**

- Unix socket denied: `rooted_open_error_codes`.

**strata-apfs / tests/rooted_remove.rs — 2 / 2 / 0**

- Unix socket denied: `remove_rooted_file_removes_one_entry_without_following`.
- Disk-image fixture (hdiutil): `remove_rooted_empty_dir_removes_only_empty_directories`.

**strata-apfs / tests/rooted_rename.rs — 3 / 4 / 0**

- Disk-image fixture (hdiutil): `full_fsync_accepts_file_and_directory_handles`, `rename_rooted_error_mapping`, `rename_rooted_excl_renames_links_and_case_only_spellings`, `rename_rooted_swap_exchanges_two_existing_entries`.

**strata-apfs / tests/rooted_write.rs — 6 / 1 / 0**

- Disk-image fixture (hdiutil): `clone_tree_rooted_is_atomic_and_refuses_cross_volume`.

**strata-apfs / tests/volume.rs — 0 / 4 / 0**

- Disk-image fixture (hdiutil): `nearest_existing_ancestor_climbs_to_the_first_existing_path`, `same_volume_follows_links_and_reports_missing_paths`, `same_volume_tells_system_volume_from_data_volume`, `volume_caps_per_filesystem`.

**strata-apfs / tests/walk.rs — 24 / 1 / 1**

- Disk-image fixture (hdiutil): `walk_does_not_descend_into_mount_points`.

**strata-apfs / tests/watch.rs — 0 / 8 / 2**

- FSEvents service: `dropping_the_watcher_stops_callbacks`, `flush_drains_the_callback_queue`, `watcher_excludes_configured_dirs`, `watcher_paths_use_the_callers_root_and_disk_spelling`, `watcher_reentry_from_the_callback_never_traps`, `watcher_reports_a_renamed_root_as_dropped`, `watcher_reports_file_and_directory_changes`, `watcher_reports_must_rescan`.

**strata-confirm / tests/la_bridge.rs — 1 / 1 / 0**

- LocalAuthentication service: `la_non_interactive_round_trip_maps_codes`.

**strata-daemon / tests/daemon.rs — 0 / 10 / 0**

- Unix socket denied: `cli_readonly_commands_work_without_daemon`, `confirmed_publish_is_bound_to_the_shown_chain_top`, `e2e_checkpoint_restore_matches_oracle`, `failure_keeps_scene_no_replay`, `lost_rpc_reply_is_not_replayed`, `publish_conflict_fix_rerun_succeeds_without_duplicates`, `reconcile_loop_rescans_skips_busy_and_stops`, `rm_waits_for_publishing_in_the_lock_free_gap`, `search_after_checkpoint_sees_uncheckpointed_changes`, `standalone_push_unknown_keeps_scene_and_reports_across_restart`.

**strata-daemon / tests/deps.rs — 0 / 4 / 0**

- Unix socket denied: `approve_unknown_proposal_is_refused_without_prompt`, `propose_on_drift_awaits_publish_and_approve_refuses_it`, `propose_switch_moves_the_proposer_without_prompt`, `propose_without_drift_records_nothing`.

**strata-daemon / tests/fetch_after.rs — 4 / 1 / 0**

- FSEvents service: `hint_and_flush_off_callback_thread`.

**strata-daemon / tests/resolve.rs — 5 / 5 / 0**

- Unix socket denied: `chain_resolve_refs_under_repo_lock`, `confirmed_push_is_bound_to_the_shown_chain_top`, `resolve_move_exclusive_choice`, `resolve_move_queues_and_does_not_hold_repo_lock`, `resolve_move_skips_tty_and_confirm`.

**strata-daemon / tests/rpc.rs — 30 / 7 / 0**

- Unix socket denied: `no_auth_facade_observation`, `no_auth_traced_diagnostic`, `no_branch_page_frame`, `no_duplex_read_timeout`, `no_encrypted_record`, `no_telemetry_beside_reply`, `readiness_is_socket_connect`.

**strata-daemon / tests/startup.rs — 6 / 4 / 0**

- Unix socket denied: `daemon_does_not_idle_exit`, `idle_close_is_not_ours`, `second_instance_refused_then_reacquired`, `unknown_publish_reported_on_restart_not_replayed`.

**strata-deps / tests/adopt.rs — 5 / 1 / 0**

- Unix socket denied: `adopt_matches_inputs_hash_else_refuses`.

**strata-deps / tests/ensure.rs — 5 / 1 / 0**

- Unix socket denied: `ensure_prefers_sealed_then_adopt_then_build`.

**strata-deps / tests/inputs.rs — 6 / 1 / 0**

- Unix socket denied: `inputs_hash_rejects_special_files`.

**strata-gitio / src/lib.rs — 29 / 1 / 0**

- Git confstr diagnostic: `config::tests::config_values_read_like_git`.

**strata-gitio / tests/push.rs — 5 / 1 / 0**

- Inherited LC_ALL: `push_sets_strata_publish_and_never_forces`.

**strata-gitio / tests/staging.rs — 8 / 1 / 0**

- Inherited LC_ALL: `stage_applies_clean_filters`.

**strata-overlay / tests/checkpoint.rs — 17 / 1 / 0**

- FSEvents service: `quiet_workspace_stays_clean`.

**strata-overlay / tests/classify.rs — 13 / 1 / 2**

- Unix socket denied: `special_files_are_never_opened`.

**strata-overlay / tests/incremental.rs — 7 / 1 / 1**

- FSEvents service: `excluded_dirs_never_enter_overlay_real_watch`.

**strata-overlay / tests/restore.rs — 14 / 1 / 0**

- Special mode bits stripped: `special_mode_bits_are_normalized_and_warned`.

**strata-publish / tests/check.rs — 4 / 1 / 0**

- Git confstr diagnostic: `sibling_layout_matches_workspace`.

## Short summary

**9 Fixed / 4 Partially fixed / 0 Not fixed.** The core H2/H3/H5 repairs are present; M1/M2/M3/M7/M8 and the original L1 corrections are implemented. Finish M4's result propagation, M5's deadline, M6's scene identity/lifecycle, and the H4 re-run selector/confirmation/error paths. The highest-priority new regression is silent omission of edits beneath a previously published directory selector.

Build passed with sccache bypassed. Full offline suite: **810 passed / 114 failed / 25 ignored**; all observed failures and their environment limits are listed above. **REQUEST CHANGES**, without independent architectural approval.

