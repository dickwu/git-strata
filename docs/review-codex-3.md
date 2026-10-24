# git-strata: third focused review

| Finding | Verdict | Implementation and completeness (current file:line evidence) | Regression test and counterfactual assessment |
| --- | --- | --- | --- |
| N1 — directory selectors and partially changed reruns | **Fixed** | crates/strata-daemon/src/handlers.rs:1133 keeps each selector that selects current checkpoint content, using strata_publish::selection::selects. The shared exact-path, rename-source and descendant rules are at crates/strata-publish/src/stage.rs:212 and :237; stage itself uses the same hits function at :254. Empty selectors are removed individually only after comparing the prior publication's actual file modes and object IDs with the checkpoint base (handlers.rs:1160). Thus a changed directory is retained and unchanged b no longer blocks changed a. This implements daemon.md D22 without weakening rename/type-pair selection. | **Yes, by assertion/source inspection:** crates/strata-daemon/tests/daemon.rs:651 publishes a directory, changes an existing file and adds another, republishes both, then verifies no third commit. :690 publishes {a,b}, changes only a, and requires another successful commit with b unchanged. Both would fail with the old filter. Both were **socket-blocked** in this run. The existing stage suite, including rename/type transitions, passed **25/25**; it does not exercise daemon historical filtering. |
| N2 — already-landed guarded repo forces TTY | **Fixed** for the reported, stable-plan scenario | The CLI snapshots the batches before the gate, calls the read-only publish_plan only when guarded repos occur, and gates the intersection with remaining repos: crates/git-strata/src/confirm.rs:495, :516, :527. Planning and execution share rerun_plan (crates/strata-daemon/src/handlers.rs:470, :951). The exact checkpoint IDs used for planning are sent to publish. Skipped repos no longer enter the daemon's per-repo confirmation loop. The checkpoint-before-TTY exception is intentional cli.md D23; recording a checkpoint does not authorize a push. **New plan-to-prompt race: T1 below.** The documented conservative Waiting fallback also needs the reference qualification in T3. | **Yes:** crates/git-strata/tests/confirm_gate.rs:287 first publishes a guarded repo on a PTY, reruns without a TTY and requires no prompt/no new commit, then edits it and requires NotTty before publish. It would fail against the old gate. crates/strata-daemon/tests/daemon.rs:556 checks the plan's remaining set, unchanged publish rows/refs and no prompt. Both were **socket-blocked**. There is no new mixed guarded-A/unguarded-B conflict fixture or inter-RPC plan-change test. |
| N3 — after-commit state lost after push/while recording error | **Partially fixed** | The two reported late paths are repaired: with_landed preserves push/checks after BeforeRebase, BeforeDone and cleanup failures (crates/strata-publish/src/push.rs:565); final Done-row failures preserve them at :573; close_failed copies the original known commit/push/checks through row_moved and row_not_closed at :330. The daemon uses explicit push/checks instead of guessing (crates/strata-daemon/src/handlers.rs:1067), and the CLI consumes landed (confirm.rs:549). **Incomplete against the new “any error after commit” contract:** commit_into/move_chain failures bypass with_landed at push.rs:421, losing checks_ran; see R3. **New recording-error classification regression:** T2. | **Yes for late definite-push failures:** crates/strata-daemon/tests/daemon.rs:622 injects BeforeRebase and BeforeDone and requires landed.commit, Pushed and numeric checks_ran. Those assertions reject the old adapter. The test was **socket-blocked**. No added test forces close_failed's row_moved/row_not_closed branches or checks the late-failure CLI envelope. Existing crates/strata-publish/tests/push.rs:402 **passed**, including BeforeRefs, but never asserts its checks count or explicit push field; it misses R3. |
| N4 — settlement erases another push target's scene; restore omitted | **Partially fixed** | Branch separation, conservative retention of legacy records and restore settlement are implemented at crates/strata-daemon/src/resolve.rs:214, :289, :415; standalone push calls the same helper at handlers.rs:1056. **Destination/repository identity remains unfixed:** the stored remote is the constant string origin, and repo is only RepoName. A changed origin push URL or replacement repository under the same name still passes the predicate. This is an explicit scenario of N4, not a newly expanded review. See R4. | **Yes for the repaired portion:** crates/strata-daemon/tests/resolve.rs:464 **passed**. It requires matching restore scenes to disappear and other-branch/legacy scenes to remain; reverting the fix fails those assertions. It uses synthetic records and covers restore, not a real uncertain-send/cross-target/restart sequence. The existing standalone restart test at tests/daemon.rs:883 was **socket-blocked**. No new URL-change or RepoId-change test exists. |
| N5 — one deadline for publishing flag and queue retries | **Partially fixed** | resolve.rs:83 now creates one deadline and passes it through all flag waits and queue acquisitions. locks.rs:303 and :320 override the normal unbounded second take until a visible write. This fixes both the fresh-full-wait and unbounded-reacquisition problems. **Remaining expiry path:** a cleared flag lets the wait return after the deadline; a free queue accepts a zero-duration acquisition before checking expiry, and f then starts. See R5. The changed lock_wait reason for queue expiry is explicitly documented and reasonable. | **Yes for reacquisition:** crates/strata-daemon/tests/locks.rs:502 **passed**. After a request's first take, it tries a queue held for 1.5 s with a 200 ms deadline and requires Busy with no callback. Reverting the new bounded take makes that assertion fail. It does not force the full flag/queue/recheck loop, combine flag time with queue time, or test an expired deadline on a free queue. Existing rm-gap integration coverage was **socket-blocked**. |
| N6 — later-repo authentication/preparation errors lose earlier results | **Fixed** | crates/strata-daemon/src/handlers.rs:958 puts pinned/config lookup, require_validated and publication-context construction inside the per-repo attempt; :992 decorates all those failures with the stopped repo and accumulated published_before/already_published. Authentication remains before that repo's commit. CLI confirm.rs:545 retains both arrays and the failing repo's landed result. This closes every per-repo escape named in N6. | **Yes:** crates/strata-daemon/tests/daemon.rs:509 scripts cancellation for tool after front lands, requires front's commit in published_before and no tool commit, then requires front in already_published on rerun. The old early ? loses front and fails those assertions. The test was **socket-blocked**. It is a daemon/RPC regression, not a new real-CLI cancellation test. |

