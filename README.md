<p align="center">
  <img src="assets/cadence-mark.svg" width="96" height="96" alt="Cadence logo">
</p>

<h1 align="center">Cadence</h1>

<p align="center">
  A minimal Spotify player for macOS and Windows.<br>
  Rust, GPUI rendering, and native audio playback.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-black.svg" alt="MIT license"></a>
</p>

![Cadence showing the Liked Songs library](assets/cadence.webp)

This is an open-source Windows-focused fork of
[infomiho/cadence](https://github.com/infomiho/cadence). The original MIT license
and author attribution are preserved. macOS source support remains available.

## Windows downloads and updates

Open the [Releases](https://github.com/parentless/cadence-for-windows/releases/latest) page for the optimized Windows x86-64 EXE
or portable ZIP. These are release builds with thin LTO, an embedded C runtime,
assets and precompiled GPUI shaders. The target computer needs no Rust toolchain
or Spotify desktop app. Spotify Premium and your own Spotify developer Client
ID are required; the app guides you through setup.

Windows release builds check this repository's public GitHub releases in the
background after launch and once daily. **Settings → Updates → Install update**
downloads and verifies the new executable, then restarts Cadence to replace it.
Automatic checks can be disabled. Keep Cadence in a writable directory such as
`%LOCALAPPDATA%\Programs\Cadence`. Debug builds do not self-update.

Every push to `main` builds an optimized EXE/ZIP in Actions. Bumping `version`
in `Cargo.toml` and its `Cargo.lock` package entry publishes a new stable
`vMAJOR.MINOR.PATCH` release automatically after Windows checks pass. Re-pushing
the same version keeps its published release immutable and leaves fresh build
artifacts in the workflow run. Tag pushes and manual runs are supported too.
No Apple signing secrets, private access tokens, or separate update server are
needed. See [Windows setup](docs/WINDOWS.md) and [releasing](docs/RELEASING.md).

## Features

- Search tracks and playlists
- Play music locally with queue, history, favorites, and radio
- Browse native artist and album pages
- Browse all playlists in a virtualized sidebar with truncated names; pin them from their page
- Open playlists in 15-entry pages; fetch more near the scroll end
- Double-click tracks to play; right-click for the same actions across every track list
- Choose Graphite, Blue, Green, Purple, or Rose selection fills in Settings
- Scale the UI from 75% to 125% with saved zoom presets
- Use current-track play/pause controls directly in any track list
- Now Playing and media key controls
- Optional Discord Rich Presence with current track, artist, artwork, and progress
- Light, dark, and system appearances
- Optional pixel-art player mascot
- Persistent playback and library state

## Built With

**Rust** + **GPUI** + **librespot** + **rspotify** + **SQLite** + **CPAL**

GPUI renders the native GPU-accelerated interface, librespot handles playback
through CPAL, rspotify connects to the Spotify Web API, and SQLite keeps local state.

## Install

For Windows, see [Windows build and installation instructions](docs/WINDOWS.md).
The **Windows** Actions workflow produces a portable `.exe` and release zip;
this fork preserves the Rust, GPUI, librespot, and CPAL architecture.

### macOS

1. Download the latest DMG from [Releases](https://github.com/infomiho/cadence/releases/latest).
2. Open the DMG and drag Cadence into Applications.
3. Launch Cadence from Applications and follow the Spotify setup below.

Release downloads are signed with Developer ID and notarized by Apple.

## Set Up Spotify

Cadence requires Spotify Premium. On first launch, Cadence guides you
through creating a Spotify developer app and entering its Client ID. Client IDs
are public; Cadence never needs your client secret.

1. Create an app in the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard).
2. Add `http://127.0.0.1:8888/callback` as a redirect URI.
3. Copy the Client ID from **Basic Information** and enter it in Cadence.

Cadence stores the Client ID in app preferences and keeps login and playback
tokens in macOS Keychain or Windows Credential Manager.

## Run From Source

On Windows, follow the [MSVC and Windows SDK build instructions](docs/WINDOWS.md).
On macOS, building Cadence requires Rust and Apple's Metal Toolchain. Developers can skip
the setup screen by setting `SPOTIFY_CLIENT_ID` at launch:

```sh
SPOTIFY_CLIENT_ID="your-client-id" cargo run
```

For a stable local signature and fewer Keychain prompts during development:

```sh
SPOTIFY_CLIENT_ID="your-client-id" ./scripts/run-signed.sh
```

## Releases

Windows releases are built and published by this fork's Actions workflow.
See [releasing](docs/RELEASING.md) for the complete versioning and update flow.
The macOS signing scripts remain in source for maintainers who configure their
own Apple signing and Sparkle feed; this fork's publisher produces Windows assets.

Release builds do not include a shared Spotify Client ID. Each person configures
their own Spotify developer app on first launch. Logo and bundled backend
attributions are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## License

[MIT](LICENSE)
