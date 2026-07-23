# Data Sources

## Automatic Corpus

The GUI owns trail acquisition. A project stores bronze-framed fetch rectangles in
`[trail_data].regions`; the routable corpus is the geometric union of those rectangles. The
default US provider batch is:

| Provider ID | Source | Role | License |
| --- | --- | --- | --- |
| `ny-state-parks` | NYS OPRHP State Parks Trails | Public, current, foot-permitted state-park routes with authoritative marked/unmarked, surface, lifecycle, and access fields | Informational, non-commercial; OPRHP attribution required |
| `texas-state-parks` | TPWD Texas State Parks Trails | Park-vetted official hiking geometry and use classification | TPWD informational public data; TPWD attribution |
| `osm` | OpenStreetMap through bounded Overpass queries | Broad geometry, trail standing, access, route relations, road connectors, and nearby road/water context | ODbL 1.0 |
| `usgs-national-trails` | USGS National Digital Trails | Official-agency trail geometry and source-originator evidence | USGS public domain |

Each region-provider pair has its own immutable raw shard, exact request sidecar, and fingerprinted
receipt beneath `sources/<provider>/`. A damaged derived index rebuilds from those shards. A missing,
drifted, or obsolete provider receipt refetches only that provider and rectangle. `cache/graph.json`
is written last and is the GUI readiness marker. State providers carry explicit geographic coverage;
an irrelevant rectangle receives a deterministic empty receipt without a network request.

All providers normalize through `NetworkProvider` into `SegmentDraft` and `ContextOverlay`; no
provider owns a second graph-building path. Normalized lines are clipped to the rectangle union,
then conflated explicitly. Lower numeric precedence wins duplicate geometry, while corroborating
provenance and useful missing attributes survive. Residual nonparallel geometry from a lower-priority
provider remains routable. `cache/conflation.json` records every bounded suppression decision;
`cache/trails.json` keeps only compact counts.

State authority geometry has precedence 0, OSM precedence 10, and USGS precedence 20. An authority's
unknown attribute is still filled by a corroborating lower stratum, so TPWD geometry can inherit an
OSM closure or blaze without surrendering its identity. OPRHP's explicit lifecycle and access values
remain authoritative.

Elevation follows the same acquisition law without pretending to be a trail-network provider. The
GUI downloads the rectangle union's Mapzen Terrarium PNGs from the AWS Open Data terrain bucket at
the deepest common zoom that stays at or below 256 tiles. Tiles live beneath
`sources/mapzen-terrain/<z>/<x>/<y>.png`; `cache/trails.json` records their fingerprints and
coordinates, so damaged tiles are reacquired and an index cannot silently reuse different terrain.
The graph is densified and sampled only after network conflation. Implausible void or bathymetric
pixels below -150 m or above 9,000 m become missing evidence rather than catastrophic grade spikes.
This is bare-earth terrain: bridge decks and other elevated structures may still require explicit
elevation evidence.

The USGS adapter queries the Transportation service's National Digital Trails layer for terrestrial,
hiker/pedestrian trails. It preserves permanent or source feature identity, source originator,
dataset identity, surface, and public-domain provenance. USGS inclusion does not prove public access,
so its edges deliberately carry `access = unknown`.

The OPRHP adapter queries only public, foot-permitted, non-proposed lines. Its native subtype law is
explicit: 0 unmarked trail, 1 marked trail, 2 unpaved road, 3 paved road, and 4 sidewalk. Closed
geometry is retained as established but non-routable evidence. Provider IDs, facility and region,
surface, access, and the current OPRHP use terms survive in provenance. This provider is enabled for
the repository's unpublished non-commercial dogfooding; publication needs the planned terms and
attribution surface.

The TPWD adapter queries only `Official = Yes` lines whose use includes hiking. TPWD does not publish
a live closure field in this layer, so access remains unknown and may be filled by corroborating
evidence rather than fabricated as open. Provider IDs, park identity, and TPWD attribution survive.

The OSM adapter accepts path, non-sidewalk footway, track, steps, and bridleway geometry as trail
evidence. A street is never a trail merely because walking is legal there: service, pedestrian, and
road segments survive only when they form the nearest bridge of at most 1 km between two genuine
trail junctions. The unpruned road layer remains sequestered as context for crossings and exposure.
Hiking, foot, and walking route relations raise evidence; simple node-via foot turn restrictions
become graph turn bans. `foot`/`access`, direction, surface, trail visibility, wayfinding marks,
maintenance, `informal`, `disused`, and `abandoned` tags remain semantic input rather than display
trivia.

## Standing, Marking, And Access

`TrailStanding` and `Access` answer different questions. Standing describes what sort of path the
source claims exists:

- `established`: current ordinary trail or connector
- `unmaintained`: current but faded, badly visible, or explicitly unmaintained
- `informal`: an OSM `informal=yes` path, rendered in the GUI as an **Informal / YOLO** path
- `historical`: abandoned trail geometry retained as uncertain evidence
- `unknown`: the provider did not establish standing

Access separately records `open`, `restricted`, `closed`, `private`, or `unknown`. The GUI uses a
dual stroke so terrain/class remains legible while standing and access remain visible. Search may
surface informal or unmaintained geometry; it never silently promotes that standing to an access
claim.