“Fixed” judges the original finding's described scenario; it does not approve unrelated behavior in the patch. Counterfactual assessments come from the assertions and baseline diff. No fix was reverted, no test source was changed, and socket-blocked tests did not execute their behavioral assertions.

## New findings and remaining gaps, ordered by severity

No Critical or High finding was established in this scope. R3/R4/R5 below are remaining portions of the requested findings; T1–T4 are new issues caused by these fixes or their documentation.

### Medium

#### R4 — N4 still equates a remote alias with destination identity

**Location:** crates/strata-daemon/src/resolve.rs:214, :245, :291. Both settlement callers: handlers.rs:1056 and resolve.rs:415.

An Unknown push of commit C to origin/main at destination A records only {repo, commit, remote:"origin", branch:"main"}. Change origin's push URL to destination B, restart if needed, and successfully push C or a descendant to B/main. The predicate still matches and deletes A's unresolved scene and warning. Reusing a RepoName for a different common directory has the same identity defect when that repository contains C. Keeping legacy records is sound, but the new records still lack the information required to prove settlement.

**Contract:** design.md §4 identifies a repository by its common directory, §6.7 identifies the configured publish target, and §9.6 requires preserving unresolved uncertainty. The second review's N4 explicitly named changing origin's push URL. daemon.md §5.5's new “same remote” decision is insufficient when implemented as a constant alias.

**Fix:** persist Git::repo_id() plus the effective push destination identity and branch at intent creation. Compare those identities against the successful send before deleting anything; retain scenes whose identities cannot be established. Add local-bare-remote A/B tests across restart and a same-name/different-common-dir case, for both standalone and restore settlement. No network remote is needed.

#### T1 — a changed publish plan can trigger LocalAuthentication from a non-TTY

**Location:** crates/git-strata/src/confirm.rs:516, :527, :535; crates/strata-daemon/src/handlers.rs:951, :962, :1162.

