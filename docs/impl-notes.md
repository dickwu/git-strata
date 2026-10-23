# Implementation notes

Decisions made while implementing, where a module doc and design.md disagreed or left the choice open. design.md wins. Each ruling is `what — why — cost if wrong`.

## types

- `design-6.7` is a load-time check in `Config::validate`: a repo with non-empty `deps_dirs` must have `publish.branch == default_branch` and `pin_unpushed = true`. — design.md §6.7 (2026-10-23) requires the rejection; config.md §4.1 never numbered it. Error code `InvalidInput`, `detail.rule = "design-6.7"`, checked after V8. — Cost if wrong: a deps repo can publish off the default branch and a pending deps proposal spans two lines.
- V8 requires install, install_inputs, and deps_dirs all present or all absent. — The table's biconditional `deps_dirs 非空 ⇔ install 存在且 install_inputs 非空` would allow `install` without inputs when deps_dirs is empty; the same cell's prose forbids that. — Cost if wrong: one extra rejection, or a half-specified install that later modules treat as installable.
- `Config::parse` validates repos in file order (`toml` `preserve_order`). `Config::validate` on a constructed struct uses `BTreeMap` key order, because a struct has no file. — The "first violation" rule is about the file. — Cost if wrong: a caller who builds `Config` by hand and calls `validate` sees a different first error than `parse` would.
- The shippable inter default is `crates/strata-types/default-config.toml`, extracted from config.md appendix A and parsed by test. — config.md leaves distribution to the builder and forbids a second hand-copied text in code; `include_str!` is the distribution. The markdown appendix stays the narrative. — Cost if wrong: the file and the appendix drift, and the parse test only proves the file is valid.
- `serde_rfc3339` accepts only a `Z` suffix, not `+00:00`. — The documented example is `Z`, and a numeric offset is a different instant spelling we do not need. — Cost if wrong: a caller who emits `+00:00` cannot round-trip a `SystemTime` field.
- `ConfirmToken` fields stay public, as types.md writes them. Only `strata-confirm` and tests should construct one; the compiler does not enforce that. — Matching the written struct. — Cost if wrong: another crate can forge a token in-process. The daemon must still only accept tokens that confirm issued.
