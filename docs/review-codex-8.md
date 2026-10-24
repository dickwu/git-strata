**Fixed — review-7 Medium.** Reviewed only `8d828fd` on `impl/v1`.

The push sets `GIT_CONFIG_COUNT=1` with `core.hooksPath` pointing to the guard (`crates/strata-gitio/src/push.rs:206`). Apple Git 2.54.0 probes confirmed command scope wins over repository-local, global, and system values. The push has no competing `-c core.hooksPath`; its `tree_command` uses `index=None` (`push.rs:196`; `crates/strata-gitio/src/command.rs:237`). Inherited `GIT_*` variables are stripped (`command.rs:75`); the extra-environment injection API is test-only (`crates/strata-gitio/src/seams.rs:180`). No other production `GIT_CONFIG_COUNT` source was found.

D25 still holds for the guarded push: the guard records and compares Git's resolved `$2` before running the human hook (`push.rs:436`). Unsetting the hook child's variables cannot alter the parent Git process or its resolved transport. The single-mismatch `InvalidInput/url-rewritten` envelope and mixed/missing-guard `Unknown` handling remain unchanged (`push.rs:245`). Ref advertisements may still be read from a rejected destination before the hook; objects/ref updates are blocked.

Before `exec`, the guard removes the three config variables and its two control variables (`push.rs:444`). A runtime comparison confirmed `GIT_CONFIG_PARAMETERS` retains only `core.fsmonitor=false` and the two URL pins, matching pre-D25. Literal environment equality has a pre-existing caveat: for a Python human hook, the shell wrapper normalizes inherited `PWD` to staging (`push.rs:434`). The parent commit does the same; this is not a regression from `8d828fd`.

**Commit-caused findings: none.** The changed mechanism/test descriptions agree with the code (`docs/modules/gitio.md:694`, `:919`, `:1294`; `docs/modules/daemon.md:872`), subject to the existing literal-environment caveat above. No new error-envelope regression was established.

**Tests:** all Cargo runs used `--offline`, `CARGO_NET_OFFLINE=true`, an external target directory, and disposable local Git remotes.

| Check | Result |
| --- | --- |
| `cargo test --offline -p strata-gitio --features test-seams --test push` | Exit 0; **12 passed**. |
| `cargo test --offline -p strata-daemon --features test-seams --test resolve -- chain_resolve_restore` | Exit 0; **7 passed**, 7 filtered out. |
| New nested-hook test, parent `push.rs` in scratch with the new test retained | Exit 101; fails at `tests/push.rs:451`, `Unknown` instead of `Pushed`; log contains guard hooksPath. |
| Same scratch, `8d828fd` implementation restored | Exit 0; **1 passed**, 11 filtered out. |

Scratch source, probes, and targets were removed. Only this report was added to the checkout; the pre-existing untracked `.omc/` was left alone.

**Independent review unavailable:** both required role agents failed with unsupported model `gpt-5.5`. The [code-review skill](/Users/gwddeveloper/.codex/skills/code-review/SKILL.md) says “do not approve or mark the review merge-ready” when a lane cannot launch. Approval therefore requires rerunning the reviewer and architect lanes with supported role models.

Recommendation: REQUEST CHANGES — independent-review gate only; the scoped technical fix passes.
