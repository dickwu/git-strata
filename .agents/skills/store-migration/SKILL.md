---
name: store-migration
description: Store schema migration for git-strata. Use before adding or altering a table, column, index, trigger or CHECK in crates/strata-store, or editing schema.rs. Shipped DDL is frozen, and tests, docs and the shipped skill hard-code the schema version.
metadata:
  internal: true
---

# Store schema migration

The store schema is append-only. `Store::open` rebuilds the expected schema in memory and compares `sqlite_schema` row by row, so any textual difference in an existing store opens as `Corrupt` with `reason: "schema"`. Shipped DDL text therefore never changes, and every schema change is one new migration.

A bump is irreversible for users. The daemon migrates the store on its first start and keeps a backup. Every binary refuses a store whose `user_version` is newer than its own, and the CLI opens the store itself, so the previous release stops working and CLI and daemon upgrade together. Keep the change minimal, and put changes that ship together into one migration.

## Steps

1. **Append one `Migration` to `MIGRATIONS`** in `crates/strata-store/src/schema.rs`: `to` is the current `LATEST` plus 1, `name` is a short label, and `sql` ends with `PRAGMA user_version = <to>;`.
   - `BASE_DDL` and the earlier migrations stay byte-identical, whitespace included.
   - The engine is Turso (pinned in `crates/strata-store/Cargo.toml`), not stock SQLite. Stay with SQL forms the file already uses; its header comment lists forms Turso rejects.
   - Model a new table on its nearest neighbour: `STRICT`, with the same style of insert, update and delete guard triggers.

2. **Give a session-scoped table to the store gc.** A table with a foreign key to `sessions` joins the `TABLES` list in `delete_sessions` in `crates/strata-store/src/gc.rs`, placed where the delete order stays FK-safe.

3. **Move the hard-coded version numbers.** They are literals, so `cargo test -p strata-store` stays red until each one is moved. Search `crates/strata-store/tests/` and the unit test in `schema.rs` for the old `LATEST` and for `LATEST + 1`. They come in four kinds:
   - The version a migration ends at: `LATEST` and the `pending(n).count()` values that grow in the `schema.rs` unit test `migrations_are_contiguous_and_set_their_version`, and the expectations in `migrate.rs`, `migrate_v3.rs`, `migrate_v4.rs`, `migrate_v6.rs` and `migrate_v7.rs`.
   - The "newer store is refused" cases in `migrate.rs`, `migrate_v3.rs` and `open.rs`, which set `LATEST + 1` and match the message `older than the store (v<LATEST + 1>)`.
   - Both `NEW_TABLES` lists, in `migrate.rs` and `migrate_v3.rs`, when the migration adds a table.
   - Test names that carry the number, such as `v3_to_v7_migration_keeps_rows` and `crash_right_after_commit_keeps_v7`.

4. **Add `crates/strata-store/tests/migrate_v<N>.rs`.** It proves that a populated store of the previous version migrates, that rows which existed before are unchanged afterwards, and that the migrated schema equals a fresh store's. Obtain the previous-version store in one of two ways:
   - Build a current store with `support::env()` and `support::world`, then strip the new addition and set `PRAGMA user_version` back, as `migrate_v7.rs` does. Stripping yields an exact older schema only while that migration is the newest. The previous `migrate_v<N-1>.rs` therefore has to strip your addition as well, or move to a fixture.
   - Commit a fixture `tests/fixtures/store-v<N-1>.db` produced by the previous commit's code, and record how in `tests/fixtures/README.md`, as `migrate_v6.rs` does with `store-v5.db`.

   The earlier migrations are never rewritten to produce a fixture.

5. **Carry the new data upward in layer order:** types in `strata-types`, the store API and its tests in `crates/strata-store`, then session, daemon and CLI. An RPC method, request field or reply field goes through the `rpc-change` skill.

6. **Update what names the schema version.**
   - `docs/modules/store.md`: every statement of the current number (search the file for `LATEST` and `user_version`), plus a new section for this version beside the existing per-version ones, in English like them. The DDL block in §4.3 is version 1 and stays as it is.
   - `README.md`: the "Downgrade warning" paragraph.
   - `skills/git-strata/SKILL.md`: the `compatibility:` frontmatter line.
   - Release notes: the next `docs/release-notes/v<version>.md` opens with an `## Upgrade first: …` section modelled on `v0.2.1.md`. If that file does not exist yet, tell the user the release needs this section.

## Done

The done bar in `AGENTS.md` passes (workspace tests and clippy), and each of the four places in step 6 names the new version.
