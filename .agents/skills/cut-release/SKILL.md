---
name: cut-release
description: Prepare a git-strata release commit (version bump, lock files, release notes), then hand the tag and push to the maintainer.
argument-hint: "[version, e.g. 0.2.2]"
disable-model-invocation: true
metadata:
  internal: true
---

Prepare the release commit for git-strata `$ARGUMENTS`.

`docs/release.md` is the source of truth for the whole release. This skill does its step 1 plus the release notes, and stops before anything leaves the machine: tagging, pushing and publishing stay with the maintainer.

Before step 1, check all four and stop with a question if any fails: `$ARGUMENTS` is a version `X.Y.Z` higher than the current one in `crates/git-strata/Cargo.toml`; `git tag -l "v$ARGUMENTS"` prints nothing; the branch is `main`; `git status --short` is empty.

## Steps

1. **Collect what ships.** Take the previous tag from `git describe --tags --abbrev=0` and read `git log --oneline <previous>..HEAD`. Note each store migration added to `MIGRATIONS` in `crates/strata-store/src/schema.rs`, each new `FEATURES` string, config key and CLI command or flag. Done when every commit since the previous tag is classed as user-visible or internal.

2. **Bump the version in exactly these 16 files.** Edit the `version` field itself in each one. A search-and-replace of the old version corrupts other lines: `libc = "0.2.191"` contains `0.2.1`.
   - line 3 of each of the 13 `crates/*/Cargo.toml`
   - `apps/desktop/src-tauri/Cargo.toml`
   - `apps/desktop/src-tauri/tauri.conf.json`
   - `apps/desktop/package.json`

   `release.yml` compares only `git-strata`, `strata-daemon` and `tauri.conf.json` with the tag, so a miss in the other 13 ships silently. Done when `git diff --stat` lists those 16 files with one changed line each.

3. **Refresh both lock files.** The core release build and the desktop tests run with `--locked`.

   ```sh
   cargo update --workspace
   cargo update --workspace --manifest-path apps/desktop/src-tauri/Cargo.toml
   ```

   Done when both of these exit 0 and the two lock diffs change only this repo's own packages (the 13 crates in `Cargo.lock`; `git-strata-desktop`, `strata-client` and `strata-types` in the desktop lock):

   ```sh
   cargo metadata --locked --format-version 1 > /dev/null
   cargo metadata --locked --format-version 1 --manifest-path apps/desktop/src-tauri/Cargo.toml > /dev/null
   ```

   A third-party package in either diff means something else was upgraded: restore that lock file and find out why before going on.

4. **Write `docs/release-notes/v$ARGUMENTS.md`.** Read the newest `docs/release-notes/v*.md` first and match its shape:
   - Title `# git-strata X.Y.Z — <theme>`.
   - If step 1 found a store migration, the notes open with an `## Upgrade first: …` section, as every earlier notes file with a migration does. Model it on `v0.2.1.md`: the daemon writes a verified backup under `store/backups/`, the previous release cannot open the new store, CLI, daemon and hook callers upgrade together, and how to downgrade.
   - One `##` section per user-visible change from step 1, written for the person or agent running the CLI: commands, flags, config keys, reply fields.

   Done when every user-visible item from step 1 appears in the notes.

5. **Update the docs that name versions**, when step 1 found a store migration: the `compatibility:` line in the frontmatter of `skills/git-strata/SKILL.md`, and the "Downgrade warning" paragraph in `README.md`. Done when neither still names the old schema version as current.

6. **Run the done bars** from `AGENTS.md` and `apps/desktop/AGENTS.md`, then the test marked "run before release":

   ```sh
   cargo test -p strata-gitio --test export export_large_tree_is_complete -- --ignored
   ```

   If a known flake needed a rerun, say so in a `## Validation note` section at the end of the notes, as `v0.2.1.md` does. A real failure stops the release: report it and leave the tree uncommitted.

7. **Show, then commit.** Show the user `git status --short` and the notes file. After they approve, commit on `main` with the subject in the shape history uses:

   ```
   Release X.Y.Z: bump crate/app versions; release notes (<store vA->vB, then the headline changes>)
   ```

   The commit holds the 16 version files, both lock files, the notes, and any step 5 edits.

8. **Hand over.** Tell the maintainer what remains, with the version filled in, and run none of it: push `main`, then steps 2 to 5 of `docs/release.md` (the optional dry run, the tag below, the notes onto the draft release and publishing it, the Homebrew check).

   ```sh
   git tag -a vX.Y.Z -m "git-strata X.Y.Z" <release commit>
   git push origin vX.Y.Z
   ```
