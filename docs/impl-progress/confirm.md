# confirm (crates/strata-confirm): implementation progress

Spec: `docs/modules/confirm.md`. Branch `impl/confirm` (from impl/v1 779669c).

## §8 tasks

| Task | State | Commit |
| --- | --- | --- |
| 1. Token type guarantees and prompt text | done | 884b43e |
| 2. Mint, register and redeem single-use tokens | done | 10a58bc |
| 3. Refuse cancelled, timed-out, concurrent and unsupported prompts | done | e2699b7 |
| 4. LocalAuthentication bridge through objc2 | done | 0fefe07 |
| 5. Guards: tokens only built by confirm; no env var reaches it | done | 84086ab |
| 6. End-to-end tests in the cli crate | blocked: needs the daemon test seams (in-process daemon with `ScriptedAuth`) and the `git-strata` binary from daemon.md / cli.md | — |

`manual_prompt_round_trip` (tests/la_manual.rs) is `#[ignore]`; run it with a human at the Mac:
`cargo test -p strata-confirm --features test-seams --test la_manual -- --ignored --nocapture`.
It has not been run yet. §9's open item (prompt from the launchd agent) is still open.

The non-prompting LA tests ran on this Mac in a graphic session: `quiet()` gives
`Unsupported/not_interactive/-1004`, `invalidate_first` gives `NotConfirmed/unexpected/-10`,
both well under 1 s, also from a background thread.

## Decisions on points the spec leaves open

- **Authenticator shape.** The spec's four operations are split as `Authenticator::gui_session`
  + `Authenticator::context()`, which returns a fresh, non-`Send` `AuthContext` with `preflight`,
  `evaluate`, `invalidate`. That keeps each `LAContext` on the calling thread and makes reuse
  across prompts impossible by construction.
- **Invalidate exactly once per prompt.** A wrapper (`Ctx`) tracks it; drop also invalidates.
  `PromptLog::invalidated()` therefore counts prompts (and preflight failures), not calls.
- **`PromptLog::probes()`** counts each graphic-session check and each preflight (+1 each).
  `require` with no GUI adds 1; a `require` or `probe()` that reaches preflight adds 2.
- **Gate.** `try_lock`, taken before the graphic-session check. A busy gate gives
  `NotConfirmed/prompt_in_progress` without touching the authenticator. A poisoned gate counts as free.
- **Reply channel disconnected before any reply** (framework dropped the block) gives
  `NotConfirmed/unexpected`.
- **After the deadline**, the confirmer invalidates and waits up to `CANCEL_GRACE` on the mono
  clock. It discards whatever arrives (even success) and returns `timeout`. `classify`'s
  `we_cancelled` flag keeps the §5.4 table complete for -9 but is not reached from that path.
- **Preflight failure codes** are taken from `NSError.code()` whatever the domain
  (-5, -1004 or `cannot_evaluate` with `la_code`).
- **Error detail.** `require` errors carry `{reason, action, la_code?}` (la_code only for
  LA-domain codes) plus `attrs` (no_gui_session) or `status` (session_info). `probe()` errors
  carry the same minus `action`. `validate` errors carry only what §5.5 names:
  `{reason}`, plus `{expected, confirmed}` on mismatch and `age_ms` on expiry.
- **Mapping tests** go through `require` with scripted replies (20-row table in
  tests/require_failures.rs), because `cfg(test)` is banned in `src/`.
- **Test-seams in dev builds.** The crate's own dev-dependency on itself enables `test-seams`,
  so `cargo test -p strata-confirm` works without `--features` (same pattern as strata-apfs).
  Normal dependents never see the feature.
- **`Confirmer::new`** exists only on macOS (`la` module is `cfg(target_os = "macos")`). The
  project is macOS-only.
- **Bypass guard.** The child test is an `#[ignore]` test (`child_require_under_environment`),
  re-run with `--exact --ignored` and stdin `/dev/null`. One child checks both a cancelled
  and a passing prompt, so there are (names × 6) + 1 children. `bypass_env_names` skips
  `target` directories. "Only seams can swap the authenticator" is checked statically:
  `Confirmer::build(` appears only in `seams.rs` and `la.rs`, and `seams` is
  `#[cfg(feature = "test-seams")]`.
- **strata-types**: no changes.

## Merge watch-outs

- Root `Cargo.toml` members gained `crates/strata-confirm`. `Cargo.lock` gained objc2-*,
  block2, static_assertions and regex entries.
- `bypass_env_names` scans every crate's sources. Other modules' `STRATA_*` names (for example
  strata-apfs's `STRATA_APFS_CHILD`) are set to bypass values in the children, which is harmless.
