# Project State

The GUI creates and governs projects. `trailgen.toml` is the project mark and the stable home of advanced defaults; ordinary workflow should mutate map areas, search intent, trails, and names through the workbench.

## Ownership

| State | Canonical owner |
| --- | --- |
| Project name, advanced constraints, solver/search envelope | `trailgen.toml` |
| Live map-area rectangles, names, and provider roster | `[trail_data]` in `trailgen.toml` |
| Saved trails and compact search recipe | `library/index.json` |
| Normalized routable corpus | `cache/graph.bin` plus sequestered `sources/` evidence |
| Viewport, inspector, gallery, sorting, unfinished manual draft | `$XDG_STATE_HOME/trailgen/projects/` |
| Base Pace and other app-wide preferences | `$XDG_CONFIG_HOME/trailgen/preferences.toml` |
| Shared roaming map data | `$XDG_CACHE_HOME/trailgen` |

No generated-route snapshot is current project state. `routes/generated.*` is read only by the explicit legacy migration path and must never outrank the Library or managed corpus.

## Trail Data

The workbench persists each map rectangle as a `SurveyRegion` with a content-derived identity:

```toml
[trail_data]
managed = true
providers = [
  "ny-state-parks",
  "osm",
  "texas-state-parks",
  "usgs-national-trails",
]

[[trail_data.regions]]
id = "8cd28c863327f323e563b851"

[trail_data.regions.bounds]
west = -74.20
south = 41.10
east = -74.00
north = 41.35
```

Region identities are spatial receipts, not labels or provider IDs. Add, rename, resize, and remove areas in the GUI so desired state, source sequestration, graph replacement, and durable receipts move together. `managed = true` remains true after the final area is removed; an obsolete compatibility snapshot cannot then masquerade as live data.

## Search Defaults

`[constraints]` stores advanced metric windows and shape laws in metres, seconds, fractions, and `FGJW km`. `[search]` bounds solver work; `solver` is `auto`, `heuristic`, or `exact`. The GUI overlays its one compact Library recipe: optional trailhead and boundary, distance, moving-time and climb ranges, target lower-limb load, and shape. Graph-local vertex IDs are never durable interface state.

The GUI’s lower-limb value is a ranking target, not a medical claim or hard capacity ceiling. Base Pace changes the presentation of population moving-time estimates without rewriting route facts. See [physical load and moving time](physical-load.md).

Closed and private edges remain unlawful. Roads are discouraged by finite routing aversion rather than categorical exclusion. Temporal access overlays without enough planning context are conservatively active. Terrain, access, standing, marking, and geometry confidence remain independent channels.

## Export

The Library is the export authority. The GUI’s `↥` control and `trailgen export` both serialize the stored name, exact saved geometry, available elevations, and durable metrics through the same GPX writer. Export neither invokes a solver nor consults legacy `routes/generated.*` files.
