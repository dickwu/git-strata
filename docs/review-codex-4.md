# git-strata: fourth focused review

| Finding | Verdict | Implementation, documentation and completeness | Test that would fail without the fix; result in this run |
| --- | --- | --- | --- |
| T1 — plan changes can prompt from a non-TTY | **Fixed** for the reported history-change scenario | The CLI sends the guarded set its TTY gate covered, including an explicit empty set: `crates/git-strata/src/confirm.rs:527`, `:537`. RPC parses it at `crates/strata-daemon/src/rpc.rs:185`. After recomputing the content plan, the daemon rejects a newly required guarded repo outside that set at `crates/strata-daemon/src/handlers.rs:965`, before `require_validated` at `:993` and before any publish body. This implements `docs/modules/cli.md:458` D23 and closes the C1/C2 inter-RPC scenario in review 3. The new early rejection has a separate partial-result regression, F2 below. | **Yes, by assertion and baseline inspection:** `crates/git-strata/tests/confirm_gate.rs:538` changes a Done row just before the second RPC, requires `NotConfirmed/plan_changed`, no second prompt and unchanged chain/origin. `crates/strata-daemon/tests/daemon.rs:441` additionally requires no row, commit or push with an omitted guarded repo, then a successful planned control. Both were **socket-blocked before behavioral assertions**. The CLI fixture hook is test-only and releases its mutex before invoking the hook (`tests/support/live.rs:183`). |
| T2 — recording failure gets misreported as a bare push refusal | **Fixed** | `carry` copies only commit/push/checks at `crates/strata-publish/src/push.rs:118`; both `row_moved` and `row_not_closed` use it at `:335` and `:338`. Their own reason and nested refusal cause survive. The CLI's `bare_refusal` also excludes wrappers with a reason (`crates/git-strata/src/confirm.rs:570`); `failed.or(bad)` at `:616` preserves the outer recording error over a synthesized refusal. This matches `docs/modules/publish.md:460`. | **Yes:** `crates/strata-publish/tests/push.rs:670` pauses after commit, moves the Running row, then refuses the send. It requires outer `Unknown/row_moved`, no outer kind, nested PushRefused, retained commit/Refused/checks and the other writer's unchanged outcome. **Passed**, in the 17/17 publish-push target. `crates/git-strata/src/confirm.rs:645` rejects both wrapper reasons and accepts a bare refusal; **passed**. A real Store-error/`row_not_closed` integration case and a full CLI recording-failure envelope test are still absent; the shared carry call and CLI error precedence cover them by inspection. |
| R3 — chain-ref failure loses checks and explicit unsent push | **Fixed** | Once `commit_into` sets `written`, its error now receives `push: null` and the actual check count before `close_failed`: `crates/strata-publish/src/push.rs:426`. Pre-object errors remain unchanged. `close_failed` adds the commit and retains those fields through either row-close failure (`:306`, `:335`, `:338`). The daemon consumes explicit fields at `crates/strata-daemon/src/handlers.rs:1104`; the CLI includes the landed result and sums checks at `confirm.rs:552` and `:592`. The BeforeRefs seam, real ref CAS failure and surrounding ref reads use the same exit. | **Yes:** `crates/strata-publish/tests/push.rs:652` runs two checks, injects BeforeRefs and requires the object, count 2, an explicitly present null push and no chain ref. **Passed**, in the 17/17 target. Reverting only the metadata addition fails its count/null assertions. No new test combines this exact chain failure with a failing row close, or exercises this exact error through the CLI; T2's moved-row test separately proves the shared result-field carry. |
| R4 — a different push destination/repo can settle an old scene | **Partially fixed** | The named stable retarget/restart and different-common-dir cases are repaired: local `Git::push_url` at `crates/strata-gitio/src/push.rs:48`; scene repo_id/push_url at `crates/strata-daemon/src/resolve.rs:252`; exact identity/branch/ancestor matching and conservative legacy retention at `:366`. Both standalone push (`handlers.rs:1080`, `:1089`) and restore (`resolve.rs:479`, `:502`) use it. However, the recorded URL and gitio's actual send URL are independent reads. The before/after comparison does not bind the attempt to its recorded destination; F1 below can still erase another destination's uncertainty. | **Yes for the stable cases:** `crates/strata-gitio/tests/push.rs:226` tests url versus pushurl B and verifies B receives the push; **passed**. `crates/strata-daemon/tests/resolve.rs:470` uses local bare A/B and synthetic records to retain other-URL, other-RepoId, other-branch and legacy scenes while settling the exact match; **passed**. The strengthened real Unknown/restart/retarget standalone test at `tests/daemon.rs:971` was **socket-blocked**. None changes the URL between the record read and send read, or forces an ABA change. |
| R5 — expired deadline can start a guarded step on a free queue | **Fixed** | `crates/strata-daemon/src/resolve.rs:118` checks the same absolute deadline after the final queue acquisition and publishing-flag read, immediately before the callback. Both rm (`handlers.rs:498`) and resolve_move (`resolve.rs:146`) use this helper. A flag clearing after expiry and a successful zero-duration take now return Busy/lock_wait with deadline=true without starting the step. General try-lock semantics remain intact. Unbounded completion once the callback starts is the explicit decision at `docs/modules/daemon.md:416`, not a missing deadline check. | **Yes:** `crates/strata-daemon/tests/locks.rs:533` tests an expired deadline/free queue, a flag read that returns clear after expiry, zero callbacks in both cases and a successful before-deadline control. **Passed**, in the 16/16 lock target. Without the new final check the first unwrap_err fails. The older held-queue/reacquisition test also passed. The rm lock-free-gap integration test was socket-blocked. |
| T3 — reference overpromises TTY-free already-landed reruns | **Fixed** for the Waiting limitation | `skills/git-strata/reference.md:20` now limits the exemption to content-proven landings and explicitly keeps a Waiting session on its old base subject to TTY/confirmation. This agrees with `docs/modules/daemon.md:108`, `handlers.rs:1173` and `handlers.rs:992`, and respects design §6.4's pending-deps boundary. A separate new overstatement about history changes appears in F3. | **No test detects reverting the actual documentation correction.** The added behavioral test `crates/strata-daemon/tests/daemon.rs:661` requires a Waiting rebase and the guarded repo still present in publish_plan; it was **socket-blocked**. It pins the intended limitation but would also pass against the pre-correction implementation: reverting reference.md alone would not fail it. It does not execute a second CLI confirmation. |
| T4 — method count and common CLI procedure disagree | **Fixed** | `docs/modules/daemon.md:68` now states 20; its inventory matches `crates/strata-daemon/src/rpc.rs:50`, including all seven read-only classifications. Both the common procedure (`docs/modules/cli.md:247`) and interactive subsection (`:289`) defer guarded publish's TTY check to D23's checkpoint → publish_plan → gate ordering. The two contradictions named in review 3 are gone. | **Yes for the inventory:** `crates/strata-daemon/tests/rpc.rs:496` parses the stated number, method names and read-only column and compares them with Method::ALL. **Passed**, in the RPC target's 31 passed / 7 socket-blocked results; reverting 20 to 19 fails. The two prose corrections are verified by reading, without an automated prose assertion. |

