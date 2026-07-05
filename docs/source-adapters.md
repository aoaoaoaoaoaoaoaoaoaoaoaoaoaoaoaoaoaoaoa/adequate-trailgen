# Source Adapters

Source adapters live at the perimeter. Their job is to turn provider-native bytes into provider-neutral graph drafts, overlays, elevation samplers, or seed routes while preserving provenance and confidence. Provider quirks must not leak into `TrailGraph`, `Route`, `LoopConstraints`, or the optimizer.

The current registry is `adapter_registry()` in `crates/trailgen-core/src/source.rs`. Add a new adapter there with:

- stable `id`
- `SourceKind`
- implemented/planned status
- consumed file extensions or protocol names
- produced normalized artifacts
- a terse note about scope and limitations

If filenames can identify the source class, update `classify_path()` in the same module. If the adapter requires an explicit CLI command, add it in `crates/trailgen-cli/src/main.rs`, cache or copy source bytes under `project/sources/`, fingerprint the cached artifact with `SourceFingerprint`, then register a `SourceCandidate` in `sources/manifest.json`.

Shapefile adapters must treat `.shp`, `.dbf`, and `.shx` as one source bundle where applicable. The DBF table is semantic input, not metadata garnish: cache and verification code must copy/hash sidecars so attribute drift changes the manifest fingerprint.

Normalization targets:

- trail networks become `SegmentDraft` values and are built by `GraphBuilder`
- route files may also become low-confidence `SegmentDraft` scaffolds for `trailgen build` when no network layer is available
- route JSON adapters should stay structural and provider-neutral: accept coordinate arrays or point objects, not opaque private API documents
- seed-route adapters may preserve only provider-neutral metadata fields: title, description, recorded timestamp, and activity type
- terrain/access/context layers become overlay structs in `overlay.rs`
- elevation sources implement `ElevationSampler`
- user routes become `LineString` or `SeedRoute`

Adapter invariants:

- never crown a provider as authoritative inside core types
- validate advertised CRS metadata; vector geometries must be lon/lat WGS84/CRS84 unless a real reprojection adapter is added
- keep source provenance on every derived edge attribute
- attach confidence to inferred or transformed attributes
- fail on unsupported shapes or ambiguous units instead of inventing precision
- add fixture-backed tests that run without network access
- update `docs/data-sources.md` and the demo if the adapter changes user workflow

A good adapter test proves both parsing and downstream effect: for example, a closure overlay should parse active dates, set `Access::Closed` only on the planning date it covers, record access provenance, lower confidence if appropriate, and change route constraint verdicts.
