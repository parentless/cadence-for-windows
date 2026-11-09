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


# Review of 0.8.7–0.8.11

Discrete wheel input is coalesced into one fixed-size accumulator per window.
An 80 ms ease-out preserves line distance and the operating system's wheel
setting; it does not add momentum. Precise touchpad input, scrollbar dragging,
keyboard navigation and reduced-motion input remain direct. Direction reversal,
pointer movement, clicks and route changes cancel pending movement. One frame
callback is outstanding at most; settling stops frame demand. Only native
scroll recipients are invalidated, rather than repainting the entire workspace
for an animation timer. No extra track metadata, background blur or network
request is needed for a scroll frame.

The artist virtual list now preserves its release anchor through zoom/column
changes instead of resetting to the header. Track rows retain a constant stride
with permanent gaps around selection/hover fills; Popular uses a smaller fixed
stride. Context-menu identity shares the existing track ID rather than
formatting another string on every visible row repaint. Stable artist/header
geometry is checked before scheduling deferred updates, avoiding allocation of
no-op closures on normal scroll frames.

The recent paging, artwork, header-docking and Discord paths were reviewed:
playlist metadata remains eight pages, artwork eviction drops renderer images,
blur work is cached, scroll callbacks capture weak page handles, identity
settling has a finite deadline, and Discord keeps a replaceable watch value
rather than an accumulating event queue. This source review found no obvious
new retention cycle in those paths. It is not a Windows heap/GPU profile and
does not establish zero leaks or zero rendering cost. Smooth wheel movement
necessarily draws intermediate frames during input; it adds no idle polling.

Popular requests Spotify's ranked top-tracks endpoint where the application's
API access permits it, preserving Spotify's response order. A 403/404 disables
that probe for the session (shared across client clones), retaining artist
search as a fallback. Auth, rate-limit and server errors retain normal handling.
Spotify's [February 2026 changes](https://developer.spotify.com/documentation/web-api/references/changes/february-2026)
removed top tracks for new Development Mode apps, so the fallback cannot promise
the same ranking as Spotify's own client. No private scraping endpoint or
continuous popularity polling is added.