Verdicts assess the specified scenario and the documented contract. Counterfactual test claims above come from assertions and the baseline diff, not from reverting patches. No implementation or test file was edited. A socket-blocked test is a failed test whose behavioral assertions were not reached, not a behavioral pass.

## New findings and residual gaps, ordered by severity

No Critical or High issue was established in this seven-commit scope.

### Medium

#### F1 — scene identity is sampled separately from the URL actually sent to Git

**Locations:** `crates/strata-daemon/src/handlers.rs:1080`; `crates/strata-daemon/src/resolve.rs:252`, `:368`, `:479`; `crates/strata-gitio/src/push.rs:101`, `:108`.

The daemon reads destination A, writes A into push.json, exports/attaches staging, then calls the push layer. gitio independently resolves origin again at line 101 and sends that second URL. The daemon's repo lock serializes daemon operations; it does not prevent the user or another Git process changing the repository config.

Two concrete schedules break the new identity claim:

1. Read A for the scene; change origin's pushurl to B while staging is prepared; gitio sends to B and returns Unknown. The retained record says A, even though B is the uncertain destination. A later definite push of the commit or a descendant to A satisfies every settlement predicate and deletes B's unresolved scene/warning. There is no post-send identity validation for the Unknown record.
2. For a successful send, read A for metadata, B for the actual push, then restore A before settlement. The pre/post reads both say A, so an old A scene is deleted although only B was proven to receive this attempt. Equality at the two endpoints does not rule out an intervening change.

