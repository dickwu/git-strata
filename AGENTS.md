# AGENTS.md

This file provides guidance to Codex (claude.ai/code) when working with code in this repository.

git-strata is macOS-only (APFS clones, FSEvents, launchd): a daemon (`git-stratad`), a CLI (`git-strata`) and a Tauri desktop app. `README.md` describes the current code; when in doubt, `git-strata <command> --help` is the authority.

## Done bar and tests

- A change under `crates/` is done when the workspace tests pass and clippy is clean. The tests:

  ```sh
  env -u TMPDIR cargo test --workspace --no-fail-fast
  ```

  `TMPDIR` is unset because `strata-apfs` tests mount `hdiutil` disk images under the temp dir, and `hdiutil` refuses a mount point under `/Volumes`.
- Clippy, on the stable toolchain: `cargo clippy --workspace --all-targets -- -D warnings`, and once more without `--all-targets` (that build has `test-seams` off). Both are clean against the baseline in `[workspace.lints]` in the root `Cargo.toml`. Fix a new warning in the code; the allow list there only shrinks.
- The workspace excludes `apps/desktop/src-tauri` and `vendor/`, so root `cargo` commands never build or test the desktop app. Its done bar is in `apps/desktop/AGENTS.md`. A change to `strata-types` or `strata-client` runs that one too, because the app builds against both.
- One test: `cargo test -p strata-publish --test chain <test_name>`.
- Run one suite at a time: FSEvents is system-wide. Under load the watch tests print `SKIPPED <test>: fseventsd starved` on stderr. That is a skip, not a pass; rerun on a quiet machine. The `strata-deps` `open_files` tests can fail inside the full run; rerun them with `cargo test -p strata-deps --test open_files -- --test-threads=1` before treating that as a regression.
- `#[ignore]` marks tests that need a human (Touch ID), take minutes, or are children their parent test re-executes. Leave them ignored, and pass `-- --ignored` only together with one test name.
- `cargo test` turns the `test-seams` feature on by itself: the crates that have it dev-depend on themselves with it. A hook that integration tests need goes behind that feature, beside the crate's existing seams. A `git-stratad` built with seams refuses to start, by design.

## Formatting

There is no `rustfmt.toml`, and most files differ from default rustfmt. Format exactly the `.rs` files you edited, each through stdin:

```sh
f=crates/strata-store/src/marks.rs
rustfmt --edition 2024 < "$f" > "$f.fmt" && mv "$f.fmt" "$f" || rm -f "$f.fmt"
```

`cargo fmt` and `rustfmt <path>` also rewrite every out-of-line `mod` child of the file, which reformats files the change never touched. Leave `crates/strata-apfs/src/casefold_table.rs` (generated) and `vendor/` as they are.

## Standing rules

