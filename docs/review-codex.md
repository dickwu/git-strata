# Codex review of git-strata impl/v1

Review date: 2026-10-23. Recommendation: **REQUEST CHANGES**.

Scope: git diff main...impl/v1 in /Volumes/GWD/opensource/git-strata. Reviewed main 5dfaeef4c2df79e9c5eceb0dff4c5e70b1cc967d against impl/v1 2d02a064f7ea611c8704e7e1d9fe3ffefa31be70. The diff contains 463 files, 162,070 insertions and 23 deletions, including vendored tgrep sources.

The review treats docs/design.md as authoritative. It also checks the shared types/config contracts, all eleven module task/test inventories, the six test-matrix files, the implementation ledgers and the agent skill. Implementation decisions, including those marked [实现时补的决定], are evaluated below rather than automatically classified as mistakes.

**Findings: Critical 0 / High 5 / Medium 8 / Low 1 / Nit 0.** These are source-backed findings, not diagnoses inferred from the sandbox test failures. Reproduction sequences below describe the code paths; new reproducer files were not created.

Independent review unavailable: the native code-reviewer and architect lanes both failed before examining files because their configured gpt-5.5 model is unsupported for this account. This is the primary lane's review; it is not an independent architectural approval or a merge-readiness certification.

## Findings, ordered by severity

### Critical

None established by this review. This is not a guarantee that every data-loss interleaving has been excluded.

### High

#### H1. Recovery choices can execute through raw RPC without a human gate

**Location:** crates/strata-daemon/src/resolve.rs:66, crates/strata-daemon/src/resolve.rs:259; dispatch at crates/strata-daemon/src/handlers.rs:405; request shapes at crates/strata-daemon/src/rpc.rs:194.

The TTY checks exist only in the CLI. The daemon directly accepts resolve_move with accept/abandon and chain_resolve with AcceptOriginDrop/AcceptOriginReplay. Those paths require neither a terminal nor LocalAuthentication. A same-uid process can send an ordinary valid JSON-line request to daemon.sock from a non-TTY and make the choice. Unknown-field rejection does not help: the unextended request is sufficient.

For example, chain_resolve with a configured repo and choice AcceptOriginDrop reaches the ref transaction that replaces the local chain and anchor with origin. A resolve_move request with exactly one boolean true reaches resolve_failed_move. No bypass flag or forged credential is necessary.

**Contract:** the requested human-gate requirement explicitly includes resolve and chain; docs/modules/cli.md §1, §3 and D18 require a human choice and a TTY. This follows the literal no-confirm decision in docs/modules/daemon.md §5.5 and docs/impl-progress/daemon.md Task 7. That intentional choice is unsafe under the requested same-uid/non-TTY threat model. For rm --discard, deps approve, guarded push/publish and config relax, raw RPC still reaches real LocalAuthentication; this finding does not claim their authentication can be forged. Their CLI-only TTY check nevertheless is not a server-enforced property.

**Fix:** enforce human authority at the daemon boundary for recovery decisions, with a one-use LocalAuthentication-backed action bound to the exact session/repo, choice and inspected recovery plan. Keep the CLI's early TTY refusal too. Update the Action/protocol documentation deliberately; a client-supplied tty/confirmed boolean is not proof. Add raw-socket non-TTY negative tests for resolve and all chain choices, alongside positive authenticated controls.

#### H2. Push confirmation is not bound to the commit range displayed to the human

**Location:** crates/git-strata/src/confirm.rs:76, crates/git-strata/src/show.rs:13, crates/strata-daemon/src/handlers.rs:857; publish confirmation at crates/strata-daemon/src/handlers.rs:815.

The CLI displays origin..chain-top, but sends only the repo in the push RPC. The daemon then reads a new chain top as its pre-prompt snapshot. Its later chain_moved comparison protects only the interval after that daemon-side read, not the interval after the CLI displayed the plan.

Concrete interleaving: a repo is configured push=false, confirm=true. Human A sees C1 in the push plan. Another permitted local publish appends C2 before the daemon reads A's request. The daemon snapshots C2, presents the generic repo/branch authentication prompt, observes C2 unchanged under the lock, and pushes both commits. A was never shown C2. The publish handler is weaker still: it obtains PushDeploy and then calls publish without retaining a pre-confirmation chain snapshot.

**Contract:** design.md §2 anti-goal and §9.3/§9.7; docs/modules/confirm.md §5.1, “A confirmation names the exact plan”; docs/modules/cli.md §5 says a chain that changes after display must be refused.

**Fix:** prepare an immutable push plan containing the repo identity, branch, origin, expected chain tip and selected release content; display and authenticate that same plan. Carry its expected values through RPC and compare them under the repo lock immediately before commit/send. Guarded publish must apply the same rule to existing unpushed commits plus its selected batches. Test a local chain advance between display and request handling, as well as during authentication.

#### H3. Publish staging omits the publishing repo's dependencies

**Location:** crates/strata-publish/src/stage.rs:732, crates/strata-publish/src/stage.rs:736; checks run at crates/strata-publish/src/push.rs:355.

The target repo is populated with export_tree, which exports tracked source only. The following cloning loop explicitly skips that repo and clones only its siblings. Nothing clones the target's sealed dependency set into the staging worktree before check runs. The pre-lock dependency preparation described in publish.md §5.2 step 1 is also absent from publish().

This prevents normal configured checks from working. For example, the default staff-api check invokes vendor/bin/phpstan, but staging contains no vendor directory. Next/TypeScript checks likewise lack the target's node_modules; bunx may attempt an external install instead of using the selected sealed set. The passing publish tests mostly use synthetic check scripts and do not require a dependency executable in the target repo.