The plan and publish RPCs independently recompute history, but publish receives no planned remaining/confirmation set. Consider a guarded repo with an empty checkpoint on base C1 whose selected path is proven landed by a Done row C1. A non-TTY CLI's plan drops that repo, so it skips the TTY check and sends no shown entry. Before publish arrives, another call publishes new content for that session/path as C2 and closes its row Done. The second rerun_plan now consults C2, whose file differs from the request's immutable C1 base, so it retains the formerly skipped selector. The daemon enters require_validated and prompts before checking shown or staging.

The later shown-chain check or NoMatch prevents this example from committing. The regression is the authentication prompt/credential attempt from a command that never passed TTY, not a demonstrated unauthorized push. Reusing checkpoint IDs binds content; it does not bind the historical proof used to omit confirmation.

**Contract:** design.md §6.9 and §9.7 require TTY together with LocalAuthentication. cli.md §4 invariant 1 and D23 promise no prompt or credential from the pre-gate non-TTY path; §4 invariant 4 binds confirmation to one plan. The read-only preparation exception itself is reasonable, but its result needs revalidation before prompting.

**Fix:** carry the planned remaining/guarded set with the exact batch identities and reject a newly required guarded repo before require_validated. An explicit shown-entry check before prompting can be part of that check, distinguishing an absent entry from an intentionally displayed null tip. Do not hold publication locks while prompting. Test a paused CLI between publish_plan and publish with the C1→C2 change, and assert no prompt, no credential and no commit on the non-TTY request. Existing tests keep the two RPCs' plan stable.

#### T2 — copying PushRefused onto a recording failure hides the recording failure in the CLI

**Location:** crates/strata-publish/src/push.rs:115, :332; crates/git-strata/src/confirm.rs:557; final error selection at :602.

If the remote refuses a just-created commit and close_failed then gets a failed phase CAS or Store error, the outer error correctly becomes Unknown with reason row_moved/row_not_closed. The new carry helper also copies kind="PushRefused" to that outer detail. The unchanged CLI sees that kind, discards the outer error through (!refused).then_some(e), and synthesizes a plain PushRefused from the preserved push outcome. The commit and Refused result survive in data, but the Store/row failure, original cause and recovery reason disappear.

This is introduced by carrying the old classification key, not by the valid decision to retain commit/push/checks. Before this change, the recording failure stayed Unknown, although its partial data was missing.

**Contract:** design.md §9.6 requires an accurate account of uncertainty; publish.md §6, including the new row_moved/row_not_closed rule at :460, preserves the outer failure. cli.md §4's structured error/partial-result contract does not authorize replacing a recording failure with its nested remote refusal.

**Fix:** carry result fields commit/push/checks_ran independently of classification. Keep kind/message in the nested cause when the wrapper describes a different failure, or restrict the CLI's ordinary-refusal conversion to a direct refusal with a successfully recorded outcome. Add a CLI test that moves the Running row while publication is paused, then causes a remote refusal; require error.code=Unknown, the row failure reason/cause, and retained commits/pushes.

### Low

#### R3 — N3's new all-after-commit contract misses the chain-transaction path

**Location:** crates/strata-publish/src/push.rs:186, :189, :400, :421, :525; crates/strata-daemon/src/handlers.rs:1077; crates/git-strata/src/confirm.rs:578.

Checks have already run when commit_into writes the object and then move_chain fails (including the existing BeforeRefs seam or a real ref CAS failure). At :421, r? exits without with_landed. close_failed adds the known commit but has no checks count to preserve. The daemon emits landed.checks_ran=null, and the CLI sums it as zero. An executed nonzero check count is therefore reported incorrectly. The push is unsent in this case, so the adapter's null push happens to be correct; the explicit detail.push field promised by the new rule is still absent.

**Contract:** publish.md §6, :460 explicitly promises push and checks_ran for any error after the commit object exists, including the ref-transaction failure immediately above it. daemon.md §3 says landed uses those fields, and cli.md §4 describes the actual checks_ran count.

**Fix:** on a commit_into error for which written is Some, attach push=null and ran.ran before it reaches close_failed. Preserve pre-commit errors unchanged. Extend injected_failure_after_commit_is_unknown (crates/strata-publish/tests/push.rs:402) with at least one configured check and assert BeforeRefs metadata, including through a failed row close. Its existing passing assertions only prove object/ref/row behavior.

#### R5 — N5 can still start its guarded operation after expiry on a free queue