Both standalone and restore use the same split reads. The stable between-attempt URL-change tests correctly pass, but do not exercise either schedule.

**Contract:** design.md §6.7 and §9 invariants 3/6 (`docs/design.md:217`, `:265`, `:268`); the new same-destination settlement decision in daemon.md §5.5 (`docs/modules/daemon.md:406`). The new gitio decision at `docs/modules/gitio.md:1276` specifically claims using the same read implementation prevents disagreement with the actual push address. Separate calls do not establish that guarantee.

**Concrete fix:** resolve a push attempt's destination once and carry that same resolved value into the actual Git send and its persistent scene. An API accepting a prepared destination, or a preparation/receipt type shared with the scene writer, can enforce that relationship. Write the intent for that destination before sending. Returning the actual destination in the result alone is insufficient for a crash/Unknown record that was already written with a different one. A post-send reread may remain as an additional conservative retention check.

**Validation to add:** local bare A/B fixtures that pause between scene preparation and send resolution, and before recording/settlement. Cover Unknown-to-B with later success-to-A, plus the successful ABA case, through both callers. Require the unmatched scene and warning to remain across restart. This finding is a source-established interleaving; no new probe/test was written under this review's file restriction.

#### F2 — the new plan_changed return drops already_published results

**Locations:** `crates/strata-daemon/src/handlers.rs:963`, `:969`, `:1038`; `crates/git-strata/src/confirm.rs:548`, `:555`.

rerun_plan has already computed `already`, but the new preflight returns plan_changed directly, before the common error decoration. Its detail contains only reason, repo and planned. Consequently the CLI sees neither published_before nor already_published and returns the error with `data: {}`.

For example, rerun a group with a content-proven landed front repo and another guarded repo tool that the CLI's plan also skipped. Between plan and publish, change tool's historical proof as in the T1 fixture. The daemon still proves front landed, retains tool, and correctly rejects tool without prompting. It nevertheless loses front from the response. Before this patch, the error from tool's body flowed through the common decoration and retained earlier proven landings. This is a new exit introduced by the T1 fix, not a reopening of unrelated publication behavior.

**Contract:** daemon.md §3's publish response explicitly attaches repo/published_before/already_published to any per-repo error (`docs/modules/daemon.md:107`); cli.md §3.2 describes rerun landings in data.already_published (`docs/modules/cli.md:124`). The intentional per-repository agent rerun contract is also recorded in `docs/impl-notes.md:81` and `skills/git-strata/SKILL.md:132`.

**Concrete fix:** preserve the whole-group preflight before any prompt or publication, but route its errors through common response decoration. Include the already-known `already_published` entries on plan_changed (and relevant preflight config errors) and keep the stopped repo. There are no new published_before entries at that point. Do not move this guard into a later per-repo iteration where an earlier repo could already prompt or write.

**Validation to add:** extend the paused CLI history-change fixture with a second, stable already-landed repo. Require plan_changed plus that repo in both error detail and data.already_published, with no prompt, new publish row, commit or push from the refused request. The new single-repo fixtures do not assert this case.

### Low

#### F3 — the updated reference promises rejection for every intervening history change

**Location:** `skills/git-strata/reference.md:20`. Actual condition: `crates/strata-daemon/src/handlers.rs:965`.

The new sentence says that if history changes between planning and publishing, publishing fails with plan_changed. The parameter is only a set of guarded repository names; no history version or snapshot identity is compared. An unrelated Done row, a change confined to an unguarded repo, or a plan that shrinks while staying within the confirmed set does not meet the rejection condition and can succeed.

**Contract:** cli.md D23 (`docs/modules/cli.md:458`) and daemon.md §3 (`:107`) specify rejection for a newly required guarded repo outside the set. They do not require aborting on every history change. The narrower implementation is sufficient for T1's TTY boundary; this is a documentation overstatement, not evidence that every change needs a new authentication.

**Concrete fix:** replace that sentence with “If replanning requires a guarded repo outside the set covered by the CLI's TTY check, publish returns NotConfirmed/plan_changed before prompting or publishing; rerun the command.” Also qualify the generic NotConfirmed recovery row in `skills/git-strata/SKILL.md:185` for this new reason: D23 permits replanning first, with any resulting NotTty handed to the human. No confirmation was attempted in the rejected request.