**Contract:** design.md §4 base generations, §6.5 step 4; docs/config.md §2.3 and appendix A, including the staff-api PHPStan command; docs/modules/publish.md §5.2 step 1 and §5.5.

**Fix:** prepare the dependency generation needed for the staged installation inputs outside session queues/repo write locks, then clone the sealed dependency directories into the target staging repo before checks. Revalidate the plan after reacquiring locks; do not cold-install while holding the repo lock or modify a live session's dependencies. Add a publish test whose configured check runs an executable present only in the sealed vendor/node_modules tree and proves that staging receives a private clone.

#### H4. A later repo's conflict can follow an earlier repo's commit and deployment

**Location:** crates/strata-daemon/src/handlers.rs:812 and crates/strata-daemon/src/handlers.rs:821.

A cross-repo release group calls the complete publish operation for each repo in sorted name order. That operation merges, checks, commits, pushes and rebases before the next repo is inspected. If repo A is clean and repo B has an already-existing merge conflict, A can already be committed and pushed when B returns Conflict. published_before acknowledges the partial result but does not prevent it.

**Contract:** design.md §6.5 step 3 says a real conflict stops the entire release group. The per-repo sequencing decision in daemon.md D15 and the CLI's partial-result handling are intentional, but do not satisfy this higher-level preflight guarantee. This is not a request to roll back an already-sent production push.

**Fix:** preflight merge/conflict detection and required checks for every repo before permitting the group's first commit/push, then revalidate the prepared inputs through a documented lock/queue protocol. Establish a global ordering if multiple locks are necessary. Add a two-repo test with a clean first repo and a conflicting second repo that asserts zero commits and zero pushes anywhere in the group.

#### H5. Dependency sealing retries terminal lock errors forever during shutdown

**Location:** crates/strata-deps/src/seal.rs:396.

For AttemptKind::Build, every outer error from RepoWriteLock::with_write is treated as retryable and followed by a 10 ms sleep. The error is not checked for Code::Busy. Once daemon shutdown closes Locks, each acquisition returns ReadOnly with reason shutting_down forever. A cold build finishing at that moment never leaves this loop, retains its build lock/attempt and prevents the scoped server threads from joining.

**Contract:** deps.md §5.3 permits retrying a busy repo lock after install; daemon.md §5.9/D17 requires queued writers to fail on shutdown and the daemon to finish. The build-lock → session-queue → repo-lock order does not justify retrying a permanent shutdown refusal.

**Fix:** retry only the documented transient Busy case. Propagate terminal errors while recording/retaining the completed install scene appropriately and releasing the build lock. Add a test that closes the daemon locks while a successful install is waiting to seal and verifies bounded shutdown without replaying install.

### Medium

#### M1. The hourly reconciliation/GC scheduler is absent

**Location:** crates/strata-daemon/src/lib.rs:42, crates/strata-daemon/src/startup.rs:402, crates/strata-daemon/src/handlers.rs:224.

RECONCILE_EVERY is declared but never used. The serving loop only accepts connections or sleeps. There is no hourly iteration over workspaces, no scheduled full Tracker scan, and no scheduled GC. Trackers/watchers are created lazily on a checkpoint rather than initialized for existing tracked workspaces when listening starts.

Without a later explicit full-scan command, a silently missed event has no time-bounded repair. The overlay test named periodic_full_scan_repairs_missed_changes manually invokes compute(..., None); it does not test a running daemon scheduler.

**Contract:** design.md §6.2 and §8; daemon.md §5.1 step 13, §5.9 and I12; overlay.md O7 and §5.5.

**Fix:** wire a cancellable periodic task into the real server entry point. Use the existing session queues and repo locks, skip/report busy sessions as documented, refresh caches without inventing an hourly checkpoint reason, then run GC. Initialize existing tracked workspaces' watchers. Test scheduling through an injected clock/short interval, including a completely missed event and shutdown.

#### M2. Daemon grep/files can use an obsolete checkpoint overlay

**Location:** crates/strata-daemon/src/handlers.rs:187; consumed by crates/strata-search/src/lib.rs:233 and crates/strata-search/src/view.rs:172.

OverlaySource::last returns a cached Tracker overlay whenever its base still matches. It does not consume pending events or recompute membership. Search attaches from that overlay, so a new eligible untracked file created after a previous checkpoint is absent from both all_paths and members. A deleted file can remain in files output. Re-reading bytes of known base files cannot discover a new path.

Concrete sequence: checkpoint a clean session once, create new.txt without another checkpoint, then grep its unique contents or list files through the daemon. The cached empty overlay wins over the cold path. Session status computes a current overlay separately but does not refresh this cache.

**Contract:** design.md §6.3 and the current-workspace overlay semantics in §4; search.md §4.4/§5.1/§5.4; daemon.md §5.5 grep/files.

**Fix:** obtain a reconciled current membership snapshot before attaching/querying, with safe handling of an in-flight tracker and a full-scan fallback when completeness is unknown. Preserve read-only query behavior. Add end-to-end daemon tests for uncheckpointed creation, deletion, rename and ignore-rule changes after a tracker already exists.

#### M3. Proposer-only dependency switching is unreachable from the public CLI

**Location:** crates/git-strata/src/cmds.rs:699; command definition at crates/git-strata/src/args.rs:185.

The daemon and deps layer implement switch=true, plan_switch and an unconfirmed proposer-only move. The CLI unconditionally sends switch=false, and no public subcommand/flag offers the implemented operation. An AwaitingPublish proposal also cannot be approved through deps approve, so that is not an alternative for testing the proposed dependency change before publication.

