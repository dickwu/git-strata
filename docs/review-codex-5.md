| Finding from review 4 | Verdict | Scenario, completeness and file:line evidence | Test that would fail without the fix; observed result |
| --- | --- | --- | --- |
| F1 — recorded destination differs from the send | **Partially fixed** | Both callers now pass the recorded URL: standalone `crates/strata-daemon/src/handlers.rs:1090`, `crates/strata-daemon/src/handlers.rs:1093` → `crates/strata-publish/src/push.rs:227`; restore `crates/strata-daemon/src/resolve.rs:487`, `crates/strata-daemon/src/resolve.rs:492`. With a supplied URL, gitio skips its second `remote get-url` call at `crates/strata-gitio/src/push.rs:104`. This repairs both review-4 schedules, for both callers, when the intervening changes are to `remote.origin.url/pushurl`. The fix is incomplete: the Git child can still rewrite the supplied URL using live config, while settlement now trusts that string without a post-send check (`crates/strata-daemon/src/resolve.rs:373`, `crates/strata-daemon/src/resolve.rs:377`). N1 below establishes the residual hole and a newly regressed settlement schedule. | **Yes for the ordinary URL-routing fix, with coverage gaps.** `crates/strata-gitio/tests/push.rs:255` asserts both new APIs send to A while origin is configured to B; A/B, tracking and anchor assertions at lines 270–279 would reject restoring the old config-resolving send. **Passed.** `crates/strata-daemon/tests/resolve.rs:516` forces a successful A→B→A between scene creation and recording; destination assertions at lines 562–564 fail if restore resumes using `push_recorded` without the recorded URL. **Passed.** No new test covers standalone's within-attempt interleavings, a real Unknown after retargeting between scene creation and send through either caller, or Git URL-rewrite changes. |
| F2 — plan_changed drops already_published | **Fixed** | The guard remains before the per-repo loop and every prompt/write: `crates/strata-daemon/src/handlers.rs:967`, loop at `crates/strata-daemon/src/handlers.rs:988`. Both its pinned-config error (line 969) and `plan_changed` (line 983) use `decorate_publish_error`. That helper retains repo, landed, published_before, already_published and Unknown reporting at `crates/strata-daemon/src/handlers.rs:1037`. It receives an empty current-call result list for preflight, so it correctly omits published_before. The original per-repo error exit uses the same helper at line 1026. This satisfies daemon.md §3's publish-response contract (`docs/modules/daemon.md:107`). | **Yes, by assertion and baseline comparison.** The extended `publish_confirmation_cancelled_after_a_repo_landed_keeps_it_in_the_envelope` at `crates/strata-daemon/tests/daemon.rs:558` requires the refused rerun to retain front's repo/commit in already_published (lines 604–605), omit published_before, avoid another prompt and leave tool uncommitted (lines 606–608). The old direct return fails the already_published assertions. **Blocked:** full runs and the exact filtered rerun fail at socket binding, `crates/strata-daemon/tests/daemon.rs:33`, EPERM/no_permission, before those assertions. There is no added direct config-error test or full CLI-envelope test for this exact F2 scenario; those paths are assessed by inspection. |
| F3 — reference overstates history-change rejection | **Fixed** | `skills/git-strata/reference.md:20` now specifies a guarded repo outside the CLI-covered set, matching `crates/strata-daemon/src/handlers.rs:970` and cli D23 (`docs/modules/cli.md:458`). It no longer promises rejection for every intervening history change. `skills/git-strata/SKILL.md:185` distinguishes plan_changed from a failed/cancelled authentication: the agent replans first and hands a resulting NotTty to the human. This is consistent with the pre-loop rejection and the retained TTY gate at `crates/git-strata/src/confirm.rs:527`. | **No test detects reverting these prose corrections.** e6719db changes documentation only; the Rust suite does not assert this wording. The behavioral gate tests can support the contract but cannot detect reverting reference.md or SKILL.md. The correction is verified by reading the changed lines against code and D23. |