`TrailMarking` independently records `marked`, `unmarked`, or `unknown`. OSM `trailblazed` evidence
and hiking-route membership establish marking; authoritative fields such as NYS `Asset = Unmarked
Trail` survive GeoJSON and shapefile ingestion. Unmarked lines use a deliberately sparse dotted core
and add navigation pressure without being mislabeled informal, unmaintained, closed, or pathless.

## Provider Contract

A new automatic source implements one boundary:

```rust
pub trait NetworkProvider {
    fn descriptor(&self) -> ProviderDescriptor;
    fn covers(&self, bounds: GeoBounds) -> bool;
    fn acquire(&self, bounds: GeoBounds) -> anyhow::Result<ProviderPayload>;
    fn normalize(&self, shards: &[RawShard<'_>]) -> anyhow::Result<NormalizedNetwork>;
}
```

The descriptor owns a path-safe ID, adapter revision, precedence, extensions, and label. `covers`
defaults to nationwide applicability and lets regional authorities reject foreign rectangles without
contacting their servers. Acquisition must be bounded and return raw bytes, the exact request, and an
origin. Normalization must preserve provider identity and licensing in provenance. Provider-native
types stop at this boundary;
`TrailGraph`, routing, the library, and the GUI remain provider-neutral. Adapter revision changes
invalidate only that provider's receipts.

## Debug Frontend

The CLI is a diagnostic frontend over the same engine:

```sh
trailgen survey PROJECT --place "Harriman State Park, NY" --radius-km 20
trailgen coverage PROJECT --route owned-route.csv --max-snap-m 40 --output coverage.json
trailgen stats PROJECT
```

`survey` uses US-restricted Nominatim only to turn a place into one rectangle. The GUI normally skips
place lookup and lets the user draw exact live regions. `coverage` reports remote geometry and true
topological discontinuities separately; it does not mutate the graph.

Lower-level development commands remain available for explicit source work:

- `acquire-osm` prints or caches a bounded trails, roads, hydrology, or combined Overpass query.
- `cache-source`, `discover`, `source-plan`, `verify-sources`, and `vet-sources` manage inspectable
  local candidates and content fingerprints.
- `build` accepts GeoJSON, OSM XML/PBF, or shapefile networks and GPX, GeoJSON/JSON, KML/KMZ, or CSV
  route scaffolds.
- `apply-elevation`, `apply-terrain`, `apply-context`, and `apply-access` enrich one graph rather than
  creating parallel products.
- `assemble` deterministically realizes the registered source manifest.

Vector adapters accept geographic WGS84/NAD83/CRS84, declared EPSG:3857, and WGS84/NAD83 UTM
(EPSG:326xx/327xx/269xx). Other projected coordinate systems fail with a reproject-first diagnostic.
Elevation accepts Arc/Info ASCII Grid, the supported affine GeoTIFF projections, and VRT wrappers.

Every consumed source has byte count and SHA-256 identity in `sources/manifest.json`. Generation
snapshots that manifest, its coverage summary, the effective graph, constraints, solver, start snap,
seed ledger, and emitted artifact fingerprints. `verify-generation` replays those laws rather than
trusting filenames.

## Personal And Historical Data

User-supplied route exports, including AllTrails exports, enter only through documented file formats.
Trailgen neither reads nor writes private AllTrails APIs. Personal traces are useful seed routes and
low-confidence graph scaffolds, not public-source authority.

Historical OSM is intentionally opt-in forensic material. An Overpass attic probe of Harriman from
2020 covered the owned South Lows trace materially worse than current OSM, so the application does
not fetch history by default merely to increase line count.

## Harriman Evidence

The owned fixtures `harriman-south-lows.csv` and `harriman-west.csv` are deterministic end-to-end
solver replays over route-derived graphs. A 2026-07-21 public-source probe additionally matched every
source segment of both traces to the current OSM graph within 40 m and found no disconnected
transition; maximum geometric separation was 2.74 m for South Lows and 5.23 m for West. USGS
enriched the wider Harriman corpus but did not improve either owned trace. Reproduction from the
dense public graph remains a separate support-point test obligation; scalar search constraints do
not identify either custom route.

The [Harriman source audit](harriman-source-audit.md) identifies every anonymous interval and traces
the important near-bushwhack lines to OSM history, official NYS unmarked-trail data, and independent
field descriptions. Its central semantic finding is now encoded directly: wayfinding marking is
independent of standing, maintenance, and access.

The [US public trail source census](../notes/us-public-trail-source-census.md) catalogs all state
authorities and the principal federal land managers. Catalog presence is intentionally separate from
automatic-provider admission: licensing, lifecycle filtering, field semantics, and coverage evidence
must be proved first.

See the [USGS access guide](https://www.usgs.gov/national-digital-trails/how-access-or-view-usgs-trails-dataset),
[USGS dataset Q&A](https://www.usgs.gov/national-digital-trails/qas-about-usgs-trail-data),
[USGS Transportation service](https://cartowfs.nationalmap.gov/arcgis/rest/services/transportation/MapServer),
[AWS Open Data terrain registry](https://registry.opendata.aws/terrain-tiles/),
[Mapzen terrain attribution](https://github.com/tilezen/joerd/blob/master/docs/attribution.md),
and [OpenStreetMap copyright and attribution](https://www.openstreetmap.org/copyright/en-US).