**Contract:** design.md goal 3 and §6.4 step 3 explicitly allow the proposing session to use its new dependencies immediately; deps.md §5.12 and §8 Task 8 implement that capability. The narrower CLI table leaves a required v1 workflow inaccessible.

**Fix:** expose an explicit proposer-only switch operation on deps propose (or an equivalent documented command), using the existing origin validation and open-file checks. Do not turn it into approval for other sessions. Test through the actual CLI that the proposer moves, another session stays pinned, and an open dependency file prevents the move.

#### M4. A publish failure after commit loses the required result in data

**Location:** crates/git-strata/src/confirm.rs:497; upstream error packaging at crates/strata-daemon/src/handlers.rs:832.

If the first repo writes its commit and then its push times out/refuses, publish returns Unknown with the commit in error detail. The daemon only adds published_before for earlier fully successful repos. The CLI only calls publish_envelope on that array; otherwise it propagates an ordinary error, producing data={} and no data.commits/data.pushes for the affected repo. A failure in a later repo likewise omits that repo's known commit/outcome.

**Contract:** docs/modules/cli.md §4, §5 and §7 envelope_keys_always_present explicitly require data.pushes to survive an after-commit timeout. Error Code remains Unknown as required; the defect is loss of structured partial results. The current test constructs Envelope::err_with directly and does not exercise this actual dispatch path.

**Fix:** return a structured partial publish result from the lower layer/daemon even on error, including every known commit and uncertain push outcome; render it with err_with. Add an actual CLI → daemon → local bare-remote timeout test that checks ok=false, Unknown, and the retained data.commits/data.pushes.

#### M5. rm does not wait for the session's publishing flag

**Location:** crates/strata-daemon/src/handlers.rs:367; publishing wait helper at crates/strata-daemon/src/resolve.rs:48.

The rm handler authenticates when needed and takes the session queue, but never checks an open Running publish. Publish deliberately releases that queue during post-commit dependency preparation/rebase. In that interval rm can retire the session (or leave a failed removal scene), and the publisher subsequently fails while trying to rebase it. A landed publish becomes Unknown and can block later publications for the repo.

The progress ledger explicitly acknowledges that rm does not wait; this is an unfinished contract, not a justified change to it.

**Contract:** daemon.md §5.2, §5.6 and D14 require rm to wait for publishing while holding no queue, build lock or repo lock. Checkpoint is deliberately permitted during this gap; rm is not.

**Fix:** gate removal on the Running publication state before acquiring its queue and recheck under the queue to close the race. Release/retry without holding locks if a publication began meanwhile. Test a publish paused in its lock-free post-commit phase against both ordinary and discard removal.

#### M6. Standalone push deletes uncertain scenes and records no durable Unknown attempt

**Location:** crates/strata-daemon/src/handlers.rs:878, especially line 886; crates/strata-publish/src/push.rs:215.

The standalone push path unconditionally removes its staging directory, including on Unknown. It does not create a publication/push attempt record or write the refs audit for that operation. report_unknown only enumerates existing Unknown publish rows; it cannot report this push if the preceding local publish was Done. After a timeout or successful remote send whose local recording fails, status and a restart therefore have no durable account of the uncertain send, and its staging scene has been discarded. resolve::with_staging has the same unconditional cleanup issue.

**Contract:** design.md §9.6, publish.md §6 and the CLI skill's Unknown handling require preserving uncertain scenes and reporting them without replay.

**Fix:** persist a push intent/outcome and expose unresolved pushes in status/restart recovery. Retain staging on Unknown and report its location; clean it only after a definite result or explicit resolution. Exercise standalone push timeout, record failure and restart against local remotes, not only the combined publish path.

#### M7. chain --restore records tracking and anchor in separate transactions

**Location:** crates/strata-daemon/src/resolve.rs:238 and crates/strata-daemon/src/resolve.rs:255; crates/strata-gitio/src/push.rs:145.

Restore calls plain Git::push, which independently advances the strata remote-tracking ref and even logs/ignores an update failure. Only afterward does resolve run a transaction verifying the chain and advancing the anchor. Readers/crashes can observe one moved without the other, and Restore can report Pushed after the tracking update failed but the later anchor transaction succeeded.

A stale anchor after a crash matters: a subsequent origin rewind within the old anchor..chain range can be classified as Leads instead of Rewound, defeating the guard against reintroducing deliberately removed commits.

**Contract:** design.md §6.7 requires the tracking ref and anchor to advance together in the call that records successful push. docs/impl-progress/daemon.md intentionally chose plain push plus a transaction, following daemon.md's literal steps; that choice conflicts with the authoritative atomic recording contract. Normal publish already uses push_published for this reason.

**Fix:** use/extend the recording push primitive so the expected chain verification, tracking update and anchor update commit atomically, and return Unknown if recording fails. Add a seam between remote acceptance and recording for chain Restore, including a crash/rewind case and a tracking CAS failure.

#### M8. A no-op ToolCall is never remembered, so retries can return a different checkpoint

**Location:** crates/strata-overlay/src/checkpoint.rs:369 and crates/strata-overlay/src/checkpoint.rs:384; CLI lookup at crates/git-strata/src/cmds.rs:270.

An unchanged ToolCall returns the latest checkpoint without recording the new tool-call ID. Example: checkpoint C1 exists; tool call T1 changes nothing and returns C1; T2 then changes a file and records C2; replay T1. No stored row mentions T1, so the CLI sends it again and the no-op shortcut now returns C2. The same tool-call ID has two different results, and the returned reason can name a different call.