Counterfactual claims above come from the new assertions and the scoped diff. No source/test patch was reverted or edited, and no counterfactual test run was performed. “Blocked” remains a failed test, not a behavioral pass.

## New findings, ordered by severity

No Critical or High finding was established in this three-commit scope.

### Medium

#### N1 — Git can still rewrite the “exact” URL, and settlement now discards the former conservative check

**Locations:** `crates/strata-gitio/src/push.rs:104`, `crates/strata-gitio/src/push.rs:115`, `crates/strata-gitio/src/push.rs:117`; `crates/strata-daemon/src/resolve.rs:373`, `crates/strata-daemon/src/resolve.rs:377`, `crates/strata-daemon/src/resolve.rs:385`, `crates/strata-daemon/src/resolve.rs:389`. Both affected caller paths are identified in the F1 table.

Passing the old URL as a process argument removes gitio's own second lookup. It does not make that argument a literal transport endpoint: Git itself reads config and applies `url.<base>.insteadOf` to explicit URL arguments. The staging Git directory shares the main common dir (`crates/strata-gitio/src/staging.rs:123`, `crates/strata-gitio/src/staging.rs:128`), and `tree_command` does not freeze or suppress URL rewriting (`crates/strata-gitio/src/command.rs:234`). `--end-of-options` protects option parsing, not URL resolution.

A concrete schedule newly regressed by f7e8970 is:

1. Local bare A and B both contain parent p. The chain contains c, a descendant of p; A still lacks c. An older Unknown scene for c at A is retained.
2. The next standalone push or restore reads A and writes A into its new scene.
3. While staging is paused, add `url.<B>.insteadOf = <A>` to the repository config and leave it present.
4. The new `*_to(A)` passes A to Git, but Git resolves it to B. B accepts c and the tracking/anchor transaction commits, so the result is Pushed.
5. `settle_push_scenes` compares the older scene's A with the supplied PushDest A and removes its directory/warning. A still lacks c.

Before f7e8970, the final `push_dest(git) != dest` check in `8804e1e:crates/strata-daemon/src/resolve.rs` observed B versus A and retained that older A scene. Removing this check is therefore a new regression for this schedule, rather than a restatement of an unrelated existing Git limitation.

The original F1 shapes also remain possible through URL rewriting: an Unknown send to rewritten B leaves an A record that a later definite A push can erase; adding the rewrite before send and removing it before recording recreates successful A→B→A despite the new API. Ordinary edits to origin's own url/pushurl are fixed; edits to the transport's rewrite rules are not.

**Contract:** design §6.7 (`docs/design.md:212`, `docs/design.md:217`) and invariant 6 (`docs/design.md:268`) require a definite destination before uncertainty can be settled. The new exact-destination promise appears at `docs/modules/gitio.md:267`, `docs/modules/gitio.md:1283`, `docs/modules/publish.md:141`, and daemon §5.5/D23 (`docs/modules/daemon.md:406`, `docs/modules/daemon.md:867`). These additions overclaim what supplying an argv string guarantees. The intentional decision to settle against the recorded destination without rereading config is reasonable only once the transport is actually bound to that destination.

**Read-only evidence:** installed Git is 2.54.0 (Apple Git-157). This command, run in the checkout, exited 0 and printed `/private/tmp/strata-review-B.git` on stdout:

```sh
git -c url./private/tmp/strata-review-B.git.insteadOf=/private/tmp/strata-review-A.git ls-remote --get-url /private/tmp/strata-review-A.git
```

`ls-remote --get-url` performs local resolution without contacting a remote. The input A/B paths need not exist; no file/config/ref was created or changed. This is transport-resolution evidence, not an end-to-end push reproducer. The subsequent wrong-settlement result is established by the send/settlement code. No operator `git push` was run.

**Concrete fix:** prepare an immutable transport/config view for the send, resolve the endpoint once in that view, and prevent the child from applying further URL rewrites or treating the resolved string as a configured remote alias. Preserve the intended hook/auth settings in that private view. Persist the endpoint used by that same prepared send before it starts. Until that binding is guaranteed, retain affected scenes conservatively. Restoring only the old post-send comparison mitigates the stable-rule schedule above but does not solve the Unknown or ABA schedules.