- **The live install is off-limits.** `$HOME` alone locates the config, socket and pins (no flag, no env var), and the config names the data root, so an installed or freshly built `git-strata` / `git-stratad` acts on the user's real daemon and data. Test a change only through the test suites: they use a temp `HOME`, an in-process daemon and fixtures. Never run `scripts/dev-install.sh`, `launchctl`, `git-strata daemon start|stop`, `git-strata gc` or a built `git-stratad`.
- **Wire compatibility.** A newer CLI must keep working against an older daemon, and a newer daemon against an older CLI. RPC `Params` are `deny_unknown_fields`, so an old daemon rejects a field it does not know. A new request field is therefore `#[serde(default)]` in the daemon, announced by appending a string to `FEATURES` (additive only), and handled in the CLI for a daemon that lacks it. A field the command cannot do without is refused up front with `daemon_too_old`; one it can do without is dropped, after a `daemon_status.features` check or after the daemon's unknown-field refusal. Use the `rpc-change` skill for any method, command, flag or reply-shape change.
- **Docs move with code.** A behaviour change updates `README.md`, `docs/modules/<module>.md` and `skills/git-strata/*` (the skill shipped to users' agents) in the same branch. Module `cli` is `crates/git-strata`, `gui` covers `apps/desktop` and `crates/strata-client`, `strata-types` is documented in `docs/types.md`, and every other module `<m>` is `crates/strata-<m>`.
- **Match the language around your edit.** README, the skill, release notes and `docs/impl-notes.md` are English. `docs/design.md`, `docs/types.md`, `docs/config.md` and `docs/modules/*` have Chinese bodies with English identifiers, so existing Chinese text is edited in Chinese. The sections added to those files later are English, and a new section follows them.
- **Stay in scope.** Keep the diff to the task, with shared code refactored only when the task requires it, and never kill a process you did not start.

## Architecture rules that are easy to break

- **Layering** (`docs/types.md` §8): a crate depends only on lower layers: `types` ← `apfs`, `gitio`, `store` ← `overlay`, `deps` ← `session` ← `publish`, `search` ← `daemon` ← `git-strata`. Crates on one layer do not depend on each other, except that `store` and `gitio` use `apfs`. `strata-client` depends on `types` only. Outside tests, only the daemon depends on `strata-confirm`.
- **Doc authority.** `docs/design.md` wins over the module docs; rulings made where they disagreed are in `docs/impl-notes.md`. Status lines in `docs/` lag the code (`docs/modules/gui.md` still calls the shipped app a proposal), so trust the code and README for what exists.
- **Design invariants** (`docs/design.md` §9) hold everywhere: strata is read-only on the main checkout and writes only `refs/strata/*`; bases and deps generations are never modified in place; an uncertain failure (push timeout, interrupted merge) is left exactly as it is, with no replay and no cleanup.
- **Human confirmation** is LocalAuthentication plus a TTY, and nothing bypasses it: there is no `--yes` or `--force`, and tests scan the `strata-confirm` and `strata-publish` sources for environment reads.
- **Errors** are `StrataError { code, message, detail }` with a closed `Code` enum. Callers branch on `code`, then on a snake_case `detail.reason`. `message` is for humans and may be reworded.
- **CLI output:** stdout is exactly one compact JSON line `{"ok","data","error"}` with all three keys always present, and everything for humans goes to stderr. The few exceptions (help and version output, `ls --table`, `sync --check --hook`, a started `session exec`) are listed in `skills/git-strata/errors.md`.
- **git subprocesses:** a new git call goes through `strata-gitio`, whose `base_command` scrubs `GIT_*` and pins safe options.
- **Store schema:** `BASE_DDL` and every shipped migration are frozen, because `open` compares the schema text row by row and rejects any difference as `Corrupt`. Change the schema only by appending a migration, with the `store-migration` skill.
- **Tests read docs and source text.** `docs/modules/daemon.md` (method table), `docs/config.md` (Appendix A TOML), `examples/config.toml` and `apps/desktop/src-tauri/tests/fixtures/p1/` are parsed by tests, and several tests `include_str!` source files to ban words, comments included. When one of these fails, bring the doc or fixture in line with the code; the test stays as strict as it was.
- **Generated and vendored:** `crates/strata-apfs/src/casefold_table.rs` comes from `crates/strata-apfs/scripts/gen_casefold_table.py`; `vendor/` changes only as listed in `vendor/tgrep/PATCHES.md`; `crates/strata-gitio/tests/fixtures/**` is byte-exact.

## Git

- Current topic branches are `feat/<topic>` or `fix/<topic>` (older history used `impl/<topic>`), merged into `main` with a merge commit (`git merge --no-ff`).
- Current commit subjects are `type(scope): summary` with the area as scope, such as `feat(session): …`, `fix(publish): …` or `fix(desktop): …`. A release commit is `Release X.Y.Z: …`. History carries no `Co-Authored-By` or `Signed-off-by` trailers; keep it that way.
- Releases follow `docs/release.md`. `/cut-release <version>` prepares the release commit.