The UpToDate optimization in overlay.md §5.6 is intentional and tested, but its interaction with the public idempotency contract is not tested.

**Contract:** types.md §4 ToolCall and docs/modules/cli.md §5.2/§7 require a repeated (session, repo, tool-call ID) to return the same checkpoint rows, including retries without the daemon when a result already exists.

**Fix:** durably map every accepted tool-call ID to its original checkpoint result, including no-op calls, or record a distinct empty/unchanged checkpoint for each new ToolCall ID and deduplicate only repeated IDs. Add the C1 → no-op T1 → changed T2 → replay T1 sequence, including daemon-down retry.

### Low

#### L1. The skill gives the wrong publish-snapshot and manual-checkpoint guarantees

**Location:** skills/git-strata/SKILL.md:82, skills/git-strata/SKILL.md:84; also docs/modules/cli.md:279.

The skill says publish uses the newest existing checkpoint and that edits made afterward stay out. The real CLI creates a fresh BeforePublish checkpoint for every batch before sending publish (confirm.rs:476), so those later edits to a selected file are included. An agent following the skill can therefore publish file content it believed excluded. The CLI has no option selecting an older checkpoint.

The skill and CLI docs also say every -m call creates a new Manual checkpoint. Overlay deliberately coalesces unchanged Manual calls and can return an older checkpoint/message instead. The opening assertion that sessions cannot see each other's edits is also misleading given the documented --session/--all read commands.

**Contract:** the requested skill/reference-to-CLI check; design.md §6.2/§6.3/§6.5; the intentional no-op decision in overlay.md §5.6. The command/flag table in reference.md otherwise matches the built help.

**Fix:** document that CLI publish captures current selected-path content in a new BeforePublish snapshot. If publishing a previously reviewed checkpoint is a supported promise, add an explicit checkpoint selector and preserve its identity. Reconcile the Manual no-op contract across CLI docs, skill and implementation, and describe cross-session visibility accurately.

## Module §8 implementation and test-matrix audit

There are **81 task groups across the eleven module §8 sections**. Every task group has a real implementation/test footprint; this is not a skeleton workspace. That does not make every task complete: the integration gaps above are material despite “done” ledger entries.

| Module | §8 tasks | Implementation evidence and remaining gap |
| --- | ---: | --- |
| apfs | 1–10 | Clone/rooted IO/rename/remove, attributes/bulk walk, case folding and FSEvents code exist. Volume/image/watch acceptance is sandbox-blocked; budget/spike tests are ignored. |
| gitio | 1–11 | Sanitized commands, index/config/ignore parsing, private refs, export, shadow/worktree/staging, commit/push and merge implementations exist. Main-checkout invariant test passed. See M7 for misuse of the plain push API by recovery. |
| store | 1–9 | Real Turso schema/guards, blob IO, deps/base/session/move/checkpoint/publication records, transactions, crash tests and GC exist. All 133 reported store tests passed; three image-dependent tests were ignored. |
| overlay | 1–10 | Full/incremental classification, rename/LWW, checkpoint, projection/rebase and recoverable restore exist. M8 exposes an integration hole in the intentional no-op behavior. |
| deps | 1–8 | Input keys, libproc holders, build/adopt/ensure/seal, drift, proposals and rollout plans exist. H5 affects terminal lock errors; M3 leaves the proposer switch inaccessible through CLI. |
| session | 1–7 | Validation, sibling clones/seeds, identity/status, recovery, atomic moves, removal and failed-move resolution exist. All 73 executed tests passed. M5 is the missing daemon coordination around removal. |
| publish | 1–6 | Chain/anchor logic, path merging, checks, commit/push/rebase/assign/diff and concurrency code exist. H2–H4 and M4/M6/M7 prevent end-to-end acceptance. |
| search | 1–4 | Shared base index, overlays, views, snapshots and root replacement guards exist; all 32 executed tests passed. M2 is in the daemon's OverlaySource integration, outside these wrapper tests. |
| confirm | 1–6 | Token type/registry/expiry, real objc2 LA bridge, scripted refusal tests and CLI guard tests exist. The real prompt remains human-gated; the noninteractive service check failed in sandbox. H1/H2 concern caller-side authority/plan binding. |
| daemon | 1–7 | Real RPC dispatch, queues/locks, startup/recovery, pins/LA/plist, fetch alerts and command handlers exist. The unused hourly constant is not a scheduler; M1/M5 and H1/H5 remain. |
| cli | 1–3 | Clap commands, envelope IO, early TTY checks, local read path and hook stdin/id handling exist. M3/M4/M8 and L1 remain. |

The smaller apparent inventory mismatches were checked rather than counted as missing work. For example, session's named recover_abandons_pre_visible_move_intents obligation is split across several move_workspace.rs tests; config V3 tests are in strata-types. Search's matrix_* “not adopted/unused” entries and publish's “not retained” entries are dispositions, not missing executable tests.

The matrix has 2,059 source rows: 19 retained, 321 rewritten, 280 replaced, 152 delegated to dependencies and 1,287 not retained. These are semantic mappings, not a requirement for 2,059 Rust test functions. The wrapper workspace excludes vendor; cargo test --workspace does **not** execute the vendored crates' own unit-test suites. Thus the 152 dependency-assumed obligations have not all been freshly revalidated here. Workspace search integrations passed, but that is a narrower claim.

