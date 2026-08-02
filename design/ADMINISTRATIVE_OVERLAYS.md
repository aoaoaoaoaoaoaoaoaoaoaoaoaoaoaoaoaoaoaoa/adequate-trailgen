# Civic Area Overlays

Status: MVP contract.

## Purpose

The feature answers one question while planning a trail: where is the edge of a
named civic area? Its proving use is a walk around Brooklyn. It does not alter
routing, trail acquisition, search boundaries, or the active workbench view.

The UI calls the inspector section `Overlays`. The implementation calls one
selected region a `CivicArea`; `Overlay` already names graph annotations and
`Boundary` already names the finder constraint.

## Search Space

The catalog contains United States Census Places, including incorporated
places and census-designated places, plus the five New York City boroughs.
Generic counties and larger geographies are excluded. The NYC records are
boroughs even though four share county boundaries; county ontology does not
leak into search or presentation.

Autocomplete uses a release-generated, bundled Census catalog. Typing never
requires the network. Two normalized characters admit suggestions. Exact and
prefix matches precede token matches, then project proximity and lexical order.
At most eight suggestions are presented; this is a completion-space bound, not
a cap on active civic areas.

## Identity And Ownership

```text
CivicKey { source, geoid }
CivicRecord { key, name, kind, jurisdiction, anchor }
CivicSnapshot { record, rings, bounds, provenance }
```

Dataset vintage belongs to snapshot provenance, not identity. The project owns
an ordered index of active records and a normalized geometry snapshot for each
ready record. The snapshot contains enough display metadata and geometry to
survive catalog changes and offline restart. XDG state owns only the `Overlays`
shutter. Query text, suggestions, selection, progress, faults, hover, and
prepared render projections are session state.

## Acquisition

Accepting a suggestion creates its inspector row immediately in `Preparing`
state. One serial worker fetches and decodes authoritative GeoJSON, normalizes
polygon rings, prepares retained levels of detail, persists the snapshot
atomically, and publishes one immutable projection. Existing map content and
civic areas remain usable throughout. A fault row offers Retry and Remove.

Census Places resolve through the current Census TIGERweb incorporated-place
or census-designated-place feature layer. NYC boroughs resolve through the New
York City Department of City Planning Borough Boundaries feature service.
Raw provider payloads are not durable project state.

Removal invalidates a pending generation and deletes project ownership. There
is no refresh command in the MVP; a second source vintage must first prove the
need and migration law.

## Inspector

`Overlays` follows `Map Areas`. It contains one completion field and the active
civic-area rows. Rows show canonical name, jurisdiction, kind, state, and
Remove. Clicking a ready row explicitly fits its bounds; adding one never moves
the camera. Selecting an already-active suggestion identifies its row without
duplicating it.

Active civic areas remain visible in Browse, Focus, and Edit. Remove is the
visibility operation. There is no mode selector, palette editor, opacity axis,
or arbitrary active-area cap.

## Rendering

A civic area renders only as a thin magenta boundary with a darker supporting
stroke and world-anchored hatches. Retained nested simplifications and spatial
chunks keep frame work proportional to visible geometry. Camera frames may
cull, transform, and submit visible prepared chunks; they may not simplify,
decode, hash, or scan a complete detailed polygon.

Each area paints exactly one name whenever any part of its boundary intersects
the viewport. The label is chosen from the visible boundary and may slide as
the viewport changes. Civic labels are deliberately exempt from fixed-world
label anchoring because their contract is continuous visible identification,
not cross-scale cartographic stability.

Composition is:

```text
basemap and relief
walking context
dead/live-area mask
civic boundary
privileged routes
ordinary labels and parking
civic name
controls
```

Routes remain primary; labels and symbols cannot be struck through.

## Evidence

A deterministic fixture supplies civic GeoJSON through the existing private
provider server. The acceptance story completes Brooklyn borough by keyboard,
observes immediate preparation without a stalled frame, proves the rendered
boundary after publication, verifies that addition did not move the viewport,
fits it explicitly, restarts offline, and proves persistence.
Input-to-row acknowledgement and input-to-presented-boundary have separate
budgets.

## Sources

- [Census TIGERweb incorporated places](https://tigerweb.geo.census.gov/arcgis/rest/services/TIGERweb/tigerWMS_Current/MapServer/28)
- [Census TIGERweb census-designated places](https://tigerweb.geo.census.gov/arcgis/rest/services/TIGERweb/tigerWMS_Current/MapServer/30)
- [NYC Department of City Planning Borough Boundaries](https://services5.arcgis.com/GfwWNkhOj9bNBqoJ/arcgis/rest/services/NYC_Borough_Boundary/FeatureServer/0)

## Deferred

Neighborhoods, parks, districts, generic counties, states, federal geographies,
provider selection, refresh, per-area styling, visibility matrices, and
arbitrary import are outside the MVP.