**Regression tests to add:** through both standalone and restore, pause after scene creation, add an A→B rewrite, and verify which bare remote receives the commit and which prior scenes/warnings survive. Include a real Unknown followed by success to A, a successful rewrite ABA, and a stable-rule case that would have been retained at 8804e1e. The two new tests use plain absolute paths and change only origin.pushurl, so both pass while N1 remains.

No additional Low or Nit finding is established. Missing test coverage is recorded explicitly rather than reported as another implementation regression.

## Scenario and regression audit

### F1's four requested ordinary schedules

This matrix concerns edits to `remote.origin.url/pushurl`; N1 addresses the remaining transport-rewrite case.

| Caller / schedule | Current behavior and evidence | Execution coverage |
| --- | --- | --- |
| Standalone / Unknown-to-B-then-A from review 4 | With scene A, `crates/strata-daemon/src/handlers.rs:1093` → `crates/strata-publish/src/push.rs:227` → gitio's supplied-URL branch sends A even if origin becomes B while staging. That attempt cannot produce the old “record A, Unknown at B” mismatch from an origin-only edit. An Unknown is retained by `crates/strata-daemon/src/resolve.rs:269`; later success at A may legitimately settle A. | Shared gitio routing test passed. No new standalone within-attempt/Unknown test. The existing standalone restart/retarget test at `crates/strata-daemon/tests/daemon.rs:991` is socket-blocked and changes the URL between attempts, so it would not detect reverting this F1 fix. |
| Restore / Unknown-to-B-then-A from review 4 | `crates/strata-daemon/src/resolve.rs:492` sends recorded A. Unknown becomes an error at line 496 and retains the A scene through with_staging, so an origin-only edit cannot make that record describe an Unknown send to B. | Shared `push_recorded_to` assertion passed. No new real Unknown/interleaving/restart test for restore. |
| Standalone / successful A→B→A | The send uses captured A throughout; a later config change does not alter which URL was supplied. A definite A push can settle matching A scenes. | No new standalone integration test for this schedule; source assessment plus the shared gitio test. |
| Restore / successful A→B→A | The new test pauses export, changes origin.pushurl to B, pauses after acceptance, restores A, then requires A=c, B=p and the older A scene removed (`crates/strata-daemon/tests/resolve.rs:533`, `crates/strata-daemon/tests/resolve.rs:555`, `crates/strata-daemon/tests/resolve.rs:562`). | New ABA test passed in both workspace runs and the required filtered restore run. |

### Other requested boundaries

