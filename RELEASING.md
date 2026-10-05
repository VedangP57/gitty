# Releasing gitty

Releases are built by [dist](https://github.com/axodotdev/cargo-dist): pushing a `vX.Y.Z` tag runs
`.github/workflows/release.yml`, which builds the four archives (macOS and Linux, arm64 and
x86_64), creates the GitHub Release with the shell installer, and pushes the formula to
[VedangP57/homebrew-tap](https://github.com/VedangP57/homebrew-tap).

One-time setup: the `HOMEBREW_TAP_TOKEN` repository secret holds a token that can write to the tap.

## Checklist

1. Bump `version` in the workspace `Cargo.toml` (and the path dependencies' `version`), then
   `cargo check` so `Cargo.lock` follows.
2. Regenerate the licence notices and check nothing changed unnoticed:
   `scripts/third-party-licenses.sh && git diff THIRD-PARTY-LICENSES.md` (the script rewrites the
   file in place).
   A new dependency under a licence `about.toml` doesn't accept fails the script: review the licence
   before adding it.
3. In `CHANGELOG.md`, move the `[Unreleased]` entries under `## [X.Y.Z] - YYYY-MM-DD`. dist uses
   that section as the release notes.
4. `dist plan` lists the artifacts; `dist build --artifacts=all --target aarch64-apple-darwin` builds
   one archive, the installer and the formula into `target/distrib/` for a look.
5. Commit, push `main`, wait for CI, then `git tag vX.Y.Z && git push origin vX.Y.Z`.
6. When the workflow finishes: `brew update && brew install vedangp57/tap/gitty` (or
   `brew upgrade gitty`) and run `gitty --version`.

After upgrading dist, run `dist init` again so the workflow matches the new version: the release
workflow's `plan` job fails when `release.yml` is out of date.

The README demo (`assets/demo.gif`) is re-recorded with `assets/demo/record.sh <linux gitty binary>`
(Docker; a made-up repository, so no real data) when the UI changes visibly.