**Location:** crates/strata-daemon/src/resolve.rs:54, :86, :91; crates/strata-daemon/src/locks.rs:321, :154. Affected callers: handlers.rs:498 and resolve.rs:115.

wait_not_publishing_until checks the clock only while publishing is true. If the flag clears during the last 20 ms sleep and the thread next runs after the deadline, it returns Ok. with_sessions_until converts the expired deadline into Duration::ZERO, but Fifo::acquire accepts a free queue before examining timeout. The under-queue recheck also sees a clear flag and invokes f without checking expiry. Thus rm or resolve_move can begin after the advertised pre-mutation deadline. Usually the overshoot is a polling interval; scheduler or Store delays can extend it.

**Contract:** daemon.md §5.5's N5 addition and resolve.rs:76 promise one absolute deadline and no mutation on expiry. This residual is narrower than the repaired full-timeout/unbounded-second-take defects.

**Fix:** check the absolute deadline before taking the guarded queue and again immediately before f starts, preserving the documented unbounded completion only after the mutation begins. Keep general zero-duration try-lock behavior intact for try_session; it legitimately means “take now if free.” Add a helper-level test with an already expired deadline/free queue and a flag-clear-at-deadline case. The new held-queue test cannot detect this branch.

#### T3 — the command reference overpromises TTY-free reruns

**Location:** skills/git-strata/reference.md:20; limitation documented at docs/modules/daemon.md:108; current flow at handlers.rs:962 and :980.

The changed reference says TTY is needed only for a repo it “will still commit,” and that a rerun whose guarded repos already landed needs no TTY. The new daemon contract explicitly excludes the Waiting/NoCommit fallback from publish_plan: when dependency changes leave a session on its old base, its checkpoint still selects the landed content, so the repo remains in repos. The CLI gates it, although staging would return NoCommit and the handler would ultimately classify it already_published. This is a documented conservative planning choice, not a reason to auto-roll dependencies or weaken confirmation.

**Contract:** daemon.md §3's publish_plan row explicitly states the limitation; design.md §6.4 requires retaining the old base/deps while approval is pending. A reference claiming to describe the actual CLI must include that limitation.

**Fix:** say the TTY exemption applies to repos the content filter proves skippable, and mention that a Waiting session can still require a terminal/approval even when staging would find no new commit. Keep the actual flags unchanged. Add a guarded Waiting scenario to confirmation tests if the broader behavior is desired instead.

### Nit

#### T4 — the new method count and common CLI procedure were not fully reconciled

**Location:** docs/modules/daemon.md:68; docs/modules/cli.md:247 and :289.

The daemon introduction still freezes “these 19” methods, but its updated table, rpc.rs:50 and tests/rpc.rs:94 now contain **20**, including publish_plan; all **7** read-only classifications match. The CLI common flow still says guarded publish checks TTY “before connecting,” and the interactive subsection repeats the old rule, while D23 now requires checkpoint and publish_plan RPCs first. These are contradictions left by the additions, not evidence that the new RPC is an unapproved design deviation.

**Contract:** daemon.md §3 is the method inventory; cli.md §5/D23 define command ordering.

**Fix:** update 19→20 (or remove the hard-coded count) and explicitly refer guarded publish to D23 in both common/interactive procedures. A small doc/table consistency check would catch the count; no runtime behavior change is required.

## Scope and boundaries

Reviewed 2026-10-24, branch impl/v1 at f2502f8da5c0a32eaeb2f2419049b773b176c548, against 0d4c1ff. Read the second review as the N1–N6 scenario inventory, and used docs/design.md as the authority, with types/config, relevant publish/daemon/CLI/deps/Store/session contracts, impl-notes and implementation ledgers to interpret intentional decisions. H1 and the retained per-repository H4 publication design were not reopened. No other earlier finding is re-reported here.

The N2 checkpoint-before-TTY preparation, N1 content comparisons, conservative legacy-scene retention and own-process O_EVTONLY exclusion are reasonable intentional decisions on their merits. Their implementation limits are identified above. The shared selection refactor preserves stage's prior selection algorithm rather than introducing a second interpretation.

