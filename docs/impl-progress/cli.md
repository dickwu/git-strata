# git-strata (cli) implementation progress

Branch `impl/cli` (worktree `git-strata-wt/cli`), from `impl/v1` 0c59dfb.
Not merged.

## Tasks (cli.md §8)

| Task | Status | Commit |
|---|---|---|
| pre: daemon `Busy daemon_down` (cli.md D2) | done | f19e18f |
| pre: daemon serves `deps_propose` / `deps_approve` | done | 4bee168 |
| 1 JSON envelope and write failures | done | (this commit) |

## Ambiguity decisions

- **Key order.** The envelope line is built by hand as
  `{"ok":…,"data":…,"error":…}` (serde_json's map sorts keys without the
  `preserve_order` feature). `data` and `error` are serde_json's compact
  rendering; a `null` result becomes `{}`.
- **Write failures.** `emit` writes with `write_fmt` and then flushes
  explicitly; either failing is exit 1 plus `git-strata:
  stdout_write_failed: …` on stderr. Rust's stdout treats `EBADF` as a
  successful write, so the "format write" binary test uses a regular file
  under `RLIMIT_FSIZE = 0` (`EFBIG`, `SIGXFSZ` ignored) instead of a
  read-only fd; the flush failure is tested at the library level (a writer
  whose `flush` fails), since the real stdout cannot fail only on flush.
- **`every_command_emits_ok_envelope` in Task 1** covers the library and
  `daemon status` (the only command that exists in Task 1); Task 3 extends
  it to every subcommand.
- **Config unreadable** (not missing): `InvalidInput`, reason
  `config_unreadable`. Missing: `NoSuchPath`, reason `no_config`.
  types.md has no I/O code.
