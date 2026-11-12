---
name: rpc-change
description: Checklist for changing the git-strata daemon RPC or CLI surface. Use before adding or changing an RPC method, a request or reply field, or a CLI command or flag (crates/strata-daemon/src/rpc.rs, handlers.rs, crates/git-strata/src/args.rs, cmds.rs, help.rs). Covers old-daemon and old-CLI compatibility and the docs and fixtures that tests parse.
metadata:
  internal: true
---

# Changing the RPC or CLI surface

A newer CLI must keep working against an older daemon, and a newer daemon against an older CLI. The compatibility rules below carry that. The sites after them must all agree, and a test checks most of them.

## Compatibility rules

- **Old daemons reject, they do not ignore.** Every `*Params` struct in `crates/strata-daemon/src/rpc.rs` is `#[serde(deny_unknown_fields)]`, so a request carrying a field the daemon does not know fails with `rpc: "Failed"` before anything runs.
- **A new request field** is `#[serde(default)]` in the daemon's `Params`, so an older CLI that omits it still parses. It is announced by appending a string to `FEATURES` in `crates/strata-daemon/src/lib.rs`; that list only grows, with existing strings kept as they are. On the CLI side, choose by whether the command can still do its job against an old daemon:
  - It cannot: call `crate::agent::require_feature(&ctx.paths.socket, "<feature>")` first, which gives the user a clean `Unsupported` with `reason: "daemon_too_old"` (`mark`, `new --base`).
  - It can: probe `daemon_status.features` and drop the field with a warning (`daemon_takes_agent` for `new --agent`), or send it and fall back when the daemon refuses the unknown field (`cwd_guard::rejected`, `predates_summary`).
- **A new method** on an old daemon answers `rpc: "UnknownMethod"`, which the client reports as `InvalidInput` with `reason: "unknown_method"`. Either gate the command with a `FEATURES` string and `require_feature`, as `mark` does with `session.mark`, or map `unknown_method` to `Unsupported` / `daemon_too_old` at the call site, as `old_daemon` in `crates/git-strata/src/sync.rs` does.
- **Reply fields are additive.** Clients ignore fields they do not read, and an existing field keeps its name and JSON type.
- **Failures** are `StrataError { code, message, detail }`. Choose an existing `Code`, add a snake_case `detail.reason`, and document the pair. Clients branch on `code`, then `detail.reason`.

## Sites that must agree

Work through every site the change touches.

1. **Method table**: the `methods!` block in `rpc.rs`, one line `Variant = "wire_name", ro = <bool>;`. Use `ro = true` only for a handler that never queues and never takes a write lock. A new method also needs its `*Params` struct, a `Params` variant and an arm in `Params::parse`. Wire names are snake_case, usually the CLI command path joined with `_` (`deps_propose`, `config_reload`), and are never renamed.

2. **Handler**: the `match` over `Params` in `crates/strata-daemon/src/handlers.rs`. A session-scoped write runs inside `core.locks.with_sessions(..)` and is listed in `session_of` in the same file.

3. **`docs/modules/daemon.md`**: the Chinese sentence starting `除表里这 <N> 个` states the method count, and the `| method |` table under it lists every method in `Method::ALL` order, with `是` in the last column exactly for `ro = true`. The test `daemon_md_method_table_matches_rpc` parses both.

4. **CLI**: the clap variant in `crates/git-strata/src/args.rs` with `after_long_help = crate::help::<NAME>_AFTER`, the help text in `help.rs`, and the handler: `cmds.rs` for most commands, `confirm.rs` for those behind the confirmation gate, and `sync.rs`, `devports.rs` and `reload.rs` for their own.
   - Keep the call in the shape `ctx.call("wire_name", json!({..}), Kind::Read)` or `Kind::Write`. The test `one_method_table` finds calls by regex and requires `Kind` to match the daemon's `ro`.
   - Each command's `--help` output stays within 4096 bytes (`rm` has 8192), and `git-strata help` within 1536 (test `all_help_is_bounded_and_gate_has_one_full_output`). Help for a command behind the confirmation gate points to `git-strata help gate` instead of repeating the gate text.
   - stdout stays one JSON envelope line and text for humans goes to stderr. The exceptions are listed in `skills/git-strata/errors.md`.

5. **Desktop contract**: when a reply the desktop reads changes (`daemon_status`, `status`, `deps_list`, `du`), regenerate the fixtures with `GUI_FIXTURES=write cargo test -p git-strata --test gui_fixtures` and update `apps/desktop/src/lib/types.ts`.

6. **Tests**: daemon behaviour in `crates/strata-daemon/tests/`, including a params case in `tests/rpc.rs`; the command end to end in `crates/git-strata/tests/<command>.rs`, using `support::live::Live`, which runs an in-process daemon under a temp `HOME`.

7. **User docs**, in the same branch: `README.md` (the "Commands" table and the section describing the behaviour), `docs/modules/cli.md` and `docs/modules/daemon.md`, and the shipped skill: `skills/git-strata/reference.md` for flags, defaults and `data` shapes, `errors.md` for a new `code` and `reason` pair, `SKILL.md` when the agent's loop changes.

## Done

The done bar in `CLAUDE.md` passes (workspace tests and clippy), and every site above that the change touches is updated, user docs included.