No new lock-order inversion or split Git ref transaction was established. The scene settlement calls run under the existing repo lock; the new planning path takes no write lock/queue and writes no Store state. The N5 helper retains flag-wait-with-no-lock → session queue → downstream repo lock. Store schema/transaction code is unchanged in this range; all 133 Store tests passed. A passing Store suite does not cover the new row-error propagation branches identified above.

The O_EVTONLY change is limited to the calling process and retains ordinary fds and other processes' event-only fds (crates/strata-deps/src/open_files.rs:255). Its counterfactual regression own_event_only_descriptors_are_not_holders (tests/open_files.rs:171) **passed**, including those two controls. The restart changes in tests/support/mod.rs:416 and :435 are fixture-only; production InstanceLock behavior was not relaxed, and the second-instance test still uses the single-attempt prepare_once at tests/startup.rs:147. No production regression was established from those fixture changes.

Independent review **unavailable**: both requested code-reviewer and architect lanes failed before reading files because their configured gpt-5.5 model is unsupported for this account. The evidence here is the primary lane's source review and test results, not independent architectural approval.

One targeted Nowledge MCP thread search located the prior second-review thread: query “git-strata review-codex-2 N1 N6”, mode normal, scope default, no additional filters, rank 1, thread codex-01a16c7d-f8b8-7b30-819e-728cfdf5bc85, title “Second git-strata review identifies incomplete fixes and directory re-run data loss risk”, relevance_score 37.04224650065104. No thread messages were fetched. Current checkout files and fresh command output determine this review.

## CLI, skill and RPC audit

All **25** built-binary help invocations exited 0 with empty stderr: root; new/status/checkpoint/restore/grep/files/diff; deps and propose/approve/list; publish/push/assign/rm/gc; daemon and start/stop/status; resolve/chain; config and relax. Only --help was invoked for the service commands; no service action was executed.

The reviewed argument/flag surface agrees with skills/git-strata/SKILL.md, reference.md and cli.md §3: repeatable publish --from, publish --assign, optional checkpoint --tool-call ID, deps propose --switch, and explicit resolve/chain choices. No new nonexistent flag or command was found. The changed TTY wording has the narrower runtime limitation in T3. Help text alone cannot establish confirmation, partial-error or historical-content semantics.

An independent read-only extraction compared the daemon method-name and read-only columns with rpc.rs: **20/20 names match, 7/7 read-only names match, no missing or extra method**. The stale numeric sentence is T4. The new publish_plan schema and handler agree on group, repos and already_published.

## Validation

Both exact requested commands were attempted from the repository root with offline dependency resolution:

    cargo build --workspace --offline
    env -u TMPDIR cargo test --workspace --no-fail-fast --offline

Both exited **101 before compilation/tests**, because /opt/homebrew/bin/sccache could not execute its rustc -vV probe: “Operation not permitted (os error 1).” No Cargo or system configuration was changed. The process-only wrapper bypass then ran the full commands:

| Command/check | Result |
| --- | --- |
| env RUSTC_WRAPPER= cargo build --workspace --offline | **Exit 0**. Workspace build passed; xcrun/linker confstr warnings remained. |
| env -u TMPDIR RUSTC_WRAPPER= cargo test --workspace --no-fail-fast --offline | **Exit 101: 813 passed / 120 failed / 25 ignored**, across **127** top-level unit/integration/doc-test targets; **35** targets failed. |
| target/debug/git-strata [command] --help | **25/25 passed**, no service action. |
| git diff --check 0d4c1ff..HEAD | **Exit 0**. |
| LC_ALL-unset push/staging filtered reruns, below | **1 passed each**, separate from full-suite totals. |

TMPDIR was unset for the complete suite and the filtered reruns. Cargo stayed offline; fixture fetches/pushes used temporary repositories and local bare remotes. The build completed before the full suite started. Test output was collected in tool/process memory, without creating log files. Totals use the final top-level summary for each target, excluding child-worker summaries printed within a target, and include the passing strata-apfs compile-fail doctest.

### Full-suite counts

| Crate | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| git-strata | 10 | 30 | 0 |
| strata-apfs | 98 | 42 | 7 |
| strata-confirm | 15 | 1 | 2 |
| strata-daemon | 69 | 36 | 0 |
| strata-deps | 61 | 3 | 1 |
| strata-gitio | 142 | 3 | 2 |
| strata-overlay | 79 | 4 | 6 |
| strata-publish | 66 | 1 | 0 |
| strata-search | 32 | 0 | 0 |
| strata-session | 73 | 0 | 4 |
| strata-store | 133 | 0 | 3 |
| strata-types | 35 | 0 | 0 |
| **Total** | **813** | **120** | **25** |