Coverage gaps exposed by the findings include: multi-repo preflight before any push, target-repo dependency availability in staging, a changed displayed push plan, raw recovery RPC human authority, real hourly scheduling, existing-tracker membership changes, public proposer switch, terminal sealing errors, removal during post-publish preparation, no-op hook replay and real after-commit CLI failure envelopes. Existing tests often cover the lower-layer primitive or a same-repo case, not the failing composition.

### Error and CLI contract audit

The actual Code enum contains the documented 39 variants; application errors use it rather than inventing a second string-code set. RPC UnknownMethod/Failed remain transport refusals and are mapped by the CLI to InvalidInput with unknown_method/rpc_failed. Envelope construction supplies ok/data/error on one compact line, checks the write and flush, and uses the documented plain-text exceptions for help/version. The after-commit result-preservation defect is M4.

The reviewed named refusal paths use the expected daemon_down, shutting_down, chain_rewound, chain_diverged and main_moved reasons. The CLI's preflight not_ancestor and daemon recovery's not_fast_forward are separate documented paths, not an accidental spelling mismatch. Some auxiliary source reasons (for example from_repo and base_manifest_missing) lack a module-document catalogue entry; this review does not assert an exhaustive closed reason vocabulary, which the docs do not define globally as they do for Code. These auxiliary entries should be added to the relevant module error tables when documenting the fixes.

## Safety, concurrency and intentional decisions

The tests inspected use throwaway HOME/config/store directories and local bare repositories. Network-looking /Volumes/GWD/inter strings in types/confirm tests are pure value/format fixtures. No reviewed test remote was a network URL. The launchd tests use a fake launchctl on PATH or a recording Launchd implementation; the real human LA round trip is ignored. No real launchd domain was loaded during review, and no system-wide install was performed.

The review did not run the application daemon, build.sh, fetch, commit, push, ref updates or repository maintenance against this checkout. Test fixture Git operations occur only in their temporary repositories/remotes. No /Volumes/GWD/inter checkout was inspected or modified. The only source-tree file written by the review is this report; cargo generated build outputs and the existing tests used temporary directories. The pre-existing untracked .omc/ directory was preserved.

The normal core paths respect the important lock direction: cold dependency builds occur outside the session queue/repo lock; rollout takes sorted session queues before the repo lock; move_workspace assumes that lock is already held instead of reacquiring it. The 14 daemon lock tests, three publish concurrency tests and store cross-process/crash tests passed. This does not cover H5 or M5.

Turso =0.9.0-pre.1 and experimental_multiprocess_wal are genuinely wired through sql::open. Store writes serialize through the process writer mutex and BEGIN IMMEDIATE, readers use separate pooled connections/read transactions, and uncertain COMMIT failures poison the writer. The cross-process engine tests passed. The documented query_only workaround, GC-gate table, normalized schema and no-recursive-trigger/REPLACE limitations are intentional engine adaptations. The store does not use REPLACE in its mutation APIs, so this review does not mislabel the documented raw-client limitation as an observed application corruption bug. The safety argument remains limited to callers using those guarded APIs.

The full-scan fallback for pathless hooks and the latest hook-ID-from-stdin change are intentional and consistent with preserving completed Bash writes. FSEvents flush is explicitly not treated as a completeness fence. Avoiding real-layout benchmarks and real LA/launchd prompts here is a validation boundary, not evidence of a stub. The unsafe intentional decisions are identified explicitly in H1, H4 and M7.

## Validation

All Cargo commands ran from the repository root with offline dependency resolution. The requested test command ran with TMPDIR removed.

| Check | Result |
| --- | --- |
| cargo build --workspace --offline, initial attempt | Exit 101: sccache denied with EPERM before compilation of strata-apfs/strata-confirm. |
| env RUSTC_WRAPPER= cargo build --workspace --offline | Exit 0; full workspace build succeeded. SDK/linker confstr/FSEvents sandbox warnings remained. |
| cargo build --workspace --offline, final exact-command check | **Exit 0**, Finished dev profile. |
| env -u TMPDIR cargo test --workspace --no-fail-fast --offline, authoritative serialized run | **Cargo exit 101: 807 passed, 102 failed, 25 ignored**, across 127 unit/integration/doc-test targets; 35 targets failed. Elapsed 309.70 s. |
| Built CLI help | Root plus 22 command/group/subcommand help calls all exited 0. Flags agree with reference.md and cli.md; behavioral differences are recorded above. No command action was executed by these --help calls. |
| git diff --check main...impl/v1 | Exit 2; 2,521 whitespace diagnostics, largely vendored CRLF/trailing whitespace plus extra EOF blank lines. No formatting changes were made. |

An earlier exact test run overlapped the wrapper-disabled normal build, which replaced a feature-gated daemon binary used by a test. Its totals are not used. The entire suite was rerun after the build with no concurrent build and those serialized results are reported here. In that run release_daemon_has_no_test_seams passed. The reporting wrapper captured Cargo's exit code separately; its own successful output collection is not a passing test result.

### Serialized counts by crate

| Crate | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| git-strata | 10 | 24 | 0 |
| strata-apfs | 98 | 42 | 7 |
| strata-confirm | 15 | 1 | 2 |
| strata-daemon | 66 | 24 | 0 |
| strata-deps | 59 | 3 | 1 |
| strata-gitio | 142 | 3 | 2 |
| strata-overlay | 79 | 4 | 6 |
| strata-publish | 65 | 1 | 0 |
| strata-search | 32 | 0 | 0 |
| strata-session | 73 | 0 | 4 |
| strata-store | 133 | 0 | 3 |
| strata-types | 35 | 0 | 0 |
| **Total** | **807** | **102** | **25** |

