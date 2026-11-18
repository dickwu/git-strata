# Releasing git-strata

Releases are built by `.github/workflows/release.yml` on a `macos-15` (Apple
silicon) runner and installed with Homebrew from the existing tap
[dickwu/homebrew-tap](https://github.com/dickwu/homebrew-tap):

```sh
brew install dickwu/tap/git-strata                 # CLI + daemon (prebuilt tarball)
brew install --cask dickwu/tap/git-strata-desktop  # GIT STRATA.app (DMG)
```

Only arm64 (Apple silicon), macOS 13+.

## Artifacts on each GitHub Release

| File | Contents | Used by |
|------|----------|---------|
| `git-strata-<v>-aarch64-apple-darwin.tar.gz` | `bin/git-strata`, `bin/git-stratad`, `share/git-strata/config.toml`, licenses, README | `Formula/git-strata.rb` |
| `Git-Strata_<v>_arm64.dmg` | `GIT STRATA.app` (with its own copies of both binaries) | `Casks/git-strata-desktop.rb` |
| `Git-Strata_<v>_arm64.app.tar.gz` | Signed app bundle with both binaries and `Contents/Resources/config.toml` | In-app updater |
| `Git-Strata_<v>_arm64.app.tar.gz.sig` | Tauri signature over the updater archive, bound to the release version | In-app updater |
| `latest.json` | Arm64 update feed with the release URL, signature, notes and advisory store schema | In-app updater |
| `*.sha256`, `SHA256SUMS.txt` | Individual Homebrew checksums and an aggregate covering all release payloads | Checking downloads |

Binaries are signed with the identifiers `io.github.dickwu.git-strata.cli` and
`io.github.dickwu.git-strata.daemon` in both the tarball and the app.
The assembly script reads the compiled release metadata from
`cargo run --locked -p strata-types --example print-release-meta` and writes
`GitStrataStoreSchema` and `GitStrataDowngradeFloor` into the app's signed
`Info.plist`. The updater feed takes its advisory schema from that bundle and
sets `min_updatable_from` to `0.3.0`.

## Signing modes

Official release tags require a stable signing identity and default to the
self-signed identity. The workflow uses Developer ID when its identity is
configured. Forks and dry runs can use ad-hoc signing when neither identity is
available.

| Mode | Secrets | Gatekeeper (cask app) | TCC grants (e.g. removable volume) |
|------|---------|----------------------|------------------------------------|
| `developer-id` + notarized | `APPLE_DEVELOPER_ID_P12`, `APPLE_DEVELOPER_ID_P12_PASSWORD`, `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, `APPLE_TEAM_ID` (variable `APPLE_DEVELOPER_ID_CERT_SHA1` optional) | opens normally, with the DMG and updater bundle stapled | app: stable (DR = Apple anchor + team + identifier) |
| `developer-id`, not notarized | the two P12 secrets only | blocked on first open; Open Anyway | app: stable |
| `self-signed` | `STRATA_CODESIGN_P12`, `STRATA_CODESIGN_P12_PASSWORD` (variable `STRATA_CODESIGN_CERT_SHA1` optional; see `apps/desktop/scripts/make-signing-identity.sh`) | blocked on first open; Open Anyway | app: stable (DR = identifier + certificate) |
| `adhoc` (forks and dry runs) | none | blocked on first open; Open Anyway | **reset by every upgrade** (DR = cdhash) |

The Developer ID `.p12` should hold one identity plus the Apple intermediate
certificate (or pin it with `APPLE_DEVELOPER_ID_CERT_SHA1`); `import-identity.sh`
uses the first identity it finds.

What this means:

- **Formula (CLI + daemon).** Homebrew downloads with curl and does not set the
  quarantine attribute, so Gatekeeper does not check the formula binaries in
  any mode. Notarization matters for the app only.
- **Cask (app).** Homebrew quarantines cask downloads. Without a notarized
  Developer ID build, macOS refuses the first open; allow it once in System
  Settings > Privacy & Security > Open Anyway, or, if it is not listed, run
  `sudo xattr -dr com.apple.quarantine "/Applications/GIT STRATA.app"`. The
  cask's caveats print the same hint; the cask does not strip the attribute
  itself. (`--no-quarantine` is no longer supported by Homebrew.) Checked on a
  Mac with dickwu/tap formulas installed: their binaries carry
  `com.apple.provenance` only, while the r2 cask app carries
  `com.apple.quarantine`.
- **TCC.** macOS keys privacy grants (for example access to a removable or
  external volume holding repos) to the binary's designated requirement. An
  ad-hoc DR is the cdhash, which changes with every build, so after each
  `brew upgrade` macOS asks again. A self-signed or Developer ID identity keeps
  the app's grant (TCC keys a bundle by its id) across upgrades as long as the
  same certificate signs every release. The formula's `git-stratad` is a bare
  binary that launchd runs from the versioned Cellar path behind the `opt`
  symlink; TCC may key it by that path, so expect the prompt again after a
  formula upgrade in every mode until a real upgrade shows otherwise.
  Development builds (`scripts/dev-install.sh`) use the identifiers
  `com.gwd.git-strata(d)`, release builds `io.github.dickwu.git-strata.cli` /
  `.daemon`: their grants are never shared.
- **Touch ID (confirmations).** The daemon's confirmations use
  LocalAuthentication (`LAContext`, device-owner authentication) with no
  keychain items, so Touch ID and the password fallback work the same in every
  signing mode; nothing is bound to the code identity and nothing needs
  re-approval after an upgrade. The prompt names the requesting process.
- **Changing modes later** (for example ad-hoc to Developer ID) changes the DR
  once: expect one more TCC prompt on the first upgrade after the switch.

## Updater signing key

Updater signing runs only in the GitHub Actions release workflow. Configure
the repository secrets `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The matching public key belongs in
`plugins.updater.pubkey` in `apps/desktop/src-tauri/tauri.conf.json`
(current Minisign key ID `42527D19B72BC853`). The app requires a version-bound
signature. Local builds keep `createUpdaterArtifacts: false`. Keep a
separate encrypted offline copy of the updater private key. Losing the key
requires a manual update for apps that trust it.

Tag pushes to `dickwu/git-strata` require both secrets before the build starts.
Forks and `workflow_dispatch` dry runs can skip updater artifacts when either
secret is absent. The updater test runs in those builds with disposable
credentials. The release workflow signs with
`tauri signer sign --app-version <version>`. Before generating `latest.json`,
it verifies the archive signature against the app's configured updater public
key and checks the signed version. It extracts the archive using the updater's
bundle layout, verifies the macOS code signature and compatibility metadata,
and checks the bundled daemon's version.

## First updater release (0.3.0)

Before publishing 0.3.0, pass all three manual gates using 0.3.0's own signed
CI artifacts. Use a signed `workflow_dispatch` build or a throwaway tag on a
fork. Run the gates on a GitHub macOS runner or in a spare macOS user account
with the test bundle outside `/Applications`.

- **G1** exercises the updater plugin's swap of the signed app bundle with its
  nested binaries, then confirms the app relaunches.
- **G2** checks quarantine, Open Anyway and App Management for the bundled
  daemon when launched by launchd and for an app replacing its own bundle.
- **G3** exercises exit-75 respawn of the bundled daemon under a throwaway
  launchd label and checks for repeated codesign kills. Verify that a disabled
  loaded job stays stopped after a nonzero exit and that disabling it also
  cancels a respawn pending in launchd's throttle. Verify that launchd refuses
  `kickstart` for the disabled loaded job. A disabled-job respawn blocks the
  release until validated replacements exist for all dependent fences.

## Release steps

1. Bump the version in every crate's `Cargo.toml`, `apps/desktop/src-tauri/Cargo.toml`,
   `apps/desktop/src-tauri/tauri.conf.json` and `apps/desktop/package.json`. Refresh the
   lock files and merge to `main`. The workflow checks the tag against `git-strata`,
   `strata-daemon` and `tauri.conf.json`. Assembly checks `strata-types` against
   the app. Check every other version field during the bump.

   Update `strata_types::STORE_SCHEMA` when the store schema changes. If the
   store, pins or config format changes, check that `strata_types::DOWNGRADE_FLOOR`
   names the oldest release that can read the files this release writes.
2. Optional dry run: Actions > Release > Run workflow on `main`. It builds,
   signs and uploads the files as a workflow artifact. Configure both updater
   signing secrets to exercise updater signing in the dry run.
3. Tag and push:

   ```sh
   git tag -a v0.1.0 -m "git-strata 0.1.0" <commit on main>
   git push origin v0.1.0
   ```

   The workflow builds, signs (and notarizes when configured) and creates a
   **draft** GitHub Release with the CLI tarball, DMG, updater archive, signature,
   manifest and checksums.
   The first time only: push and merge the tap branch `add-git-strata` (README
   section and placeholder files) BEFORE publishing, so homebrew.yml updates the
   files instead of colliding with the branch later.
4. Check that the draft contains `latest.json`, its matching archive and
   signature, and the checksums for all three. Confirm the manifest version and
   URLs point to this tag. The updater feed takes its notes from the opening
   paragraph of `docs/release-notes/v<version>.md`. Official tag builds warn if
   that paragraph is missing. Put the notes file above the
   generated notes before publishing.

   `{ cat docs/release-notes/vX.Y.Z.md; echo; gh release view vX.Y.Z --json body -q .body; } > /tmp/notes.md && gh release edit vX.Y.Z --notes-file /tmp/notes.md`

   For 0.3.0, confirm the [first updater release gates](#first-updater-release-030)
   passed before publishing. Publish it as a stable release. Publishing moves
   `releases/latest` and makes the in-app update live immediately. The Homebrew tap follows
   asynchronously. Publishing fires
   `.github/workflows/homebrew.yml`, which downloads both artifacts, computes
   their sha256, renders `Formula/git-strata.rb` and `Casks/git-strata-desktop.rb`
   from `packaging/homebrew/*.rb.in` (`scripts/release/render-tap.sh`) and PUTs
   them to dickwu/homebrew-tap with `gh api` (updating with the existing blob
   sha, or creating the file). It needs the repository secret
   **`HOMEBREW_TAP_TOKEN`**: a token with contents write access to
   dickwu/homebrew-tap. To rerun it: Actions > Update Homebrew tap > Run
   workflow with `tag` = `v0.1.0`, or `gh workflow run homebrew.yml -f tag=v0.1.0`.

   By hand instead (no token):

   ```sh
   cd /path/to/homebrew-tap && git pull
   gh release download v0.1.0 --repo dickwu/git-strata --pattern SHA256SUMS.txt --dir /tmp
   t=$(awk '/aarch64-apple-darwin.tar.gz$/{print $1}' /tmp/SHA256SUMS.txt)
   d=$(awk '/_arm64.dmg$/{print $1}' /tmp/SHA256SUMS.txt)
   /path/to/git-strata/scripts/release/render-tap.sh 0.1.0 "$t" "$d" .
   brew style Formula/git-strata.rb Casks/git-strata-desktop.rb
   git add Formula/git-strata.rb Casks/git-strata-desktop.rb
   git commit -m "git-strata 0.1.0" && git push
   ```

5. Check: `brew update && brew info dickwu/tap/git-strata`.

The release must be public: Homebrew downloads the artifacts anonymously.
