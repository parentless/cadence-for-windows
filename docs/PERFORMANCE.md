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

Artwork has a shared 48-entry / 32 MiB retained-image budget (decoded pixels
and retained compressed sources combined; one oversized entry is allowed). Detail pages use
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


# Cached suggestions and artwork polish (0.8.12)

Search autocomplete waits 120 ms after typing and returns at most six
identity references into loaded favorites, recent history and liked songs.
One cancellable task shares the existing library arrays temporarily; matching
runs on the background executor, and no full-text index, persistent suggestion
cache, artwork request or Spotify request is added. Query matching is limited
to 96 characters and eight words. ASCII matches scan borrowed bytes; Unicode
case folding uses temporary strings. A new query replaces the prior task.
Escape, submission, route changes and outside clicks dismiss suggestions.
Playback resolves and validates the original track identity against current
library data, cloning full metadata only after activation. Enter without a
selected suggestion continues to perform the explicit Spotify search.

Album track rows omit their image elements entirely, avoiding repeated artwork
lookups. The large album cover has two static box shadows, not a blurred duplicate
image or animated effect. The artist fade uses the same existing gradient and
solid seam layers in a smaller area near Popular; no new image processing pass
is added. Pending playback keeps a static play/pause icon rather than running a
spinner. These changes add small, bounded suggestion state and transient search
work during typing; they do not establish a measured zero-cost CPU/GPU claim.

# Rail and playlist recovery (0.8.13)

Collapsed-sidebar labels reuse one trigger-bounds cell per sidebar. Their native
hover lifecycle positions a single tooltip at the trigger's center with no
additional animation, polling or idle frame requests. Track numbers and play
controls share a fixed 44 by 32 logical-pixel area at 100% zoom. GPU opacity
styles switch the glyph on hover without changing row layout or creating a task.
Play callbacks share their existing allocation between row and index activation.
Track covers no longer create navigation controls.

Failed playlist pages stop requesting until an explicit retry. An initial
failure shows the actual error instead of a logical playlist full of skeletons;
retry requests the missing visible page (or the first page before layout) once.
The existing 100 ms scroll debounce, eight-page/120-track cache, cancellation,
Spotify throttling gate and playback window bounds remain in place. A 403
response explains Spotify's owner/collaborator restriction without retrying or
trying to work around it. No new network request is added to successful loads.

These are code bounds and regression checks, not a Windows CPU/GPU or long-run
RAM measurement. No claim of zero overhead or a measured speedup is made.


# Audio, artwork and short artist pages (0.8.14)

Playback reuses its float-conversion, channel-mapping and resampling buffers.
Stereo channel mapping and matching-rate output borrow their inputs directly.
Resampling uses Rubato's preallocated-output API with the same FFT settings;
regression tests compare exact samples against its allocating API across four
output rates and irregular packet boundaries. Reset clears pending samples and
resampler history without discarding buffer capacity. A repeated-packet test
checks that warmed buffer pointers and capacities stay unchanged. This reduces
allocation churn; it does not change the audio queue, quality or supported devices.

Artwork requests choose power-of-two sizes from 64 to 2048 physical pixels,
including UI zoom and display scale, and preserve the source aspect ratio.
Small artwork is not enlarged. One decoded size per URL and its compressed
source share the existing byte budget. Upgrades reuse that source and keep the
old thumbnail visible until ready; in-flight downloads finish before upgrading.
Two shared workers bound simultaneous download/decode/blur work. Downloads
retain the existing 10 MiB cap and timeouts; decoders additionally limit source
dimensions and allocation. Decode work has transient memory outside the
retained cache budget. Completed results are accounted and evicted on the UI
thread even while the window is idle. Cache clearing cancels waiting requests;
an abort-on-drop guard also cancels the underlying Tokio HTTP task.
Notifications hold a weak cache owner, and pending views are drained on
completion. Blur reads the source pixel slice directly rather than cloning it.

The approved artist backdrop extends 46 logical pixels farther at 100% zoom
and uses static gradient bands. Header and Popular share a virtual item so the
fade cannot disappear early when the header scrolls out. Track spacing and
discography virtualization remain unchanged. Docking uses the smaller of the
normal transition distance and actual available scroll distance; short pages
therefore reach the dock. Switching artists retains a usable Back destination.
Regression tests scroll short pages, switch artist, scroll again and click Back.

These changes have structural bounds and regression coverage. They have not
been measured with a Windows allocation profiler or a long-running GPU/CPU
capture, so they do not establish a measured speedup or prove absence of every leak.
