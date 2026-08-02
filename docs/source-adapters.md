# Source Adapters

Source adapters live at the perimeter. Their job is to turn provider-native bytes into provider-neutral graph drafts, overlays, elevation samplers, or seed routes while preserving provenance and confidence. Provider quirks must not leak into `WalkGraph`, `Route`, `LoopConstraints`, or the optimizer.

The current registry is `adapter_registry()` in `crates/trailgen-core/src/source.rs`. Add a new adapter there with:

- stable `id`
- `SourceKind`
- consumed file extensions or protocol names
- produced normalized artifacts
- a terse note about scope and limitations

If filenames can identify the source class, update `classify_path()` in the same module. Product acquisition belongs in `trailgen-data`: sequester raw bytes beneath `sources/`, fingerprint them, validate them against the target normalizer, and retain enough request and protocol context for audit. Surface the workflow through the GUI and its native acceptance story. Do not create an adapter-specific CLI orchestration path.

Shapefile adapters must treat `.shp`, `.dbf`, `.shx`, `.prj`, and `.cpg` as one source bundle where applicable. The DBF table and CRS sidecars are semantic input, not metadata garnish: cache and verification code must copy/hash sidecars so attribute or projection drift changes the manifest fingerprint. Zipped official downloads should be materialized into explicit cached artifacts under `sources/`, not kept as opaque archives; shapefile ZIPs become loose sidecars, while non-shapefile ZIPs must extract the requested source member by output filename or sole matching extension.

Normalization targets:

- trail networks become `SegmentDraft` values and are built by `GraphBuilder`
- explicit `way_kind=bushwhack` lines (aliases: `bushwhacking`, `off-trail`, `offtrail`, and `cross-country`) become pathless but routable drafts. Keep surrounding ground in `terrain` and the literal ground description in `surface`; do not encode a bushwhack as an informal path or forest terrain
- OSM XML/PBF network adapters should stay way-focused unless relation semantics are explicitly modeled: normalize walkable ways, preserve way IDs, access, foot direction, surface, confidence, ODbL provenance, hiking-route relation evidence, and simple via-node turn restrictions as graph-level directed turn bans
- route-file readers may produce low-confidence `SegmentDraft` scaffolds for bounded analysis, but they do not define a public project-construction workflow
- route JSON adapters should stay structural and provider-neutral: accept coordinate arrays or point objects, not opaque private API documents
- seed-route adapters may preserve only provider-neutral metadata fields: title, description, recorded timestamp, and activity type
- terrain/access/context layers become overlay structs in `overlay.rs`
- elevation sources implement `ElevationSampler`
- user routes become `LineString` or `SeedRoute`

Adapter invariants:

- never crown a provider as authoritative inside core types
- validate advertised CRS metadata; vector geometries must normalize to WGS84-compatible lon/lat at the adapter boundary. Native WGS84/NAD83/CRS84, declared EPSG:3857 Web Mercator, and WGS84/NAD83 UTM (EPSG:326xx/327xx/269xx) are implemented for vectors; GeoTIFF and VRT DEMs accept affine WGS84/NAD83 degrees, EPSG:3857 metres, or WGS84/NAD83 UTM metres, including rotated/sheared transforms where advertised. Other projected CRS require an explicit reprojection adapter rather than silent ingestion
- keep source provenance on every derived edge attribute
- attach confidence to inferred or transformed attributes
- fail on unsupported shapes or ambiguous units instead of inventing precision
- add fixture-backed tests that run without network access
- update `docs/data-sources.md` and the demo if the adapter changes user workflow

A good adapter test proves both parsing and downstream effect: for example, a closure overlay should parse active dates, weekdays, hours, and direction, set `Access::Closed` or a directed `EdgeTravel` only on the planning moment it covers, record access provenance, lower confidence if appropriate, and change route constraint verdicts.