There is no test asserting this reference sentence; the existing planned-repo success control tests the set check, rather than history-version equality.

## Scope, intentional decisions and boundaries

Reviewed 2026-10-24, branch `impl/v1` at `62788333ee3f21c0e0a05f0b2f02afc56a3defee`, diff `b4f76ca..HEAD`: f526dc7, cbbdb69, bda8027, 9ecd02f, 6e3f282, 045df81 and 6278833. All 19 changed files were inspected, with review 3 as the scenario inventory and design.md as authority; types/config, relevant module contracts, impl-notes and implementation ledgers supplied context. Earlier unrelated findings were not reopened.

The content-proven TTY exemption, conservative Waiting fallback, null/unreadable and legacy-destination retention, exact destination-string comparison and deadline boundary are reasonable intentional decisions on their merits. Their `[实现时补的决定]` status was respected. H4's retained per-repo publication model was not reconsidered.

The optional plan is a **restriction on this RPC**, not proof of authentication. Supplying a repo name does not issue or redeem a token: `require_validated` still runs before that repo's publish (`handlers.rs:992`), and publish still checks its action and shown chain under the repo lock (`crates/strata-publish/src/push.rs:388`). Omitting plan preserves existing non-CLI caller behavior as explicitly documented in `rpc.rs:188`; it is not a newly added LA bypass. The CLI always sends plan, including when empty. D23's pre-gate checkpoint preparation does not authorize a guarded push. No new bypass of invariant 7 was established on the specified CLI path.

No new lock-order inversion or split ref transaction was established. Planning and its guard use read operations without queues/repo locks or LA. Confirmation remains outside publication locks. The deadline helper retains flag wait without locks → session queue → downstream repo lock, releasing the queue on a publishing retry. T2 retains the other writer's Store outcome; R3 changes error metadata, not the Git transaction. Store schema/transaction implementation is unchanged; all 133 Store tests passed. The new URL reads occur under existing repo locks but cannot serialize external config writers (F1).

Independent review **unavailable**: the requested code-reviewer and architect lanes both failed before reading files because their configured gpt-5.5 model is unsupported for this account. No model override, substitute architectural approval or merge-ready claim is made. The findings are the primary lane's bounded source review and fresh validation evidence.

One targeted Nowledge MCP thread search located the prior third-review thread: query “git-strata third review review-codex-3 R4 T1”, requested scope default, limit 3, no other filters; returned mode full, rank 1, thread `codex-01a16cc9-159f-73b1-9527-4739696a2513`, title “Third git-strata review requests changes for three partially fixed issues”, relevance_score 55.61506729125976. No thread messages were fetched. Current checkout files and command output determine the verdicts; no memory was written.

## CLI, skill and RPC audit

All **25** `target/debug/git-strata [command] --help` invocations exited 0 with empty stderr: root; new/status/checkpoint/restore/grep/files/diff; deps and propose/approve/list; publish/push/assign/rm/gc; daemon and start/stop/status; resolve/chain; config and relax. Service commands were invoked with --help only; no service action was run.

The argument/flag surface agrees with skills/git-strata/SKILL.md, reference.md and cli.md §3: repeatable publish --from, publish --assign, optional checkpoint --tool-call ID, deps propose --switch and explicit resolve/chain choices. There is no new CLI plan flag; plan is an internal RPC field derived after the TTY gate. The Waiting qualification is now present in the reference. F3 identifies the new plan-history/recovery wording that still needs qualification. Help output establishes syntax, not confirmation or error-envelope semantics.

A separate read-only extraction found **20/20 method names and 7/7 read-only classifications** equal between daemon.md and rpc.rs. The new inventory test also passed. No extra/missing method or stale numeric count remains. The optional plan schema is present in both daemon.md and PublishParams.

## Validation

Both exact requested commands were attempted from the repo root with offline dependency resolution:

    cargo build --workspace --offline
    env -u TMPDIR cargo test --workspace --no-fail-fast --offline

Both exited **101 before compilation/tests**: /opt/homebrew/bin/sccache could not execute its rustc -vV probe, returning “Operation not permitted (os error 1).” No Cargo/system configuration was changed. A process-only wrapper bypass ran the full commands:

