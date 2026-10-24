**Hole 1: Fixed — standalone push and restore.** `crates/strata-gitio/src/push.rs:122` rejects either legacy file with `InvalidInput/url-is-remote-name` before spawning the push; `push_url_exact` uses the same check (`:84`). `lstat` also catches dangling symlinks and nonregular entries conservatively. The regression test covers both `remotes/` and `branches/` (`crates/strata-gitio/tests/push.rs:346`). Files introduced after this check are covered by the destination guard. No remaining legacy-file bypass was established.

**Hole 2: Fixed — standalone push and restore.** The parent installs its guard before spawning Git (`crates/strata-gitio/src/push.rs:192`); the hook checks Git's actual transport URL before chaining to the human hook (`:431`). Both callers share this path: standalone through `crates/strata-publish/src/push.rs:227`, restore through `crates/strata-daemon/src/resolve.rs:500`. The restore regression also verifies that the retained A scene stays (`crates/strata-daemon/tests/resolve.rs:658`).

Local probes against **Apple Git 2.54.0**, with the exact guard script and staging layout, confirmed:

- `$2` is the post-rewrite URL. An exact A→B rewrite yields B; refusal leaves B without the new commit and A unchanged. Git connects/reads the advertisement before this hook, so “nothing sent” means no object/ref update, not no contact.
- Changing the rewrite config inside the chained human hook does not redirect the already-resolved transport.
- Multiple push URLs invoke the guard separately. With a matching literal destination followed by B, the first destination can update, B is blocked, and `seen` contains both. The code correctly returns `Unknown` (`push.rs:241`), preserving uncertainty rather than settling scenes.
- Up-to-date pushes still run the guard and produce `=` with empty hook stdin. Missing/non-executable human hooks are skipped successfully. Relative `core.hooksPath` resolves against staging (`push.rs:269`). The human hook receives identical arguments, stdin bytes, cwd, `GIT_DIR`, `GIT_WORK_TREE`, and `STRATA_PUBLISH`; its command-config environment differs as below.

No wrong-destination send or false successful settlement was established for the scoped rewrite/remote-definition cases.

**Medium — the guard changes Git behavior inside the human hook.** `crates/strata-gitio/src/push.rs:203` adds `core.hooksPath=<guard>/hooks` to inherited `GIT_CONFIG_PARAMETERS`; `:438` execs the human hook without restoring the prior command-config environment. Reproduced: a human pre-push running `git hook run pre-commit` succeeds and runs its policy hook without the guard, but fails with the guard because the temporary directory has no `pre-commit`. Other nested Git operations likewise resolve their hooks in the temporary directory. `docs/modules/gitio.md:694` acknowledges the override but also claims the environment is unchanged; that claim is false. **Fix:** restore the pre-override command-config environment, retaining the existing pins/settings, before executing the human hook, while leaving the parent push guarded. Add a nested-hook regression and correct the environment claim.

**Tests — all offline; targets and scratch source outside the checkout.** The requested commands used `env -u TMPDIR -u LC_ALL RUSTC_WRAPPER= CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=<scratch target>`.

| Check | Result |
| --- | --- |
| `cargo test --offline -p strata-gitio --features test-seams --test push` | Exit 0: **11 passed**. |
| `cargo test --offline -p strata-daemon --features test-seams --test resolve -- chain_resolve_restore` | Exit 0: **7 passed**, 7 filtered out. |
| `push_to_refuses_a_bare_name_with_a_legacy_remote_file`, protections removed in scratch | Exit 101: fails `push_url_exact` assertion at `tests/push.rs:365`. |
| `push_to_rule_added_between_check_and_push_is_refused`, protections removed in scratch | Exit 101: gets `Ok(Pushed)` instead of an error at `tests/push.rs:408`. |
| `chain_resolve_restore_rule_added_between_check_and_push_is_refused`, protections removed in scratch | Exit 101: B receives C and restore returns successful `ChainResolved`, failing `tests/resolve.rs:658`. |

The scratch negative controls removed legacy-name detection and guard enforcement only; tests and pause seams stayed unchanged. All three tests pass in the intact suites above. No full workspace run was performed.

No additional cleanup/error-envelope regression was established: single mismatch without status becomes `InvalidInput/url-rewritten`; mixed destinations or unguarded success become `Unknown` (`push.rs:240`). `Guard` drops on normal/error/timeout returns (`:207`, `:478`); the new test verifies no residual guard directory after refusal (`tests/push.rs:417`). Cleanup failures are logged, so removal is best effort. `bb25714` preserves cleanup behavior. Independent code-reviewer/architect lanes were unavailable because their configured model was unsupported; the findings above use source inspection and runtime probes. Scratch files were removed; only this report changes in the checkout.

Recommendation: REQUEST CHANGES
