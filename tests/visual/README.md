# Native visual checks

Run on macOS with Metal available and the same rendering environment as the baseline:

```sh
cargo test --test ui_rendering --locked
```

The runner renders production views twice per case and requires identical RGBA pixels. Failures write expected, actual and difference images to `target/ui-artifacts`. CI runs this target explicitly and uploads only failed cases, the reference manifest, the lockfile and OS details. Diagnostic PNGs use lossless compression and retain every pixel. Passing runs upload no visual artifact.

The 136 full-window cases cover library and queue states, search text and focus, empty and filled setup inputs, autoplay on and off, the closed and open mascot selector, artist-first search, enabled shuffle, and instant collapsed-sidebar playlist tooltips. Each runs in light and dark appearances at 1280 × 820 and 900 × 820 points. Queue cases cover closed and open states with idle, hover and pointer-down input.

Eight additional cases cover the focused queue button, closed and open, in both appearances and layouts. These capture the 40-point control with four points of surrounding space. Keyboard focus adds a two-point circular border using `focus_ring`, preserving the icon, geometry and selected background.

All 144 references in `baseline` are reviewed Cadence 0.8.10 / gpui-kit 0.7.0 captures from hosted macOS 15.8.2 arm64, rendered with embedded Inter 3.19, reduced motion, active windows, a deterministic clock, local artwork fixtures and paused playback. Every capture repeats exactly. Shared track rows now link individual artists and albums using already loaded metadata while preserving incomplete legacy credit text. Eight new references cover linked credits and pinned playlists staying above the active recent item, including a hovered scrollbar in its permanent gutter. The gutter has the same inset in headers and rows; interaction tests assert duration geometry remains stable during hover and dragging. Pin ordering is cached across unchanged snapshots; per-row pin detection is constant-time. Navigation and favorite writes resolve metadata on activation and guard against a replacement row receiving stale input. Tests cover all library collections, album tracks, search, queue, playlists and keyboard navigation. The approved unified artist scroll viewport, bounded identity docking and exact backdrop fade remain covered. Settings cache references show version 0.8.10. The manifest records source revision, capture run, environment, lockfile hash and font hashes.

The 0.6.0 to 0.6.1 upgrade was separately verified with zero pixel differences on macOS 27. `upgrade-0.6.0` retains the initial system-font references. These use different font conditions from the CI baseline. The bundled fonts retain their [upstream license](../../assets/fonts/LICENSE.txt).

CI uses macOS 15. Native rasterization can vary across OS and GPU versions, so another macOS version can fail even when the UI is unchanged. For migration parity, compare both source revisions under identical conditions. Keep exact comparison and review baseline changes explicitly.

To record references in a separate directory from an approved source revision:

```sh
CADENCE_UI_BASELINE=/tmp/cadence-reference cargo test --test ui_rendering --locked -- --record
```

To capture on the CI platform, run **Actions → Regenerate screenshots → Run workflow** on the desired branch. The artifact contains repeatable PNGs, the source revision, environment details and lockfile. It expires after 14 days.

Review the images before replacing `tests/visual/baseline/*.png`, update `manifest.json` with the recorded provenance, then commit the baseline through the usual review process. Approved images stay in the repository so CI does not depend on artifact retention. Regeneration does not prove migration parity or automatically approve a visual change.

To preview the artist header with square, tall and wide deterministic local
artwork at both window widths and appearances, without replacing reference images:

```sh
cargo test --test ui_rendering --locked -- --artist-preview
```

This checks repeatable rendering, asserts a fixed square portrait, and writes images to `target/artist-previews`.
The artwork is a test pattern; installed builds use the artist's Spotify image.


To preview the native expanded and docked artist states with the local README artwork fixture:

```sh
CADENCE_README_FIXTURE=/path/to/fixture.json cargo test --test ui_rendering --locked -- --artist-scroll-preview
```

This writes repeatable native screenshots to `target/artist-scroll-previews`.


To preview linked track credits, pinned playlists and the dedicated scrollbar gutter:

```sh
cargo test --test ui_rendering --locked -- --navigation-preview
```

This writes repeatable native screenshots to `target/navigation-previews`.
