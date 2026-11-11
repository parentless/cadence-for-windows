# Windows releases

The `Windows release` workflow runs for pushes to `main`, `v*` tags, and manual
runs. It calls the Windows build workflow, which tests on Windows, runs Clippy
and formatting checks, then uses `cargo build --release --locked` and packages
an EXE, ZIP, and SHA-256 files. The GitHub repository name is embedded as the
update feed with `CADENCE_UPDATE_REPOSITORY=${{ github.repository }}`.

## Publish a version

1. Update the stable `MAJOR.MINOR.PATCH` version in `Cargo.toml` and the `cadence`
   package entry in `Cargo.lock`.
2. Commit and push to `main`. Actions builds and tests the executable.
3. The publisher creates `vMAJOR.MINOR.PATCH` at that commit. It uploads assets
   to a draft first and publishes only after uploads finish. Failed draft
   uploads can be retried by rerunning the workflow.
4. Download the public EXE or ZIP from Releases and test playback, Discord,
   settings, system media controls, and in-app update installation on Windows.

Published versions are not overwritten on subsequent pushes. Increase the
version to ship another update. Every successful build still has optimized
artifacts attached to its Actions run. When tagging manually, the tag must
match the Cargo version; do not publish a competing release by hand.

Keep old public GitHub releases for users and rollback. Local packaging clears
superseded Windows files in `dist` only after generating a replacement.

The standalone download and executable inside the ZIP are named `Cadence.exe`.
A versioned executable alias and checksum remain available for older installed
updaters. Both executables contain identical optimized bytes.

## In-app updater

The app checks the public repository's latest stable release 15 seconds after
launch and every 24 hours when automatic checks are enabled. Users can check
and install from Settings. Network traffic, streamed downloads, checksums, and
file writes run away from the UI and audio threads. There is one bounded
result channel and only one operation at a time.

Only a newer numeric stable version is offered. Drafts, prereleases, debug
asset names, wrong repositories/tags, oversized metadata/downloads, malformed
checksums, and checksum mismatches are rejected. Metadata is capped at 1 MiB,
checksum text at 1 KiB, and executable downloads at 300 MiB.

After verification, a short-lived PowerShell helper waits for normal shutdown,
keeps a rollback executable while replacing it, restarts Sonder, and cleans
up. Installation needs a writable app folder. A system policy that disables
PowerShell can prevent installation; a portable ZIP remains available.

The repository's normal `GITHUB_TOKEN` publishes releases; no token is embedded
in the app. Enable GitHub Actions in the public repository. The workflow grants
write permission only to the publishing job. Action dependencies are pinned
to verified release commits.

The source retains the upstream MIT license and bundled third-party notices.
The Windows release workflow does not sign executables with an Authenticode
certificate. Windows may display its usual warning for unsigned downloads.

## macOS visual references

Keep screenshot references from the documented macOS CI platform. If an
intentional UI change needs new references, run **Regenerate screenshots**,
review its uploaded images, update the baseline and manifest provenance, then
commit those reviewed results. Do not replace them with local-machine images.