- **Callers and dest=None:** both scene-writing callers use the recorded URL when PushDest is Some. If the initial read fails, `crates/strata-daemon/src/handlers.rs:1093` passes None and restore falls back at `crates/strata-daemon/src/resolve.rs:493`. gitio then resolves config as before. Even if config recovers and that push succeeds, `crates/strata-daemon/src/resolve.rs:378` settles nothing; null/legacy identities cannot match a later URL. This is conservative and explicitly documented in `docs/modules/daemon.md:406`. No new None-path regression is established; no dedicated None/recovery test was added.
- **Publish's own push:** `crates/strata-publish/src/push.rs:441` passes None deliberately, as D18 documents at `docs/modules/publish.md:752`. That flow uses its publish row/staging, does not write these standalone push scenes, and does not call settle_push_scenes. It is not an omitted recorded-URL caller introduced by these commits.
- **Empty/odd URLs and argv safety:** `crates/strata-gitio/src/push.rs:105` rejects an explicit empty URL before the send. The new test verifies InvalidInput/empty-url and unchanged remote state. URL and refspec are separate Command arguments after `--end-of-options` at line 117; no shell interpolation is added. Remote/source/branch validation remains at lines 90–92. Spaces or leading dashes are not split into options by this code. No new argument-injection finding was established. Named-remote interpretation/rewrite is a different issue covered by N1; there is no new odd-URL behavioral test.
- **Tracking refs and recording:** `remote` intentionally selects the logical `refs/strata/remotes/<remote>/<branch>` name (`crates/strata-gitio/src/push.rs:100`), independently of the supplied URL. The new test explicitly pins tracking/anchor updates at `crates/strata-gitio/tests/push.rs:274`. Both new APIs reuse the same transaction at `crates/strata-gitio/src/push.rs:266`; restore retains its chain verification. The existing tracking/anchor transaction test passed in the filtered restore run. No split ref transaction was introduced.
- **Settlement semantics:** Some-destination matching still requires repo name, remote, branch, RepoId, exact URL string and ancestry (`crates/strata-daemon/src/resolve.rs:381`). The retained exact-string/legacy policy is intentional and reasonable; no URL normalization is required for this review. Config edits after an actually literal A send need not prevent settling A. N1 concerns the unproven premise that the send was literal.
- **Publish error envelopes:** the body of `decorate_publish_error` is the former loop decoration extracted without changing its field logic. repo is added only when absent; a string commit yields landed via `landed_after_commit`; published_before and already_published are retained; Unknown still calls report_unknown (`crates/strata-daemon/src/handlers.rs:1038`). `landed_after_commit` still preserves explicit push/null and checks_ran at lines 1114–1121. The unchanged CLI reads all three partial-result fields (`crates/git-strata/src/confirm.rs:548`) and gives the original error priority (`crates/git-strata/src/confirm.rs:616`). No new loss of landed/results, refusal misclassification or Unknown reporting was established.
- **Docs and real CLI:** gitio/publish declarations and daemon's latest F1/F2 additions match the new call graph, subject to N1's exact-URL overclaim. daemon.md:406 retains the older R4 pre/post-read description and then explicitly supersedes it with F1's “no longer reread”; this is read as chronological supersession, not another finding. The narrowed reference/SKILL recovery agrees with D23; “nothing written” concerns the rejected publish RPC, whose preceding CLI checkpoints are documented by D23. Fresh `target/debug/git-strata publish --help`, `push --help` and `chain --help` each exited 0 with empty stderr and confirmed the documented relevant arguments, including `--from`, `--assign`, `push <REPO>`, `chain --repo` and `--restore`.

## Scope, authority and independent-review limit

Reviewed on 2026-10-24 in `/Volumes/GWD/opensource/git-strata`, branch `impl/v1`, HEAD `e6719dbb80d8ff042112d0d2deda1abb730e243a`. `git log --oneline 8804e1e..HEAD` contained exactly f7e8970, fad6010 and e6719db. All 13 changed files were inspected. Review 4's F1/F2/F3 supplied the scenario inventory; its other findings were not reopened.

docs/design.md is authoritative. README-ordered types/config and relevant gitio/publish/daemon/cli module contracts, impl-notes and implementation ledgers supplied context. The per-repo publication/rerun decision (`docs/impl-notes.md:81`), guarded-plan boundary, null/legacy retention and new prepared-URL decision were judged on their merits, including their [实现时补的决定] markings.

**Independent review unavailable:** both requested native code-reviewer and architect lanes failed before reading files: their tool-resolved `gpt-5.5` is unsupported for this account. No model override or substitute independent approval is claimed. This report contains the primary lane's bounded source review and fresh validation; no independent architectural verdict or merge-ready claim is made.

One targeted Nowledge MCP thread search (`git-strata fourth review review-codex-4 F1 F2 F3`, scope default, limit 3) returned an unrelated thread. It was not opened or used. Current checkout files and local command results determine these verdicts; no memory was written.

## Offline validation

All commands ran from the repo root. TMPDIR was unset for the full test suites and the executed targeted reruns. Cargo used --offline; transfer fixtures used their own temporary repositories/local bare remotes. No network lookup was used.

The four literal requested commands were attempted:

```sh
cargo build --workspace --offline
env -u TMPDIR cargo test --workspace --no-fail-fast --offline
cargo test --offline -p strata-gitio --test push
cargo test --offline -p strata-daemon --test resolve -- chain_resolve_restore
```

