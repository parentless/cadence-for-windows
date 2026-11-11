# Native visual checks

Run on macOS with Metal available and the same rendering environment as the baseline:

```sh
cargo test --test ui_rendering --locked
```

The runner renders production views twice per case and requires identical RGBA pixels. Failures write expected, actual and difference images to `target/ui-artifacts`. CI runs this target explicitly and uploads only failed cases, the reference manifest, the lockfile and OS details. Diagnostic PNGs use lossless compression and retain every pixel. Passing runs upload no visual artifact.

The 164 full-window cases cover library and queue states, search text and focus, empty and filled setup inputs, autoplay on and off, the closed and open mascot selector, Gallery search in initial and scrolled states, linked album-header artists, enabled shuffle, and instant collapsed-sidebar playlist tooltips. Each runs in light and dark appearances at 1280 × 820 and 900 × 820 points. Queue cases cover closed and open states with idle, hover and pointer-down input.

Eight additional cases cover the focused queue button, closed and open, in both appearances and layouts. These capture the 40-point control with four points of surrounding space. Keyboard focus adds a two-point circular border using `focus_ring`, preserving the icon, geometry and selected background.

All 172 references in `baseline` are reviewed Cadence 0.8.19 / gpui-kit 0.7.0 captures from hosted macOS 15.8.2 arm64, rendered with embedded Inter 3.19, reduced motion, active windows, a deterministic clock, local artwork fixtures and paused playback. Every capture repeats exactly. Artist references use a continuous fade with an exact page-colour ending, replacing adjacent bands that could expose seams at fractional Windows DPI/zoom. Backdrop extent and content positions remain unchanged. Whole-row hover reveals the index play control; its reference now activates from the row centre. Wheel-response tests cover first-frame input, reversal, sustained scrolling and pointer jitter. Existing docking, row spacing, sidebar pins, linked credits, scrollbar gutters, cached suggestions and rail labels remain covered. Gallery uses capped artist and album cards above shared track rows, with all sections scrolling together. Album-header artists are linked. All sixteen Settings captures changed for the tab rail, the Last.fm section and version 0.8.19; the other 156 references remain pixel-identical to 0.8.18. Settings references show version 0.8.19. The manifest records source revision, capture run, environment, lockfile hash and font hashes.

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


Preview the cached dropdown, album cover shadow, artist fade and static playback control with `CADENCE_README_FIXTURE=/path/to/fixture.json cargo test --test ui_rendering --locked -- --catalog-polish-preview`. Images are written to `target/catalog-polish-previews`.


Preview centered rail labels, the index play control and playlist error recovery with `cargo test --test ui_rendering --locked -- --rail-polish-preview`. Images are written to `target/rail-polish-previews`.
