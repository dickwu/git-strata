# gitio implementation progress (branch impl/gitio)

Spec: docs/modules/gitio.md. Crate: crates/strata-gitio.

## Tasks

| Task | Commit | Notes |
| --- | --- | --- |
| 1 sanitized command, open | (this commit) | |

## Builder decisions (ambiguities)

- tgrep sources (`/Volumes/GWD/inter`) were off-limits to this builder, so
  "copy from tgrep" steps are fresh implementations of the spec; recorded in
  THIRD-PARTY-NOTICES.
- The crate's dev-dependencies include itself with `test-seams`, so plain
  `cargo test -p strata-gitio` / `cargo test --workspace` build the seams
  (same pattern as strata-apfs). `--features test-seams` also works.
- Git version is checked once per process (OnceLock); only a successful
  parse is cached, so a failing check is retried on the next `open`.
- `open` reads `strata.shadow` / `extensions.refStorage` with one
  `config --local --null --get-regexp`; `strata.shadow` uses git's bool
  rules (a bare key is true).