| Command/check | Result |
| --- | --- |
| `env RUSTC_WRAPPER= cargo build --workspace --offline` | **Exit 0**; workspace build passed, with xcrun/linker DARWIN_USER_TEMP_DIR confstr warnings. |
| `env -u TMPDIR RUSTC_WRAPPER= cargo test --workspace --no-fail-fast --offline` | **Exit 101: 819 passed / 123 failed / 25 ignored**, across **127** top-level unit/integration/doc-test targets; **35** targets failed. Measured wall time **313.65 seconds**. |
| Built-binary --help checks | **25/25 passed**; empty stderr. |
| `git diff --check b4f76ca..HEAD` | **Exit 0**. |
| Two LC_ALL-unset filtered reruns below | **1 passed each**; separate from full-suite totals. |

TMPDIR was unset for the complete suite and the filtered reruns. Cargo used --offline. Git transfer fixtures used temporary repositories and local bare remotes; no operator fetch/push or ref-changing command targeted this checkout. Build completed before the full test run. Output was collected in process/tool memory without log files. Counts use each target's final top-level summary, excluding nested child-worker summaries, and include the passing strata-apfs compile-fail doctest.

### Full-suite counts

| Crate | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| git-strata | 11 | 31 | 0 |
| strata-apfs | 98 | 42 | 7 |
| strata-confirm | 15 | 1 | 2 |
| strata-daemon | 71 | 38 | 0 |
| strata-deps | 61 | 3 | 1 |
| strata-gitio | 143 | 3 | 2 |
| strata-overlay | 79 | 4 | 6 |
| strata-publish | 68 | 1 | 0 |
| strata-search | 32 | 0 | 0 |
| strata-session | 73 | 0 | 4 |
| strata-store | 133 | 0 | 3 |
| strata-types | 35 | 0 | 0 |
| **Total** | **819** | **123** | **25** |

### Failure causes and limits

| Observed cause | Failed tests | Evidence / interpretation |
| --- | ---: | --- |
| Unix socket creation/bind denied | 81 | EPERM/no_permission at fake sockets, temporary daemon sockets and special-file fixtures. Includes child-worker and Perl socket fixtures. The three newly added socket-dependent tests account for 120 → 123 relative to review 3's run; they do not reach behavioral assertions. |
| Disk-image fixtures unavailable | 25 | hdiutil create fails for APFS/case-sensitive APFS/HFS+ sparse images, generally with empty diagnostic stderr. Cross-volume/full-volume/filesystem fixtures cannot be established. |
| FSEvents service unavailable | 11 | FSEventStreamStart fails, including the independent canary and dependent daemon/overlay watcher fixtures. |
| LocalAuthentication service unavailable | 1 | la_non_interactive_round_trip_maps_codes gets Unsupported/cannot_evaluate, LA code 4099, instead of not_interactive. No authentication completed. |
| Special-mode fixture precondition | 1 | special_mode_bits_are_normalized_and_warned requires preserved 04755/02755 but observes 0755/0755 before testing normalization/restore. |
| Git diagnostic contamination | 2 | The sandbox's DARWIN_USER_TEMP_DIR confstr warning makes the missing-config oracle classify absence as Err; it also shifts the sibling-layout test's merged output lines. |
| Inherited LC_ALL | 2 | Push/staging environment assertions expect None and see C.UTF-8; both filtered cases pass with LC_ALL unset. |
| **Total** | **123** | Environment/fixture failures remain counted as failures. None is promoted into a new implementation finding or relabeled a pass. |

Socket-worker classifications were checked against fixture sources: `crates/strata-deps/tests/common/mod.rs:680` and `:696`; `crates/strata-overlay/tests/classify.rs:500`; the environment child calls the same socket-binding Live fixture (`crates/git-strata/tests/confirm_gate.rs:415`, `tests/support/live.rs:172`). The config oracle at `crates/strata-gitio/src/config.rs:373` requires empty stderr for an absent key. A read-only control, `env -u TMPDIR git config --local --get codex-review-4-no-such-key.value`, returned exit 1 with empty stdout and the same confstr warning, corroborating that classification.