### Failure causes and limits

| Observed cause | Failed tests | Evidence / interpretation |
| --- | ---: | --- |
| Unix socket creation/bind denied | 78 | EPERM/no_permission in fake sockets, temporary daemon d.sock/daemon.sock and special-file fixtures. This includes child-worker and Perl socket fixtures. The six new socket-dependent regression tests account for the increase from the prior 114 failures to 120. They stop before their behavior assertions. |
| Disk-image fixtures unavailable | 25 | hdiutil create fails for APFS/case-sensitive APFS/HFS+ sparse images, generally without diagnostic stderr. Cross-volume, full-volume and filesystem fixtures cannot be established. |
| FSEvents service unavailable | 11 | FSEventStreamStart fails in watcher tests, including their independent canary, and in dependent daemon/overlay watcher fixtures. |
| LocalAuthentication service unavailable | 1 | la_non_interactive_round_trip_maps_codes observes Unsupported/cannot_evaluate, LA code 4099, instead of not_interactive. It did not authenticate. |
| Special-mode fixture precondition | 1 | special_mode_bits_are_normalized_and_warned sees 0755/0755 where its fixture requires 04755/02755, before normalization/restore is tested. |
| Git diagnostic contamination | 2 | The sandbox's DARWIN_USER_TEMP_DIR confstr warning turns the missing-config oracle into Err and contaminates the sibling-layout test's merged line positions. |
| Inherited LC_ALL | 2 | The push/staging environment assertions expect None but observe C.UTF-8. Both exact filtered cases pass when LC_ALL is unset. |
| **Total** | **120** | Environment/fixture failures remain counted as failures; none is counted as a product finding or relabeled a pass. |

The socket-worker classifications were also checked against their fixture sources (strata-deps/tests/common/mod.rs:680 and strata-overlay/tests/classify.rs:500). A read-only control, env -u TMPDIR git config --local --get codex-review-3-no-such-key.value, returned exit 1 plus the same confstr warning, supporting the missing-key oracle classification.

After the full suite, these two targeted reruns completed:

    env -u TMPDIR -u LC_ALL RUSTC_WRAPPER= cargo test -p strata-gitio --test push --offline push_sets_strata_publish_and_never_forces
    env -u TMPDIR -u LC_ALL RUSTC_WRAPPER= cargo test -p strata-gitio --test staging --offline stage_applies_clean_filters

Each was **1 passed / 0 failed** (respectively 5 and 8 filtered out). They do not revise the full-suite counts. No sandbox-restricted case was retried with broader permissions.

### Every failed test, by target

Counts are passed / failed / ignored from this run. Every failed test appears once below; cause names correspond to the table above.

**git-strata / tests/cli.rs — 3 / 22 / 0**

- Unix socket denied: `binary_file_is_marked_not_rejected`, `conflict_json_is_not_windowed`, `daemon_start_stop_do_not_use_the_socket`, `daemon_up_read_does_not_open_sessions`, `deps_propose_switch_moves_only_the_proposer`, `diff_lists_content_changes_only`, `diff_omits_hunks_for_binary_and_ignores_inode`, `grep_skips_nul_and_keeps_valid_hits`, `history_is_read_from_the_store_alone`, `hook_checkpoint_is_idempotent_per_tool_call`, `hook_replay_of_a_no_op_tool_call_returns_its_own_row`, `hook_tool_call_id_comes_from_stdin_tool_use_id`, `listing_is_one_document`, `lossy_utf8_one_fffd_per_bad_sequence`, `no_pagination`, `path_order_is_repopath_ord`, `publish_conflict_fix_rerun_loop`, `publish_push_refused_keeps_commit_and_push_in_data`, `publish_push_timeout_keeps_commit_and_push_in_data`, `readonly_works_without_daemon`, `results_live_in_data_not_a_log`, `shape_is_this_doc_not_a_golden`.

**git-strata / tests/confirm_gate.rs — 1 / 6 / 0**

