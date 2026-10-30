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
| `*.sha256`, `SHA256SUMS.txt` | sha256 of each artifact | checking downloads |

Binaries are signed with the identifiers `io.github.dickwu.git-strata.cli` and
`io.github.dickwu.git-strata.daemon` in both the tarball and the app.

## Signing modes

The workflow picks the first mode whose secrets exist; with no secrets it signs ad-hoc.

| Mode | Secrets | Gatekeeper (cask app) | TCC grants (e.g. removable volume) |
|------|---------|----------------------|------------------------------------|
| `developer-id` + notarized | `APPLE_DEVELOPER_ID_P12`, `APPLE_DEVELOPER_ID_P12_PASSWORD`, `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, `APPLE_TEAM_ID` (variable `APPLE_DEVELOPER_ID_CERT_SHA1` optional) | opens normally; DMG stapled | app: stable (DR = Apple anchor + team + identifier) |
| `developer-id`, not notarized | the two P12 secrets only | blocked on first open; Open Anyway | app: stable |
| `self-signed` | `STRATA_CODESIGN_P12`, `STRATA_CODESIGN_P12_PASSWORD` (variable `STRATA_CODESIGN_CERT_SHA1` optional; see `apps/desktop/scripts/make-signing-identity.sh`) | blocked on first open; Open Anyway | app: stable (DR = identifier + certificate) |
| `adhoc` (default) | none | blocked on first open; Open Anyway | **reset by every upgrade** (DR = cdhash) |

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

## Release steps

1. Bump the version in every crate's `Cargo.toml`, `apps/desktop/src-tauri/Cargo.toml`,
   `apps/desktop/src-tauri/tauri.conf.json` and `apps/desktop/package.json`; refresh the
   lock files; merge to `main`. The workflow refuses a tag that does not match.
2. Optional dry run: Actions > Release > Run workflow on `main`. It builds,
   signs and uploads the files as a workflow artifact; nothing is published.
3. Tag and push:

   ```sh
   git tag -a v0.1.0 -m "git-strata 0.1.0" <commit on main>
   git push origin v0.1.0
   ```

   The workflow builds, signs (and notarizes when configured) and creates a
   **draft** GitHub Release with the tarball, DMG and checksums.
   The first time only: push and merge the tap branch `add-git-strata` (README
   section and placeholder files) BEFORE publishing, so homebrew.yml updates the
   files instead of colliding with the branch later.
4. Check the draft's files, then publish it (not as a pre-release: homebrew.yml
   skips pre-releases unless dispatched by hand). Publishing fires
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