The six newly added non-socket tests passed: CLI bare-refusal unit test, both publish tests, gitio push_url, deadline helper and RPC/doc inventory. The strengthened restore-settlement test also passed. Real CLI/T1, guarded Waiting and standalone Unknown/restart behavior remain execution gaps because their socket fixtures were blocked.

After the suite, the two localized checks were rerun:

    env -u TMPDIR -u LC_ALL RUSTC_WRAPPER= cargo test -p strata-gitio --test push --offline push_sets_strata_publish_and_never_forces
    env -u TMPDIR -u LC_ALL RUSTC_WRAPPER= cargo test -p strata-gitio --test staging --offline stage_applies_clean_filters

Each exited **0**, **1 passed / 0 failed / 0 ignored** (6 and 8 filtered out respectively). They do not revise the full-suite counts. No restricted test was rerun with broader permissions.

### Every failed test, by target

Each top-level failed test appears once below; causes correspond to the table above. Nested worker output is not counted as another failed test.

**git-strata / tests/cli.rs — 3 passed / 22 failed / 0 ignored**

- Unix socket denied: `binary_file_is_marked_not_rejected`, `conflict_json_is_not_windowed`, `daemon_start_stop_do_not_use_the_socket`, `daemon_up_read_does_not_open_sessions`, `deps_propose_switch_moves_only_the_proposer`, `diff_lists_content_changes_only`, `diff_omits_hunks_for_binary_and_ignores_inode`, `grep_skips_nul_and_keeps_valid_hits`, `history_is_read_from_the_store_alone`, `hook_checkpoint_is_idempotent_per_tool_call`, `hook_replay_of_a_no_op_tool_call_returns_its_own_row`, `hook_tool_call_id_comes_from_stdin_tool_use_id`, `listing_is_one_document`, `lossy_utf8_one_fffd_per_bad_sequence`, `no_pagination`, `path_order_is_repopath_ord`, `publish_conflict_fix_rerun_loop`, `publish_push_refused_keeps_commit_and_push_in_data`, `publish_push_timeout_keeps_commit_and_push_in_data`, `readonly_works_without_daemon`, `results_live_in_data_not_a_log`, `shape_is_this_doc_not_a_golden`.

**git-strata / tests/confirm_gate.rs — 1 passed / 7 failed / 0 ignored**

- Unix socket denied: `confirm_commands_refuse_non_tty`, `no_env_or_flag_bypass`, `non_tty_publish_refuses_a_guarded_repo_the_plan_left_out`, `non_tty_refused_before_prompt`, `non_tty_rerun_of_a_landed_guarded_repo_is_not_gated`, `resolve_and_chain_require_an_explicit_choice`, `superseded_proposal_is_not_auto_approved`.

**git-strata / tests/envelope.rs — 6 passed / 1 failed / 0 ignored**

- Unix socket denied: `every_command_emits_ok_envelope`.

**git-strata / tests/readonly_without_daemon.rs — 0 passed / 1 failed / 0 ignored**

- Unix socket denied: `cli_readonly_commands_work_without_daemon`.

**strata-apfs / tests/attrs.rs — 14 passed / 1 failed / 0 ignored**

- Unix socket denied: `attrs_of_reports_every_field`.

**strata-apfs / tests/case.rs — 0 passed / 2 failed / 0 ignored**

- Disk-image fixture unavailable: `case_fold_eq_matches_a_case_insensitive_volume`, `case_sensitive_volume_distinguishes_case_but_not_normalization`.

**strata-apfs / tests/clone.rs — 8 passed / 10 failed / 0 ignored**

- Unix socket denied: `clone_file_rejects_directories_and_special_files`, `clone_tree_clones_fifos_and_sockets_as_objects`.
- Disk-image fixture unavailable: `clone_dir_refuses_cross_volume`, `clone_file_refuses_cross_volume`, `clone_on_unsupported_volume_is_unsupported`, `clone_refusals_leave_no_residue`, `clone_then_small_write_allocates_only_the_touched_extent`, `clone_tree_error_precedence`, `clone_tree_leaves_a_nested_mount_as_an_empty_stub`, `clone_tree_on_a_full_volume_leaves_no_destination`.

**strata-apfs / tests/clone_from.rs — 3 passed / 3 failed / 0 ignored**

- Unix socket denied: `clone_from_rooted_checks_the_source_kind`, `clone_from_rooted_clones_files_and_directories`.
- Disk-image fixture unavailable: `clone_from_rooted_error_codes`.