Each exited **101 before compilation/tests** because the inherited RUSTC_WRAPPER `/opt/homebrew/bin/sccache` could not execute its rustc -vV probe: **Operation not permitted (os error 1)**. No Cargo/system configuration was edited. The process-only bypass was `RUSTC_WRAPPER=`.

| Executed bypass/control command | Result |
| --- | --- |
| `env RUSTC_WRAPPER= cargo build --workspace --offline` | **Exit 0**, workspace build passed. xcrun/linker emitted the sandbox's DARWIN_USER_TEMP_DIR confstr warnings. |
| `env -u TMPDIR RUSTC_WRAPPER= cargo test --workspace --no-fail-fast --offline` | First run completed with **35 failed targets**; its verbose diagnostics exceeded the capture limit, so complete aggregate counts were not retained for that run. Repeated unchanged with an in-memory output parser: **Cargo exit 101, 820 passed / 124 failed / 25 ignored**, **127** top-level targets, **36** failed targets, **312.95 s**. No log/script file was created. |
| `env -u TMPDIR RUSTC_WRAPPER= cargo test --offline -p strata-gitio --test push` | **Exit 101: 7 passed / 1 failed / 0 ignored**. F1's new test passed; only push_sets_strata_publish_and_never_forces failed because inherited LC_ALL was C.UTF-8 rather than absent. |
| `env -u TMPDIR RUSTC_WRAPPER= cargo test --offline -p strata-daemon --test resolve -- chain_resolve_restore` | **Exit 0: 5 passed / 0 failed / 0 ignored**, 7 filtered out. Includes F1 ABA, destination settlement, atomic tracking/anchor, fast-forward and confirmation tests. |
| `env -u TMPDIR -u LC_ALL RUSTC_WRAPPER= cargo test --offline -p strata-gitio --test push` | **Exit 0: 8 passed / 0 failed / 0 ignored**. All push tests passed with the inherited locale removed. |
| `env -u TMPDIR RUSTC_WRAPPER= cargo test --offline -p strata-daemon --test daemon -- publish_confirmation_cancelled_after_a_repo_landed_keeps_it_in_the_envelope --exact` | **Exit 101: 0 passed / 1 failed / 0 ignored**, 16 filtered out. F2 test blocked at socket bind, EPERM/no_permission, before behavioral assertions. |
| `env -u TMPDIR -u LC_ALL RUSTC_WRAPPER= cargo test --offline -p strata-gitio --test staging -- stage_applies_clean_filters --exact` | **Exit 0: 1 passed / 0 failed / 0 ignored**, 8 filtered out. Confirms the second locale-dependent assertion failure. |
| `env -u TMPDIR RUSTC_WRAPPER= cargo test --offline -p strata-deps --test open_files -- own_event_only_descriptors_are_not_holders --exact` | **Exit 0: 1 passed / 0 failed / 0 ignored**, 3 filtered out. This unrelated test passed in the first workspace run, failed in the compact repeat, then passed here. The intermittent failure is retained in the repeat's counts. |

Counts below are from the fully captured compact workspace repeat, using the last top-level libtest summary for each target; nested child-worker summaries are excluded. The passing strata-apfs compile-fail doctest is included. Filtered reruns do not rewrite workspace totals.

| Crate | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| git-strata | 11 | 31 | 0 |
| strata-apfs | 98 | 42 | 7 |
| strata-confirm | 15 | 1 | 2 |
| strata-daemon | 72 | 38 | 0 |
| strata-deps | 60 | 4 | 1 |
| strata-gitio | 144 | 3 | 2 |
| strata-overlay | 79 | 4 | 6 |
| strata-publish | 68 | 1 | 0 |
| strata-search | 32 | 0 | 0 |
| strata-session | 73 | 0 | 4 |
| strata-store | 133 | 0 | 3 |
| strata-types | 35 | 0 | 0 |
| **Total** | **820** | **124** | **25** |

