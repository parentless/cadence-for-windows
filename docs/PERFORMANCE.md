# Memory comparison

The README chart illustrates user-reported Windows RAM usage: **50–70 MB for
Cadence** and **400–700 MB for Spotify**. These are informal observations, not an
independently measured or controlled benchmark. Workload, app versions, warm-up,
cache state, and whether all Spotify processes were included were not verified.
The chart makes no CPU, GPU, energy, or latency claim.

The source values are in `performance-observations.json`. To regenerate the
static PNG and SVG, install matplotlib in a separate documentation environment
and run `python scripts/render-memory-comparison.py`. No charting dependency or
benchmark collection runs in the client.

For a controlled comparison, use the same Windows PC, playlist, playback time,
and cache state. Compare optimized release builds after warm-up, include every
process in each application's process group, and record the exact Task Manager
memory metric and app versions. Repeated samples matter more than one reading.

# Runtime bounds

Playlist browsing represents unloaded rows by their logical indices rather than
allocating a placeholder per song. It retains eight 15-item metadata pages,
requests the actual visible range after a 100 ms debounce, and discards older
pages. Navigation remembers one inactive playlist and one inactive album;
inactive metadata also follows the catalog's freshness and memory limits.
Older playlist chunks are evicted, but scroll position is retained: revisiting a
distant range refetches that range directly. Playback continues independently
using Spotify's raw item cursor.

Artwork has a shared 48-entry / 32 MiB decoded-image budget. Detail pages use
the same theme background as search, with no cover-colour sampling. Artist
skeletons are static; there is no shimmer timer or new blur pass.