**strata-apfs / tests/errno.rs — 2 passed / 1 failed / 0 ignored**

- Disk-image fixture unavailable: `classify_io_error_matches_real_syscalls`.

**strata-apfs / tests/remove_tree.rs — 4 passed / 4 failed / 1 ignored**

- Unix socket denied: `remove_tree_rooted_removes_a_protected_tree`, `remove_tree_rooted_takes_any_entry_kind`.
- Disk-image fixture unavailable: `remove_tree_rooted_refuses_before_touching_anything`, `remove_tree_rooted_reports_what_it_cannot_remove`.

**strata-apfs / tests/rooted.rs — 12 passed / 1 failed / 0 ignored**

- Unix socket denied: `rooted_open_error_codes`.

**strata-apfs / tests/rooted_remove.rs — 2 passed / 2 failed / 0 ignored**

- Unix socket denied: `remove_rooted_file_removes_one_entry_without_following`.
- Disk-image fixture unavailable: `remove_rooted_empty_dir_removes_only_empty_directories`.

**strata-apfs / tests/rooted_rename.rs — 3 passed / 4 failed / 0 ignored**

- Disk-image fixture unavailable: `full_fsync_accepts_file_and_directory_handles`, `rename_rooted_error_mapping`, `rename_rooted_excl_renames_links_and_case_only_spellings`, `rename_rooted_swap_exchanges_two_existing_entries`.

**strata-apfs / tests/rooted_write.rs — 6 passed / 1 failed / 0 ignored**

- Disk-image fixture unavailable: `clone_tree_rooted_is_atomic_and_refuses_cross_volume`.

**strata-apfs / tests/volume.rs — 0 passed / 4 failed / 0 ignored**

- Disk-image fixture unavailable: `nearest_existing_ancestor_climbs_to_the_first_existing_path`, `same_volume_follows_links_and_reports_missing_paths`, `same_volume_tells_system_volume_from_data_volume`, `volume_caps_per_filesystem`.

**strata-apfs / tests/walk.rs — 24 passed / 1 failed / 1 ignored**

- Disk-image fixture unavailable: `walk_does_not_descend_into_mount_points`.

**strata-apfs / tests/watch.rs — 0 passed / 8 failed / 2 ignored**

- FSEvents unavailable: `dropping_the_watcher_stops_callbacks`, `flush_drains_the_callback_queue`, `watcher_excludes_configured_dirs`, `watcher_paths_use_the_callers_root_and_disk_spelling`, `watcher_reentry_from_the_callback_never_traps`, `watcher_reports_a_renamed_root_as_dropped`, `watcher_reports_file_and_directory_changes`, `watcher_reports_must_rescan`.

**strata-confirm / tests/la_bridge.rs — 1 passed / 1 failed / 0 ignored**

- LocalAuthentication unavailable: `la_non_interactive_round_trip_maps_codes`.

**strata-daemon / tests/daemon.rs — 0 passed / 17 failed / 0 ignored**

- Unix socket denied: `cli_readonly_commands_work_without_daemon`, `confirmed_publish_is_bound_to_the_shown_chain_top`, `e2e_checkpoint_restore_matches_oracle`, `failure_keeps_scene_no_replay`, `lost_rpc_reply_is_not_replayed`, `publish_confirmation_cancelled_after_a_repo_landed_keeps_it_in_the_envelope`, `publish_conflict_fix_rerun_succeeds_without_duplicates`, `publish_failure_after_a_definite_push_reports_the_pushed_commit`, `publish_plan_keeps_a_waiting_sessions_landed_guarded_repo`, `publish_plan_names_the_remaining_repos_and_writes_nothing`, `publish_refuses_a_guarded_repo_outside_the_plan_before_prompting`, `publish_rerun_of_a_directory_selector_commits_new_edits`, `publish_rerun_of_a_partly_changed_batch_commits_the_changed_path`, `reconcile_loop_rescans_skips_busy_and_stops`, `rm_waits_for_publishing_in_the_lock_free_gap`, `search_after_checkpoint_sees_uncheckpointed_changes`, `standalone_push_unknown_keeps_scene_and_reports_across_restart`.

**strata-daemon / tests/deps.rs — 0 passed / 4 failed / 0 ignored**