- Unix socket denied: `confirm_commands_refuse_non_tty`, `no_env_or_flag_bypass`, `non_tty_refused_before_prompt`, `non_tty_rerun_of_a_landed_guarded_repo_is_not_gated`, `resolve_and_chain_require_an_explicit_choice`, `superseded_proposal_is_not_auto_approved`.

**git-strata / tests/envelope.rs — 6 / 1 / 0**

- Unix socket denied: `every_command_emits_ok_envelope`.

**git-strata / tests/readonly_without_daemon.rs — 0 / 1 / 0**

- Unix socket denied: `cli_readonly_commands_work_without_daemon`.

**strata-apfs / tests/attrs.rs — 14 / 1 / 0**

- Unix socket denied: `attrs_of_reports_every_field`.

**strata-apfs / tests/case.rs — 0 / 2 / 0**

- Disk-image fixture unavailable: `case_fold_eq_matches_a_case_insensitive_volume`, `case_sensitive_volume_distinguishes_case_but_not_normalization`.

**strata-apfs / tests/clone.rs — 8 / 10 / 0**

- Disk-image fixture unavailable: `clone_dir_refuses_cross_volume`, `clone_file_refuses_cross_volume`, `clone_on_unsupported_volume_is_unsupported`, `clone_refusals_leave_no_residue`, `clone_then_small_write_allocates_only_the_touched_extent`, `clone_tree_error_precedence`, `clone_tree_leaves_a_nested_mount_as_an_empty_stub`, `clone_tree_on_a_full_volume_leaves_no_destination`.
- Unix socket denied: `clone_file_rejects_directories_and_special_files`, `clone_tree_clones_fifos_and_sockets_as_objects`.

**strata-apfs / tests/clone_from.rs — 3 / 3 / 0**

- Unix socket denied: `clone_from_rooted_checks_the_source_kind`, `clone_from_rooted_clones_files_and_directories`.
- Disk-image fixture unavailable: `clone_from_rooted_error_codes`.

**strata-apfs / tests/errno.rs — 2 / 1 / 0**

- Disk-image fixture unavailable: `classify_io_error_matches_real_syscalls`.

**strata-apfs / tests/remove_tree.rs — 4 / 4 / 1**

- Disk-image fixture unavailable: `remove_tree_rooted_refuses_before_touching_anything`, `remove_tree_rooted_reports_what_it_cannot_remove`.
- Unix socket denied: `remove_tree_rooted_removes_a_protected_tree`, `remove_tree_rooted_takes_any_entry_kind`.

**strata-apfs / tests/rooted.rs — 12 / 1 / 0**

- Unix socket denied: `rooted_open_error_codes`.

**strata-apfs / tests/rooted_remove.rs — 2 / 2 / 0**

- Disk-image fixture unavailable: `remove_rooted_empty_dir_removes_only_empty_directories`.
- Unix socket denied: `remove_rooted_file_removes_one_entry_without_following`.

**strata-apfs / tests/rooted_rename.rs — 3 / 4 / 0**

- Disk-image fixture unavailable: `full_fsync_accepts_file_and_directory_handles`, `rename_rooted_error_mapping`, `rename_rooted_excl_renames_links_and_case_only_spellings`, `rename_rooted_swap_exchanges_two_existing_entries`.

**strata-apfs / tests/rooted_write.rs — 6 / 1 / 0**

- Disk-image fixture unavailable: `clone_tree_rooted_is_atomic_and_refuses_cross_volume`.

**strata-apfs / tests/volume.rs — 0 / 4 / 0**

- Disk-image fixture unavailable: `nearest_existing_ancestor_climbs_to_the_first_existing_path`, `same_volume_follows_links_and_reports_missing_paths`, `same_volume_tells_system_volume_from_data_volume`, `volume_caps_per_filesystem`.

**strata-apfs / tests/walk.rs — 24 / 1 / 1**

- Disk-image fixture unavailable: `walk_does_not_descend_into_mount_points`.

**strata-apfs / tests/watch.rs — 0 / 8 / 2**

- FSEvents unavailable: `dropping_the_watcher_stops_callbacks`, `flush_drains_the_callback_queue`, `watcher_excludes_configured_dirs`, `watcher_paths_use_the_callers_root_and_disk_spelling`, `watcher_reentry_from_the_callback_never_traps`, `watcher_reports_a_renamed_root_as_dropped`, `watcher_reports_file_and_directory_changes`, `watcher_reports_must_rescan`.