### Failure classification

| Key | Cause observed | Failed tests | Evidence / limitation |
| --- | --- | ---: | --- |
| S | Unix socket creation/bind denied | 81 | EPERM/no_permission in RPC and special-file fixtures. Includes F2 and the existing standalone Unknown/restart test; their behavioral assertions were not reached. Child-worker/Perl failures were checked against socket fixture sources. |
| D | Disk-image fixtures unavailable | 25 | hdiutil create/mount fixtures fail, generally without useful stderr. APFS/HFS+/cross-volume/full-volume preconditions cannot be established. |
| F | FSEvents unavailable | 11 | FSEventStreamStart/canary start fails in watcher and dependent daemon/overlay fixtures. |
| L | LocalAuthentication unavailable | 1 | LA code 4099, Unsupported/cannot_evaluate instead of not_interactive. No authentication completed. |
| M | Special-mode fixture precondition | 1 | restore test expects preserved 04755/02755 but reads 0755/0755 before exercising normalization. |
| G | Git stderr contamination | 2 | DARWIN_USER_TEMP_DIR confstr warning breaks the missing-config oracle's empty-stderr assumption and shifts the sibling-layout output lines. |
| E | Inherited LC_ALL | 2 | Push/staging environment assertions see C.UTF-8 instead of None. Both pass with LC_ALL unset. |
| T | Intermittent own-event-descriptor observation | 1 | own_event_only_descriptors_are_not_holders reports two own Fd holders in the repeat; it passed in the first run and the filtered rerun. Root cause was not established in this scoped review. Deps implementation/tests were untouched by the three commits; no causal regression finding is asserted. |
| **Total** | | **124** | All remain failed tests in the recorded workspace result. |

The **123 S/D/F/L/M/G/E failures** are environment/fixture restrictions or assumptions observed in this sandbox. T is an additional intermittent failure, not automatically attributed to the sandbox. None is promoted into a new F1–F3 implementation finding. The observed totals supersede the prompt's approximate expectation of ~114 failures.

### Every failed test, by target

Numbers are **passed/failed/ignored** for the compact workspace repeat. Cause keys refer to the table above.


**git-strata**

- tests/cli.rs — **3/22/0**: S: `binary_file_is_marked_not_rejected`, `conflict_json_is_not_windowed`, `daemon_start_stop_do_not_use_the_socket`, `daemon_up_read_does_not_open_sessions`, `deps_propose_switch_moves_only_the_proposer`, `diff_lists_content_changes_only`, `diff_omits_hunks_for_binary_and_ignores_inode`, `grep_skips_nul_and_keeps_valid_hits`, `history_is_read_from_the_store_alone`, `hook_checkpoint_is_idempotent_per_tool_call`, `hook_replay_of_a_no_op_tool_call_returns_its_own_row`, `hook_tool_call_id_comes_from_stdin_tool_use_id`, `listing_is_one_document`, `lossy_utf8_one_fffd_per_bad_sequence`, `no_pagination`, `path_order_is_repopath_ord`, `publish_conflict_fix_rerun_loop`, `publish_push_refused_keeps_commit_and_push_in_data`, `publish_push_timeout_keeps_commit_and_push_in_data`, `readonly_works_without_daemon`, `results_live_in_data_not_a_log`, `shape_is_this_doc_not_a_golden`.
- tests/confirm_gate.rs — **1/7/0**: S: `confirm_commands_refuse_non_tty`, `no_env_or_flag_bypass`, `non_tty_publish_refuses_a_guarded_repo_the_plan_left_out`, `non_tty_refused_before_prompt`, `non_tty_rerun_of_a_landed_guarded_repo_is_not_gated`, `resolve_and_chain_require_an_explicit_choice`, `superseded_proposal_is_not_auto_approved`.
- tests/envelope.rs — **6/1/0**: S: `every_command_emits_ok_envelope`.
- tests/readonly_without_daemon.rs — **0/1/0**: S: `cli_readonly_commands_work_without_daemon`.

**strata-apfs**