These counts include the one passing strata-apfs compile-fail doctest. Ignored child-helper tests can be exercised indirectly by a parent test; the ignored total is not a count of 25 unimplemented features.

### Failure classification

| Cause | Failed tests | Interpretation |
| --- | ---: | --- |
| Unix socket creation/bind denied, including fixture children | 60 | Sandbox EPERM/no_permission; tests often fail before exercising the intended behavior. |
| hdiutil sparse-image creation unavailable/denied | 25 | Cross-volume, case-sensitive/non-APFS/full-volume cases could not establish their fixtures. |
| FSEvents cannot start | 11 | FSEventStreamStart fails, including canary streams in watcher tests. |
| LocalAuthentication service unavailable | 1 | la_non_interactive_round_trip_maps_codes receives Unsupported/cannot_evaluate with la_code 4099, rather than the expected not_interactive/-1004. Authentication did not succeed. |
| Special mode bits stripped | 1 | Restore fixture expected 04755/02755 but observed 0755/0755; its precondition fails before testing restoration. |
| Git confstr diagnostic disturbs test expectations | 2 | Config oracle assumes stderr empty for a missing key; sibling-layout test merges stderr into line-numbered output. |
| Inherited LC_ALL=C.UTF-8 conflicts with test's absent-locale assertion | 2 | Test environment sensitivity, not an observed unsafe push/stage behavior. Both filtered reruns pass with LC_ALL unset. |
| **Total** | **102** | These failures are not counted as 102 product bugs. |

A direct read-only control, env -u TMPDIR git config --file /dev/null --type=bool --get core.ignorecase, returned exit 1 plus the confstr warning. This supports the config-oracle diagnosis without changing a Git config.

The following filtered offline reruns used env -u TMPDIR -u LC_ALL RUSTC_WRAPPER= before cargo test:
- -p strata-gitio --test push --offline push_sets_strata_publish_and_never_forces: **1 passed, 0 failed**, five filtered out.
- -p strata-gitio --test staging --offline stage_applies_clean_filters: **1 passed, 0 failed**, eight filtered out.

They do not replace or alter the authoritative full-run totals.

### Every failed test, grouped by target

Counts in each heading are passed / failed / ignored. The cause labels below identify the observed fixture/service/assertion failure, not an inferred product defect.

#### crates/git-strata/tests/cli.rs — 3 / 17 / 0

Cause: Unix socket bind denied: EPERM (1).

- daemon_start_stop_do_not_use_the_socket
- conflict_json_is_not_windowed
- grep_skips_nul_and_keeps_valid_hits
- daemon_up_read_does_not_open_sessions
- history_is_read_from_the_store_alone
- diff_lists_content_changes_only
- hook_checkpoint_is_idempotent_per_tool_call
- binary_file_is_marked_not_rejected
- lossy_utf8_one_fffd_per_bad_sequence
- hook_tool_call_id_comes_from_stdin_tool_use_id
- diff_omits_hunks_for_binary_and_ignores_inode
- listing_is_one_document
- no_pagination
- shape_is_this_doc_not_a_golden
- results_live_in_data_not_a_log
- path_order_is_repopath_ord
- readonly_works_without_daemon

#### crates/git-strata/tests/confirm_gate.rs — 1 / 5 / 0

Cause: Unix socket bind denied: EPERM (1).

- resolve_and_chain_require_an_explicit_choice
- confirm_commands_refuse_non_tty
- superseded_proposal_is_not_auto_approved
- non_tty_refused_before_prompt

Cause: Fixture child cannot bind a Unix socket.

- no_env_or_flag_bypass

#### crates/git-strata/tests/envelope.rs — 6 / 1 / 0

Cause: Unix socket bind denied: EPERM (1).

- every_command_emits_ok_envelope

#### crates/git-strata/tests/readonly_without_daemon.rs — 0 / 1 / 0

Cause: Unix socket bind denied: EPERM (1).

- cli_readonly_commands_work_without_daemon

#### crates/strata-apfs/tests/attrs.rs — 14 / 1 / 0

Cause: Unix socket bind denied: EPERM (1).

- attrs_of_reports_every_field

#### crates/strata-apfs/tests/case.rs — 0 / 2 / 0

Cause: Sandbox hdiutil image creation denied/unavailable.

- case_sensitive_volume_distinguishes_case_but_not_normalization
- case_fold_eq_matches_a_case_insensitive_volume

#### crates/strata-apfs/tests/clone.rs — 8 / 10 / 0

Cause: Unix socket bind denied: EPERM (1).

- clone_file_rejects_directories_and_special_files
- clone_tree_clones_fifos_and_sockets_as_objects

Cause: Sandbox hdiutil image creation denied/unavailable.

- clone_then_small_write_allocates_only_the_touched_extent
- clone_refusals_leave_no_residue
- clone_file_refuses_cross_volume
- clone_dir_refuses_cross_volume
- clone_on_unsupported_volume_is_unsupported
- clone_tree_on_a_full_volume_leaves_no_destination
- clone_tree_error_precedence
- clone_tree_leaves_a_nested_mount_as_an_empty_stub

#### crates/strata-apfs/tests/clone_from.rs — 3 / 3 / 0

Cause: Unix socket bind denied: EPERM (1).

- clone_from_rooted_checks_the_source_kind
- clone_from_rooted_clones_files_and_directories

Cause: Sandbox hdiutil image creation denied/unavailable.

- clone_from_rooted_error_codes

#### crates/strata-apfs/tests/errno.rs — 2 / 1 / 0

