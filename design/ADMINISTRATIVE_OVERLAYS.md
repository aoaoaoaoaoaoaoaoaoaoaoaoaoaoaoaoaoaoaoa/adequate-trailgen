# Administrative Overlays

Status: design; implementation deferred until playtesting confirms the first
search space.

## Contract

An `Overlay` is durable project context: one selected, named geographic area
drawn without affecting routing, acquisition, search boundaries, or the active
view. Search boundaries remain route constraints and must never be called
overlays.

The canonical identity is a versioned source key, not display text:

```text
BoundaryKey { dataset, vintage, geoid }
BoundaryCatalogEntry { key, name, kind, jurisdiction, centroid, bounds }
BoundarySnapshot { key, multipolygon, provenance }
```

Project state owns the ordered set of active keys and their normalized
snapshots. XDG state owns only the `Overlays` shutter. Query text, suggestions,
selection, hover, acquisition progress, and prepared meshes are session state.

## First Search Space

The first catalog contains active United States Census incorporated places and
consolidated cities in the fifty states, District of Columbia, and Puerto Rico.
It therefore includes the jurisdictional forms users normally call cities,
towns, villages, boroughs, and municipalities without pretending their local
legal names form one national ontology.

Census-designated places are excluded: they are statistical areas, not city
governments. Counties, county subdivisions, neighborhoods, ZIP Code
Tabulation Areas, urban areas, metropolitan areas, parks, and tribal areas are
also separate future providers. This prevents one search field from silently
mixing incompatible meanings.

`New York city, NY` is present. Brooklyn is not an incorporated place; a later
county/borough provider should expose `Brooklyn / Kings County` rather than an
alias that lies about the selected geometry.

## Census Plane

The national Places Gazetteer is the search catalog. It is compact, supplies
stable identifiers, names, representative coordinates, land and water area,
and covers the intended jurisdiction. A release-time generator should refine
it with legal/status metadata and emit one compact, versioned index shipped
with Trailgen. Autocomplete must be instant and offline; first input never
starts a network request.

Selection resolves geometry from the corresponding full-resolution,
state-based TIGER/Line Place or Consolidated City product. The 1:500,000
cartographic boundary files are deliberately rejected: their generalization
can omit or materially displace small boundary details at Trailgen's deep map
scales.

Raw state archives belong in a shared XDG cache keyed by Census vintage and
state. A selected, normalized `MultiPolygon` snapshot belongs in the project
cache so restart and offline use reproduce the same boundary. A newer vintage
never replaces project geometry silently; refresh is an explicit migration.

References:

- [2025 Census Gazetteer Files](https://www.census.gov/geographies/reference-files/time-series/geo/gazetteer-files.2025.html)
- [Gazetteer record layouts](https://www.census.gov/programs-surveys/geography/technical-documentation/records-layout/gaz-record-layouts.html)
- [2025 TIGER/Line files](https://www.census.gov/geographies/mapping-files/2025/geo/tiger-line-file.html)
- [Cartographic boundary generalization](https://www.census.gov/programs-surveys/geography/technical-documentation/naming-convention/cartographic-boundary-file.html)

## Inspector

`Overlays` follows `Map Areas` in the left inspector. It contains one search
field and the active overlay rows. Each row shows a swatch, canonical display
name, jurisdiction, and Remove action. Hover emphasizes the overlay. Clicking
the row fits it explicitly; adding one never moves the camera. Selecting an
active result pulses its existing row and performs no mutation.

There is no separate visibility toggle in the first version. An active overlay
is visible in Browse, Focus, and Edit; Remove is the visibility operation. This
keeps durable context independent of workbench mode and avoids another hidden
state axis.

## Completion

The interaction copies Booru Viewer's proven completion grammar:

- lookup begins after two normalized characters;
- `name` and `name, ST` are accepted;
- lookup runs on a serial background worker;
- prior suggestions remain visible until the newest serial publishes;
- Tab and Shift-Tab cycle, Enter accepts, Escape dismisses, and pointer click
  accepts;
- at most eight results appear;
- exact name and prefix matches outrank token matches, followed by overlap with
  project map areas, viewport distance, jurisdiction, and stable lexical order.

Rows read as `Yonkers · NY · city`; source identifiers never enter ordinary UI.
Fuzzy matching is deferred. A deterministic prefix/token index is sufficient
until playtesting proves misspelling recovery worth its cost.

## Acquisition

Acceptance creates an active row immediately in `Preparing boundary` state.
Acquisition, archive decoding, polygon normalization, spatial indexing,
tessellation, and persistence run off-thread under one generation identity.
The worker atomically publishes a prepared snapshot; failure leaves a compact
Retry/Remove fault row. Existing overlays and map presentation remain live.

One selected key may occur only once. Removal invalidates its worker generation
and drops project ownership; shared raw archives remain cache-managed.

## Rendering

Overlays use a translucent warm-bronze fill and a darker hatched boundary.
Intersections may darken through ordinary alpha composition; overlay count does
not allocate a unique categorical palette. Hover increases boundary weight and
fill opacity without changing hue.

Hatches are anchored in world coordinates and clipped to a narrow boundary
band. Polygon and boundary meshes have retained, nested levels of detail with
analytic zoom thresholds and hysteresis. Camera frames cull and submit prepared
meshes; they never triangulate, simplify, hash, or scan complete polygons.

The composition order is:

```text
basemap and relief
overlay fills
dead/live-area mask
overlay boundaries
walking context and privileged routes
labels and parking
controls
```

This preserves routes as the primary content and prevents hatches from striking
through labels or symbols.

## Evidence

A deterministic acceptance fixture contains a small catalog and two disjoint
polygons. One story types `yon`, selects by Tab and Enter, observes the
preparing row without a stalled frame, proves the polygon externally after
publication, adds a second overlay, removes the first, verifies that addition
did not move the viewport, restarts, and proves persistence. The story budgets
input-to-row acknowledgement and input-to-presented-overlay separately.

## Rejected Shortcuts

- Protomaps boundary tiles contain display strokes, not stable named polygon
  identities.
- Live Nominatim or equivalent search makes basic completion network-dependent
  and inherits unstable ranking and service limits.
- Nationwide full-resolution geometry in the executable is disproportionate;
  the compact catalog plus state archive cache gives offline search without
  shipping unused polygons.
- Cartographic 1:500,000 geometry is unsuitable for a smoothly zoomable route
  map.

## Checkpoints

1. Add the boundary types, project persistence, generated catalog fixture, and
   Census adapter.
2. Prove retained fill and hatched-border rendering against static polygons.
3. Add asynchronous completion and acquisition to the inspector.
4. Add the user-story acceptance test and responsiveness budgets, then
   playtest before admitting another boundary provider.