- tests/attrs.rs — **14/1/0**: S: `attrs_of_reports_every_field`.
- tests/case.rs — **0/2/0**: D: `case_fold_eq_matches_a_case_insensitive_volume`, `case_sensitive_volume_distinguishes_case_but_not_normalization`.
- tests/clone.rs — **8/10/0**: D: `clone_dir_refuses_cross_volume`, `clone_file_refuses_cross_volume`, `clone_on_unsupported_volume_is_unsupported`, `clone_refusals_leave_no_residue`, `clone_then_small_write_allocates_only_the_touched_extent`, `clone_tree_error_precedence`, `clone_tree_leaves_a_nested_mount_as_an_empty_stub`, `clone_tree_on_a_full_volume_leaves_no_destination`; S: `clone_file_rejects_directories_and_special_files`, `clone_tree_clones_fifos_and_sockets_as_objects`.
- tests/clone_from.rs — **3/3/0**: S: `clone_from_rooted_checks_the_source_kind`, `clone_from_rooted_clones_files_and_directories`; D: `clone_from_rooted_error_codes`.
- tests/errno.rs — **2/1/0**: D: `classify_io_error_matches_real_syscalls`.
- tests/remove_tree.rs — **4/4/1**: D: `remove_tree_rooted_refuses_before_touching_anything`, `remove_tree_rooted_reports_what_it_cannot_remove`; S: `remove_tree_rooted_removes_a_protected_tree`, `remove_tree_rooted_takes_any_entry_kind`.
- tests/rooted.rs — **12/1/0**: S: `rooted_open_error_codes`.
- tests/rooted_remove.rs — **2/2/0**: D: `remove_rooted_empty_dir_removes_only_empty_directories`; S: `remove_rooted_file_removes_one_entry_without_following`.
- tests/rooted_rename.rs — **3/4/0**: D: `full_fsync_accepts_file_and_directory_handles`, `rename_rooted_error_mapping`, `rename_rooted_excl_renames_links_and_case_only_spellings`, `rename_rooted_swap_exchanges_two_existing_entries`.
- tests/rooted_write.rs — **6/1/0**: D: `clone_tree_rooted_is_atomic_and_refuses_cross_volume`.
- tests/volume.rs — **0/4/0**: D: `nearest_existing_ancestor_climbs_to_the_first_existing_path`, `same_volume_follows_links_and_reports_missing_paths`, `same_volume_tells_system_volume_from_data_volume`, `volume_caps_per_filesystem`.
- tests/walk.rs — **24/1/1**: D: `walk_does_not_descend_into_mount_points`.
- tests/watch.rs — **0/8/2**: F: `dropping_the_watcher_stops_callbacks`, `flush_drains_the_callback_queue`, `watcher_excludes_configured_dirs`, `watcher_paths_use_the_callers_root_and_disk_spelling`, `watcher_reentry_from_the_callback_never_traps`, `watcher_reports_a_renamed_root_as_dropped`, `watcher_reports_file_and_directory_changes`, `watcher_reports_must_rescan`.

**strata-confirm**

- tests/la_bridge.rs — **1/1/0**: L: `la_non_interactive_round_trip_maps_codes`.

**strata-daemon**

