"""Render user-reported memory ranges; matplotlib is a documentation-only dependency."""
import json
from pathlib import Path
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

root = Path(__file__).resolve().parents[1]
data = json.loads((root / "docs/performance-observations.json").read_text())
plt.rcParams.update({"font.family": "DejaVu Sans", "svg.fonttype": "none"})
fig, ax = plt.subplots(figsize=(11, 4.6), dpi=160)
background = "#191a1e"
fig.patch.set_facecolor(background)
ax.set_facecolor(background)
fig.subplots_adjust(left=.14, right=.88, top=.67, bottom=.23)
for index, item in enumerate(data["observations"]):
    y = 1 - index
    low, high = item["min_mb"], item["max_mb"]
    color = ["#86d6b2", "#b0b4bf"][index]
    ax.barh(y, low, height=.36, color=color, zorder=3)
    ax.barh(y, high-low, left=low, height=.36, color=color, alpha=.3, zorder=3)
    ax.vlines(high, y-.18, y+.18, color=color, linewidth=2, zorder=4)
    ax.text(high+16, y, f"{low}–{high} MB", va="center", color="#f0f1f4", fontsize=15, weight="bold")
ax.set_xlim(0, 850)
ax.set_ylim(-.6, 1.6)
ax.set_yticks([1, 0], [item["name"] for item in data["observations"]], fontsize=14, color="#eceef3")
ax.set_xticks([0, 200, 400, 600, 800], ["0", "200", "400", "600", "800 MB"], color="#9196a3", fontsize=10)
ax.tick_params(axis="both", length=0, pad=12)
ax.grid(axis="x", color="#34373e", linewidth=.7, zorder=0)
for spine in ax.spines.values(): spine.set_visible(False)
fig.text(.075, .88, "Reported memory usage", color="#f1f3f5", fontsize=23, weight="bold")
fig.text(.075, .79, "Cadence and Spotify on Windows", color="#a9afbc", fontsize=12)
fig.text(.075, .095, "User-reported ranges • not a controlled benchmark", color="#c3c8d2", fontsize=10)
fig.text(.075, .045, "Workload, cache state and process accounting were not independently verified.", color="#8f96a4", fontsize=9)
for suffix in ["png", "svg"]:
    fig.savefig(root / f"assets/memory-comparison.{suffix}", facecolor=background, dpi=160)
plt.close(fig)