Cause: Sandbox hdiutil image creation denied/unavailable.

- classify_io_error_matches_real_syscalls

#### crates/strata-apfs/tests/remove_tree.rs — 4 / 4 / 1

Cause: Unix socket bind denied: EPERM (1).

- remove_tree_rooted_removes_a_protected_tree
- remove_tree_rooted_takes_any_entry_kind

Cause: Sandbox hdiutil image creation denied/unavailable.

- remove_tree_rooted_reports_what_it_cannot_remove
- remove_tree_rooted_refuses_before_touching_anything

#### crates/strata-apfs/tests/rooted.rs — 12 / 1 / 0

Cause: Unix socket bind denied: EPERM (1).

- rooted_open_error_codes

#### crates/strata-apfs/tests/rooted_remove.rs — 2 / 2 / 0

Cause: Unix socket bind denied: EPERM (1).

- remove_rooted_file_removes_one_entry_without_following

Cause: Sandbox hdiutil image creation denied/unavailable.

- remove_rooted_empty_dir_removes_only_empty_directories

#### crates/strata-apfs/tests/rooted_rename.rs — 3 / 4 / 0

Cause: Sandbox hdiutil image creation denied/unavailable.

- rename_rooted_excl_renames_links_and_case_only_spellings
- rename_rooted_error_mapping
- full_fsync_accepts_file_and_directory_handles
- rename_rooted_swap_exchanges_two_existing_entries

#### crates/strata-apfs/tests/rooted_write.rs — 6 / 1 / 0

Cause: Sandbox hdiutil image creation denied/unavailable.

- clone_tree_rooted_is_atomic_and_refuses_cross_volume

#### crates/strata-apfs/tests/volume.rs — 0 / 4 / 0

Cause: Sandbox hdiutil image creation denied/unavailable.

- volume_caps_per_filesystem
- same_volume_follows_links_and_reports_missing_paths
- same_volume_tells_system_volume_from_data_volume
- nearest_existing_ancestor_climbs_to_the_first_existing_path

#### crates/strata-apfs/tests/walk.rs — 24 / 1 / 1

Cause: Sandbox hdiutil image creation denied/unavailable.

- walk_does_not_descend_into_mount_points

#### crates/strata-apfs/tests/watch.rs — 0 / 8 / 2

Cause: Sandbox cannot start an FSEvents stream.

- flush_drains_the_callback_queue
- watcher_reports_file_and_directory_changes
- watcher_paths_use_the_callers_root_and_disk_spelling
- watcher_reports_must_rescan
- watcher_excludes_configured_dirs
- dropping_the_watcher_stops_callbacks
- watcher_reports_a_renamed_root_as_dropped
- watcher_reentry_from_the_callback_never_traps

#### crates/strata-confirm/tests/la_bridge.rs — 1 / 1 / 0

Cause: LocalAuthentication service unavailable: cannot_evaluate, la_code=4099.

- la_non_interactive_round_trip_maps_codes

#### crates/strata-daemon/tests/daemon.rs — 0 / 4 / 0

Cause: Unix socket bind denied: EPERM (1).

- e2e_checkpoint_restore_matches_oracle
- lost_rpc_reply_is_not_replayed
- failure_keeps_scene_no_replay
- cli_readonly_commands_work_without_daemon

#### crates/strata-daemon/tests/deps.rs — 0 / 4 / 0

Cause: Unix socket bind denied: EPERM (1).

- propose_on_drift_awaits_publish_and_approve_refuses_it
- approve_unknown_proposal_is_refused_without_prompt
- propose_switch_moves_the_proposer_without_prompt
- propose_without_drift_records_nothing

#### crates/strata-daemon/tests/fetch_after.rs — 4 / 1 / 0

Cause: Sandbox cannot start an FSEvents stream.

- hint_and_flush_off_callback_thread

#### crates/strata-daemon/tests/resolve.rs — 4 / 4 / 0

Cause: Unix socket bind denied: EPERM (1).

- resolve_move_skips_tty_and_confirm
- resolve_move_exclusive_choice
- resolve_move_queues_and_does_not_hold_repo_lock
- chain_resolve_refs_under_repo_lock

#### crates/strata-daemon/tests/rpc.rs — 30 / 7 / 0

Cause: Unix socket bind denied: EPERM (1).

- no_encrypted_record
- no_auth_facade_observation
- no_auth_traced_diagnostic
- no_branch_page_frame
- no_duplex_read_timeout
- no_telemetry_beside_reply
- readiness_is_socket_connect

#### crates/strata-daemon/tests/startup.rs — 6 / 4 / 0

Cause: Unix socket bind denied: EPERM (1).

- second_instance_refused_then_reacquired
- idle_close_is_not_ours
- daemon_does_not_idle_exit
- unknown_publish_reported_on_restart_not_replayed

#### crates/strata-deps/tests/adopt.rs — 5 / 1 / 0

Cause: Fixture child cannot bind a Unix socket (parent sees st.success assertion).

- adopt_matches_inputs_hash_else_refuses

#### crates/strata-deps/tests/ensure.rs — 5 / 1 / 0

Cause: Fixture child cannot bind a Unix socket (parent sees st.success assertion).

- ensure_prefers_sealed_then_adopt_then_build

#### crates/strata-deps/tests/inputs.rs — 6 / 1 / 0

Cause: Unix socket bind denied: EPERM (1).

- inputs_hash_rejects_special_files

#### crates/strata-gitio/unittests src/lib.rs — 29 / 1 / 0

Cause: Git confstr warning breaks the test oracle's stderr-is-empty assumption.

- config::tests::config_values_read_like_git

