# Cadence for Windows

This port keeps Rust, GPUI's GPU renderer, librespot, CPAL's native audio
backend, SQLite, the bounded image cache, resampling, and the existing playback
and library features. Windows uses Direct3D for GPUI rendering, WASAPI for audio,
and Windows Credential Manager for Spotify credentials.

## Build a runnable executable

Use 64-bit Windows 10 (1809 or newer) or Windows 11, with a Direct3D 11 capable
GPU and current graphics drivers. CI builds on GitHub's `windows-2025` runner.

Install:

- Rust through rustup, using the MSVC toolchain. The repository's
  `rust-toolchain.toml` selects the required version and components.
- Visual Studio 2022 Build Tools with **Desktop development with C++**, including
  the MSVC x64 build tools and Windows 10/11 SDK. GPUI uses the SDK's `fxc.exe`
  shader compiler for optimized builds.

In PowerShell at the repository root:

```powershell
rustup target add x86_64-pc-windows-msvc
cargo build --release --locked --target x86_64-pc-windows-msvc
./target/x86_64-pc-windows-msvc/release/cadence.exe
```

The executable embeds the fonts, icons, UI assets, compiled shaders, SQLite,
and the C/C++ runtime. It doesn't require Rust, Visual Studio, Metal, or a
Spotify desktop installation on the machine where you run it.

To produce a distributable zip, standalone exe, and SHA-256 checksums:

```powershell
./scripts/package-windows.ps1
```

Outputs are under `dist/`. Successful packaging removes superseded Windows
packages and staging folders, keeping only the newly generated EXE, ZIP and
checksums. An optional `-Label artist-profile` adds a descriptive build suffix.
Extract the zip to a writable folder such as
`$env:LOCALAPPDATA\Programs\Cadence` and run `Cadence.exe`.

## GitHub Actions build

**Windows release** runs on every `main` push, stable version tag, and manual
run. It invokes the Windows workflow on `windows-2025`, runs tests and lint,
builds the optimized executable with embedded shaders, and packages EXE/ZIP
assets with checksums. Every successful build uploads optimized artifacts.
A new Cargo version automatically publishes a public GitHub release after all
Windows checks pass; a repeat of the same version leaves its release intact.

The updater feed is compiled from `github.repository`, so downloads and updates
come from your own public fork. No macOS signing secrets are needed. See
[releasing](RELEASING.md) for version bumps and publication details.