- tests/daemon.rs — **0/17/0**: S: `cli_readonly_commands_work_without_daemon`, `confirmed_publish_is_bound_to_the_shown_chain_top`, `e2e_checkpoint_restore_matches_oracle`, `failure_keeps_scene_no_replay`, `lost_rpc_reply_is_not_replayed`, `publish_confirmation_cancelled_after_a_repo_landed_keeps_it_in_the_envelope`, `publish_conflict_fix_rerun_succeeds_without_duplicates`, `publish_failure_after_a_definite_push_reports_the_pushed_commit`, `publish_plan_keeps_a_waiting_sessions_landed_guarded_repo`, `publish_plan_names_the_remaining_repos_and_writes_nothing`, `publish_refuses_a_guarded_repo_outside_the_plan_before_prompting`, `publish_rerun_of_a_directory_selector_commits_new_edits`, `publish_rerun_of_a_partly_changed_batch_commits_the_changed_path`, `reconcile_loop_rescans_skips_busy_and_stops`, `rm_waits_for_publishing_in_the_lock_free_gap`, `search_after_checkpoint_sees_uncheckpointed_changes`, `standalone_push_unknown_keeps_scene_and_reports_across_restart`.
- tests/deps.rs — **0/4/0**: S: `approve_unknown_proposal_is_refused_without_prompt`, `propose_on_drift_awaits_publish_and_approve_refuses_it`, `propose_switch_moves_the_proposer_without_prompt`, `propose_without_drift_records_nothing`.
- tests/fetch_after.rs — **4/1/0**: F: `hint_and_flush_off_callback_thread`.
- tests/resolve.rs — **7/5/0**: S: `chain_resolve_refs_under_repo_lock`, `confirmed_push_is_bound_to_the_shown_chain_top`, `resolve_move_exclusive_choice`, `resolve_move_queues_and_does_not_hold_repo_lock`, `resolve_move_skips_tty_and_confirm`.
- tests/rpc.rs — **31/7/0**: S: `no_auth_facade_observation`, `no_auth_traced_diagnostic`, `no_branch_page_frame`, `no_duplex_read_timeout`, `no_encrypted_record`, `no_telemetry_beside_reply`, `readiness_is_socket_connect`.
- tests/startup.rs — **6/4/0**: S: `daemon_does_not_idle_exit`, `idle_close_is_not_ours`, `second_instance_refused_then_reacquired`, `unknown_publish_reported_on_restart_not_replayed`.

**strata-deps**

- tests/adopt.rs — **5/1/0**: S: `adopt_matches_inputs_hash_else_refuses`.
- tests/ensure.rs — **5/1/0**: S: `ensure_prefers_sealed_then_adopt_then_build`.
- tests/inputs.rs — **6/1/0**: S: `inputs_hash_rejects_special_files`.
- tests/open_files.rs — **3/1/0**: T: `own_event_only_descriptors_are_not_holders`.

**strata-gitio**

- unittests src/lib.rs — **29/1/0**: G: `config::tests::config_values_read_like_git`.
- tests/push.rs — **7/1/0**: E: `push_sets_strata_publish_and_never_forces`.
- tests/staging.rs — **8/1/0**: E: `stage_applies_clean_filters`.

**strata-overlay**

- tests/checkpoint.rs — **17/1/0**: F: `quiet_workspace_stays_clean`.
- tests/classify.rs — **13/1/2**: S: `special_files_are_never_opened`.
- tests/incremental.rs — **7/1/1**: F: `excluded_dirs_never_enter_overlay_real_watch`.
- tests/restore.rs — **14/1/0**: M: `special_mode_bits_are_normalized_and_warned`.

**strata-publish**

- tests/check.rs — **4/1/0**: G: `sibling_layout_matches_workspace`.

## Output boundary and recommendation

The only deliberate review-file edit is `docs/review-codex-5.md`. Existing implementation, tests, docs, refs and configuration were left intact; the pre-existing untracked `.omc/` was preserved. Cargo produced normal target artifacts and tests used temporary fixtures. No operator commit, push, fetch, ref-changing command, daemon-start service action or launchctl action was run against the workspace. `/Volumes/GWD/inter` was not accessed.

**Recommendation: REQUEST CHANGES.** Established scoped issues: **Critical 0 / High 0 / Medium 1 / Low 0 / Nit 0**; independent architectural review unavailable.

**Summary:** F1 is Partially fixed: both ordinary caller schedules are repaired, but live Git URL rewriting invalidates the new unconditional settlement assumption. F2 and F3 are Fixed; F2's behavioral test is socket-blocked, and F3 has no prose-regression test. Build passed with the wrapper bypass. The complete offline suite recorded **820 passed / 124 failed / 25 ignored** with the causes above; targeted restore passed **5/5**, and gitio push passed **8/8** with LC_ALL unset.