#### crates/strata-gitio/tests/push.rs — 5 / 1 / 0

Cause: Locale-sensitive test assertion (LC_ALL=C.UTF-8).

- push_sets_strata_publish_and_never_forces

#### crates/strata-gitio/tests/staging.rs — 8 / 1 / 0

Cause: Locale-sensitive test assertion (LC_ALL=C.UTF-8).

- stage_applies_clean_filters

#### crates/strata-overlay/tests/checkpoint.rs — 17 / 1 / 0

Cause: Sandbox cannot start an FSEvents stream.

- quiet_workspace_stays_clean

#### crates/strata-overlay/tests/classify.rs — 13 / 1 / 2

Cause: Fixture cannot create its socket.

- special_files_are_never_opened

#### crates/strata-overlay/tests/incremental.rs — 7 / 1 / 1

Cause: Sandbox cannot start an FSEvents stream.

- excluded_dirs_never_enter_overlay_real_watch

#### crates/strata-overlay/tests/restore.rs — 14 / 1 / 0

Cause: Sandbox strips setuid/setgid fixture bits.

- special_mode_bits_are_normalized_and_warned

#### crates/strata-publish/tests/check.rs — 3 / 1 / 0

Cause: Git confstr warning shifts the test's merged-output line positions.

- sibling_layout_matches_workspace

### Ignored tests

No ignored-only human prompt, disk-image or benchmark suite was enabled.

- crates/strata-apfs/tests/budget.rs: clone_node_modules_sized_tree_within_the_budget ... ignored, budget: builds a 100,000-entry tree; spike_primitive_latencies ... ignored, spike: prints numbers only; walk_tool_sized_tree_within_the_reconcile_budget ... ignored, budget: builds a 30,000-entry tree.
- crates/strata-apfs/tests/remove_tree.rs: remove_tree_low_descriptor_limit_child ... ignored, child of remove_tree_fits_a_low_descriptor_limit.
- crates/strata-apfs/tests/walk.rs: low_descriptor_limit_child ... ignored, child of walk_fits_a_low_descriptor_limit.
- crates/strata-apfs/tests/watch.rs: flush_is_not_a_fence ... ignored, measurement only: prints how often flush already covered a just-written file; watcher_reentry_child ... ignored, child process of watcher_reentry_from_the_callback_never_traps.
- crates/strata-confirm/tests/guards.rs: child_require_under_environment ... ignored, child of confirm_reads_no_environment.
- crates/strata-confirm/tests/la_manual.rs: manual_prompt_round_trip ... ignored, shows real Touch ID / password prompts; needs a human.
- crates/strata-deps/tests/build.rs: build_preflight_cross_volume ... ignored.
- crates/strata-gitio/tests/export.rs: export_large_tree_is_complete ... ignored, slow: 100k files; run before release.
- crates/strata-gitio/tests/merge.rs: merge3_fuzz_against_git_merge_file ... ignored, slow: spawns git merge-file a few hundred times.
- crates/strata-overlay/tests/classify.rs: excluded_paths_follow_disk_identity_case_sensitive ... ignored, needs hdiutil (a case-sensitive disk image); size_threshold_never_affects_membership ... ignored, needs about 130 MB of disk.
- crates/strata-overlay/tests/incremental.rs: event_spellings_and_subtrees_reverify_case_sensitive ... ignored, needs hdiutil (a case-sensitive disk image).
- crates/strata-overlay/tests/rename.rs: rename_case_only_recorded_as_rename_case_sensitive ... ignored, needs hdiutil (a case-sensitive disk image).
- crates/strata-overlay/tests/restore_safety.rs: random_edits_incremental_equals_cold_and_restores_exact_long ... ignored, long: 2048 cases; restore_child_helper ... ignored, helper: run only as a child process.
- crates/strata-session/tests/move_workspace.rs: move_workspace_seconds_warm ... ignored, timing on a full-size tree (§7 move_workspace_seconds_warm).
- crates/strata-session/tests/new_layout.rs: new_warm_under_5s ... ignored, timing target (design §8): measured for the user's decision; run by hand.
- crates/strata-session/tests/new_refusals.rs: new_refuses_cross_volume_root ... ignored, needs hdiutil attach (a disk image), which needs a human-approved environment; new_refuses_non_apfs_root ... ignored, needs hdiutil attach (a disk image), which needs a human-approved environment.
- crates/strata-store/tests/crash.rs: disk_full_during_checkpoint_keeps_previous_state ... ignored, needs an hdiutil disk image.
- crates/strata-store/tests/open.rs: open_refuses_cross_volume_root ... ignored, needs an hdiutil disk image; open_refuses_volume_without_clonefile ... ignored, needs an hdiutil disk image.

## Remaining acceptance limits

The sandbox prevents fresh confirmation of the real socket-based end-to-end guard tests, FSEvents behavior, disk-image/filesystem variants and real human authentication. macOS 26 remains untested. The real full-size layout, checkpoint/reconcile/search latency budgets and real GUI LaunchAgent prompt are not established by this run. The default suite deliberately ignores several such acceptance tests. This review did not exhaustively audit every line of the vendored tgrep implementation. No unsupported permission escalation or launchd installation was attempted to turn those gaps into passes.

The implementation has substantial working lower layers, especially store, session, search wrappers and type/config handling. The five High findings and the eight Medium integration/contract gaps prevent acceptance of impl/v1 as implementing the documented v1 workflow.

Short summary: **request changes; build passes; offline tests 807 passed / 102 failed / 25 ignored, with sandbox/environment causes listed above.**