**strata-confirm / tests/la_bridge.rs — 1 / 1 / 0**

- LocalAuthentication unavailable: `la_non_interactive_round_trip_maps_codes`.

**strata-daemon / tests/daemon.rs — 0 / 15 / 0**

- Unix socket denied: `cli_readonly_commands_work_without_daemon`, `confirmed_publish_is_bound_to_the_shown_chain_top`, `e2e_checkpoint_restore_matches_oracle`, `failure_keeps_scene_no_replay`, `lost_rpc_reply_is_not_replayed`, `publish_confirmation_cancelled_after_a_repo_landed_keeps_it_in_the_envelope`, `publish_conflict_fix_rerun_succeeds_without_duplicates`, `publish_failure_after_a_definite_push_reports_the_pushed_commit`, `publish_plan_names_the_remaining_repos_and_writes_nothing`, `publish_rerun_of_a_directory_selector_commits_new_edits`, `publish_rerun_of_a_partly_changed_batch_commits_the_changed_path`, `reconcile_loop_rescans_skips_busy_and_stops`, `rm_waits_for_publishing_in_the_lock_free_gap`, `search_after_checkpoint_sees_uncheckpointed_changes`, `standalone_push_unknown_keeps_scene_and_reports_across_restart`.

**strata-daemon / tests/deps.rs — 0 / 4 / 0**

- Unix socket denied: `approve_unknown_proposal_is_refused_without_prompt`, `propose_on_drift_awaits_publish_and_approve_refuses_it`, `propose_switch_moves_the_proposer_without_prompt`, `propose_without_drift_records_nothing`.

**strata-daemon / tests/fetch_after.rs — 4 / 1 / 0**

- FSEvents unavailable: `hint_and_flush_off_callback_thread`.

**strata-daemon / tests/resolve.rs — 6 / 5 / 0**

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

- Git diagnostic contamination: `config::tests::config_values_read_like_git`.

**strata-gitio / tests/push.rs — 5 / 1 / 0**

- Inherited LC_ALL: `push_sets_strata_publish_and_never_forces`.

**strata-gitio / tests/staging.rs — 8 / 1 / 0**

- Inherited LC_ALL: `stage_applies_clean_filters`.

**strata-overlay / tests/checkpoint.rs — 17 / 1 / 0**

- FSEvents unavailable: `quiet_workspace_stays_clean`.

**strata-overlay / tests/classify.rs — 13 / 1 / 2**

- Unix socket denied: `special_files_are_never_opened`.

**strata-overlay / tests/incremental.rs — 7 / 1 / 1**

- FSEvents unavailable: `excluded_dirs_never_enter_overlay_real_watch`.

**strata-overlay / tests/restore.rs — 14 / 1 / 0**

- Special-mode fixture precondition: `special_mode_bits_are_normalized_and_warned`.

**strata-publish / tests/check.rs — 4 / 1 / 0**

- Git diagnostic contamination: `sibling_layout_matches_workspace`.

## Recommendation and short summary

Only docs/review-codex-3.md was written as review work. The pre-existing untracked .omc/ was preserved. No source, tests, skills or other documentation were edited; no checkout refs were changed, no commit/fetch/push was performed against this checkout, and no production daemon/launchctl action was run. Cargo and the requested tests produced their normal build artifacts and temporary fixtures. /Volumes/GWD/inter was not accessed.

**Recommendation: REQUEST CHANGES.** Independent architectural review is unavailable. Established issues in this scope: **Critical 0 / High 0 / Medium 3 / Low 3 / Nit 1**, including three residual N3/N4/N5 gaps and four new code/documentation issues.

**N1–N6: 3 Fixed / 3 Partially fixed / 0 Not fixed.** The original directory/mixed-selector failure, stable-plan TTY rerun and later-repo cancellation escape are repaired. Finish target identity, all-after-commit metadata and deadline expiry handling; bind the new plan before prompting and preserve recording errors independently of push classification. Build passed with the wrapper bypass; the full offline suite recorded **813 passed / 120 failed / 25 ignored**, with all observed failures classified above.