- Unix socket denied: `approve_unknown_proposal_is_refused_without_prompt`, `propose_on_drift_awaits_publish_and_approve_refuses_it`, `propose_switch_moves_the_proposer_without_prompt`, `propose_without_drift_records_nothing`.

**strata-daemon / tests/fetch_after.rs — 4 passed / 1 failed / 0 ignored**

- FSEvents unavailable: `hint_and_flush_off_callback_thread`.

**strata-daemon / tests/resolve.rs — 6 passed / 5 failed / 0 ignored**

- Unix socket denied: `chain_resolve_refs_under_repo_lock`, `confirmed_push_is_bound_to_the_shown_chain_top`, `resolve_move_exclusive_choice`, `resolve_move_queues_and_does_not_hold_repo_lock`, `resolve_move_skips_tty_and_confirm`.

**strata-daemon / tests/rpc.rs — 31 passed / 7 failed / 0 ignored**

- Unix socket denied: `no_auth_facade_observation`, `no_auth_traced_diagnostic`, `no_branch_page_frame`, `no_duplex_read_timeout`, `no_encrypted_record`, `no_telemetry_beside_reply`, `readiness_is_socket_connect`.

**strata-daemon / tests/startup.rs — 6 passed / 4 failed / 0 ignored**

- Unix socket denied: `daemon_does_not_idle_exit`, `idle_close_is_not_ours`, `second_instance_refused_then_reacquired`, `unknown_publish_reported_on_restart_not_replayed`.

**strata-deps / tests/adopt.rs — 5 passed / 1 failed / 0 ignored**

- Unix socket denied: `adopt_matches_inputs_hash_else_refuses`.

**strata-deps / tests/ensure.rs — 5 passed / 1 failed / 0 ignored**

- Unix socket denied: `ensure_prefers_sealed_then_adopt_then_build`.

**strata-deps / tests/inputs.rs — 6 passed / 1 failed / 0 ignored**

- Unix socket denied: `inputs_hash_rejects_special_files`.

**strata-gitio / unittests src/lib.rs — 29 passed / 1 failed / 0 ignored**

- Git diagnostic contamination: `config::tests::config_values_read_like_git`.

**strata-gitio / tests/push.rs — 6 passed / 1 failed / 0 ignored**

- Inherited LC_ALL: `push_sets_strata_publish_and_never_forces`.

**strata-gitio / tests/staging.rs — 8 passed / 1 failed / 0 ignored**

- Inherited LC_ALL: `stage_applies_clean_filters`.

**strata-overlay / tests/checkpoint.rs — 17 passed / 1 failed / 0 ignored**

- FSEvents unavailable: `quiet_workspace_stays_clean`.

**strata-overlay / tests/classify.rs — 13 passed / 1 failed / 2 ignored**

- Unix socket denied: `special_files_are_never_opened`.

**strata-overlay / tests/incremental.rs — 7 passed / 1 failed / 1 ignored**

- FSEvents unavailable: `excluded_dirs_never_enter_overlay_real_watch`.

**strata-overlay / tests/restore.rs — 14 passed / 1 failed / 0 ignored**

- Special-mode fixture precondition: `special_mode_bits_are_normalized_and_warned`.

**strata-publish / tests/check.rs — 4 passed / 1 failed / 0 ignored**

- Git diagnostic contamination: `sibling_layout_matches_workspace`.

## Review output and recommendation

The only deliberate review-file edit is `docs/review-codex-4.md`; implementation, test and existing documentation files were not edited. The pre-existing untracked `.omc/` was left alone. Cargo produced its normal build artifacts and tests used their temporary fixtures. No commit, push, fetch, branch/ref change, daemon-start service action or launchctl action was executed against the workspace. `/Volumes/GWD/inter` was not accessed.

**Recommendation: REQUEST CHANGES.** Independent architectural review is unavailable. Established issues in this scope: **Critical 0 / High 0 / Medium 2 / Low 1 / Nit 0**.

**Summary:** T1/T2/R3/R5/T3/T4 are Fixed; R4 is Partially fixed. Bind scene metadata to the actual send destination and retain already_published on the new plan rejection; narrow the skill's history-change wording. Build passed with the wrapper bypass; the offline suite recorded **819 passed / 123 failed / 25 ignored**, with all failures and execution gaps classified above.