When working from macOS, use Actions for production Windows builds.
[cargo-xwin](https://github.com/rust-cross/cargo-xwin) with LLVM can cross-check
and link a development executable. Optimized GPUI Windows builds use the
Windows SDK's `fxc.exe` on the Windows runner.

## Spotify setup

A Spotify Premium account is required. Follow the app's setup screen to create
your own Spotify developer app and register
`http://127.0.0.1:8888/callback`. Enter the public Client ID; no client secret is
needed. Authorization opens in your default browser. Allow Cadence's local
loopback callback if Windows asks.

Optionally skip entering the Client ID during development:

```powershell
$env:SPOTIFY_CLIENT_ID = 'your-client-id'
cargo run --target x86_64-pc-windows-msvc
```

Preferences and SQLite data use the current user's application data directory.
Login and playback tokens use Windows Credential Manager through the existing
keyring library. Nothing is stored beside the executable except transient
update files.

## Windows behavior

- **Ctrl+K** searches, **Ctrl+W** closes the window, **Ctrl+Q** quits, and **Space**
  toggles playback when focus is outside a text field or control.
- The standard Windows title bar provides drag, minimize, maximize, and close.
- Track lists share borderless rows with fixed gaps and rounded hover/selection fills. Artwork
  has no outline and uses fixed square frames with aspect-preserving cropping.
- Artist pages use a dark grey background, rounded-corner portraits, and a
  blurred image header fading into the page. The blur is generated off the UI
  thread at up to 512 × 512 pixels and shares the bounded image cache.
- The search bar and track action menus use a static translucent glass treatment.
  The account menu is opaque. On artist pages the toolbar floats over the backdrop.
- Right-click any track row to open its actions menu. Track lists share the
  same play, favorite, navigation and context-menu behavior without trailing dots.
- Closing the last window exits Cadence and stops playback/background services.
  Ctrl+Q also exits; minimizing keeps music and system media keys running.
- The sign-in window stays above the player when reauthentication is required.
- Now Playing metadata, cover art, media buttons, and position updates use
  Windows System Media Transport Controls and a hidden window whose lifetime
  matches the player, rather than the visible window.
- System appearance, the optional mascot, local playback, queue/history/radio,
  favorites, pinned playlists, artist/album browsing, and saved state keep their
  existing implementations.

## In-app updates

Optimized CI builds check their own repository's GitHub releases, using the
compile-time `CADENCE_UPDATE_REPOSITORY=owner/repository` value. Debug and local
builds without a feed remain inert. To enable updates in your own release build:

```powershell
$env:CADENCE_UPDATE_REPOSITORY = 'your-account/your-cadence-fork'
cargo build --release --locked --target x86_64-pc-windows-msvc
```

Publish `Cadence.exe` and `Cadence.exe.sha256` together with the versioned
compatibility alias, ZIP and checksums generated by the packaging script
on a stable `vMAJOR.MINOR.PATCH` GitHub release. The updater compares versions
numerically, checks once a day when enabled, and installs only after the user
chooses **Install update** in Settings. Downloads and SHA-256 verification run
off the UI/audio threads. After normal shutdown, a PowerShell helper replaces
the executable, keeps a rollback copy during replacement, and restarts Cadence.
Installation requires a writable application folder. Corporate policies that
block PowerShell can prevent in-place installation; downloading the release
zip and replacing the executable manually remains available.

## Validation

```powershell
cargo fmt --all --check
cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo test --locked --target x86_64-pc-windows-msvc
```

The existing pixel baselines are macOS-specific and remain in macOS CI.
Before distributing a Windows release, exercise Spotify login and saved login,
playback/pause/seek, queue/radio, media keys with the window closed, second-launch
activation, output-device changes, light/dark appearance, and an actual update
between two releases on Windows with a Premium account. Cross-compilation alone
does not establish that these desktop and account integrations work at runtime.

## If the application fails to open

Windows builds now write a startup log before initializing GPUI. Open this path
in Explorer or Run (Win+R):

```text
%LOCALAPPDATA%\twoducks\cadence\data\logs\cadence.log
```

A startup panic or window-creation failure also displays an error dialog. If the
application still fails to start, include the dialog's error or the final lines
of that log when reporting it. An operating-system loader failure before Rust
starts cannot produce an application log.

Development ZIPs embed their icons, fonts, and HLSL shader source. They do not
need access to the build machine's Cargo registry or source checkout. The
vendored GPUI portability patch affects debug shader loading only; release
builds retain precompiled shaders.

## Discord Rich Presence

1. Open **Settings → Discord → Open Discord Developer Portal**.
2. Create an application named **Cadence**, then copy its **Application ID**
   from General Information. This is public; no bot token or client secret is needed.
3. Paste it into the **Discord Application ID** field, click **Save**, and enable
   **Share listening activity**.
4. Keep the Discord desktop application running with activity sharing enabled,
   then play a track in Cadence.

Your profile shows a Listening activity with track/artist details, album art
when available, progress timestamps, and an Open in Spotify button. The activity
clears when paused, signed out, disabled, or Cadence exits. Browser-only Discord
cannot receive local IPC. This is Cadence Rich Presence, rather than Spotify's
native account connection or Listen Along feature.

Sharing defaults to off. When disabled it creates no IPC connection or worker.
When enabled, a single background worker uses local IPC, a latest-state slot,
bounded incoming frames, and reconnect delays up to 30 seconds. Playback ticks
are deduplicated; playing updates are limited to once per three seconds and
pause clears promptly. No new third-party dependencies or artwork downloads
are introduced. This is designed for low overhead; live CPU/RAM impact has not
yet been benchmarked on Windows.

Protocol reference: [Discord RPC](https://discord.com/developers/docs/topics/rpc).

## Long-running sessions

Autoplay retains up to 50 previously played entries plus the current track in
memory. Older played entries are discarded, so Previous stops at the oldest
retained entry. Upcoming queued songs are preserved, even for a large playlist.
Autoplay excludes duplicates in the retained queue; tracks from older discarded
history may be recommended again. No separate queue-history archive or archive
queries are used. Existing SQLite preferences, library caches and the normal
resume snapshot remain; the resume snapshot includes only the retained queue.
Upgrading from the earlier archive build removes its old archive automatically.

Backend notifications use 128 ordered slots with asynchronous backpressure,
plus a single replaceable position update. A full bridge wakes on shutdown.
Native media input uses 64 slots and rejects excess input during saturation
instead of blocking the operating system's callback thread. Normal playback,
media keys, queue actions, and Discord presence retain their behavior.

Windows diagnostic logs rotate during the run, retaining at most two 5 MiB
files (current and previous). This bounds log disk usage without a restart.

### Spotify Liked Songs

Right-click a track and choose **Add to Liked Songs** or **Remove from
Spotify Liked Songs**. These actions change your Spotify account. Stars and
Favorites remain local to Cadence. If you signed in before this feature was
added, sign out through the account menu and sign in again once to approve
`user-library-modify`. The app reports failed writes and preserves the prior
state. One click-driven request runs at a time; it waits for library refresh
to finish and does not poll Spotify in the background.

The executable embeds Inter and loads the same font family/weights as the
native previews. No font installation is needed. Windows and macOS still use
different rasterizers, and monitor scaling, HDR and colour profiles can affect
the appearance. The app uses the same theme colour values on both platforms.

Submitted search shows a bounded Gallery of Top Artists, Top Albums, and Songs,
with one shared scroll area and no public-playlist discovery. Album header
artist names open their profiles using the loaded Spotify IDs. Closing the last
Windows window exits Cadence, stopping playback and its background services.


Artist discographies load additional releases as you reach the end of the grid,
in ten-release pages. Loaded releases sort newest first, with missing dates
last. A failed page offers Retry without clearing the releases already shown.
