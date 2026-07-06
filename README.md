# adequate-trailgen

`adequate-trailgen` is a CLI-first Rust workspace for designing long-day-hike loops over normalized trail graphs. It ingests provider-neutral route/network files, splits and snaps them into a routable graph, enriches each edge with transparent attributes, scores route difficulty, searches for constrained loop candidates, and exports route files plus reports.

The current implementation is intentionally local-first: use GeoJSON/GPX fixtures, official GIS exports, park layers, OSM-derived extracts, or user-supplied AllTrails exports. The internal model is provider-agnostic; AllTrails is treated as an import/export workflow, not as a privileged dependency.

`trailgen build` prefers provider-neutral GeoJSON, OSM XML/PBF, or shapefile trail/network layers and accepts repeated `--source` flags to merge multiple network files into one graph. It can also bootstrap a practical graph from supplied GPX, KML, KMZ, CSV, route GeoJSON, or route JSON files. Route-derived graphs preserve `route-file` provenance and lower confidence; use them as seed scaffolds when a real network layer is not yet available.

## Quickstart

```sh
cargo run -p trailgen -- init demo/mini-loop --name "Mini Loop" --bbox -105.02,39.99,-104.98,40.02
cargo run -p trailgen -- discover demo/mini-loop
cargo run -p trailgen -- build demo/mini-loop --source crates/trailgen-core/tests/fixtures/mini_network.geojson
cargo run -p trailgen -- apply-elevation demo/mini-loop --source crates/trailgen-core/tests/fixtures/mini_dem.asc --confidence 0.81
cargo run -p trailgen -- apply-terrain demo/mini-loop --source crates/trailgen-core/tests/fixtures/terrain_overlay.geojson
cargo run -p trailgen -- apply-context demo/mini-loop --source crates/trailgen-core/tests/fixtures/context_overlay.geojson
cargo run -p trailgen -- import-seed demo/mini-loop --route demo/mini-loop/routes/candidate-1.gpx --name "Known Good Loop"
cargo run -p trailgen -- apply-access demo/mini-loop --source crates/trailgen-core/tests/fixtures/access_overlay.geojson --source crates/trailgen-core/tests/fixtures/closure_overlay.geojson --date 2026-05-15
cargo run -p trailgen -- verify-sources demo/mini-loop
cargo run -p trailgen -- stats demo/mini-loop
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 4 --max-km 9 --count 4 --seed 0
cargo run -p trailgen -- export demo/mini-loop --route candidate-1 --format gpx --output /tmp/candidate-1.gpx --report-output /tmp/candidate-1.md
cargo run -p trailgen -- export demo/mini-loop --route candidate-1 --format csv --output /tmp/candidate-1.csv
cargo run -p trailgen -- report demo/mini-loop --output /tmp/generated.md
cargo run -p trailgen -- map demo/mini-loop --output /tmp/mini-loop-map.html
```

`trailgen assemble <project>` is the manifest-driven rebuild path: after `discover` or `cache-source` has populated `sources/manifest.json`, it verifies source fingerprints, builds from trail-network candidates or seed-route scaffolds, applies DEM, terrain, and road/hydrology candidates, imports seed routes, then applies access/closure candidates. The explicit `build`/`apply-*`/`import-seed` commands remain available when you want manual phase control or are experimenting with one source at a time.

For an exact small-graph search, add `--solver exact`; `--solver auto` uses the exact enumerator on small graphs and the sparse heuristic elsewhere. Use `--max-hops`, `--max-frontier`, and `--keep` to override the search envelope for one run without editing `[search]`. `--seed N` is recorded and also drives deterministic `LoopHunter` branch diversification, so the same source ledger/config/seed reproduces sparse-frontier choices. `trailgen formulate-milp <project> --start lon,lat --output loop.lp` writes a deterministic LP/MILP formulation for a connected simple loop through the snapped trailhead, with degree, flow-connectivity, distance, difficulty, ascent/descent, road, low-confidence, restricted-access, and terrain constraints. After solving that LP externally, `trailgen import-milp-solution <project> --start lon,lat --solution loop.sol` accepts selected `z_e{edge}_v{from}_v{to}` variables and writes the incumbent through the normal generated route, report, manifest, map, GPX, GeoJSON, CSV, KML, and KMZ artifact path. Use `--date YYYY-MM-DD` to record the planning date used with dated access/closure overlays. `generate --start lon,lat` snaps to the nearest graph vertex only within `max_start_snap_m` and records the snap in the manifest; use `--max-start-snap-m N` only for deliberately coarse trailhead coordinates. For an out-and-back search, run `generate` with `--shape out-and-back --max-repeated-edge-fraction 1`. For a tighter climbing window, add flags such as `--min-ascent-m 500 --max-ascent-m 1800 --max-descent-m 1800`. For terrain steering, use repeated flags such as `--forbid-terrain pavement --min-terrain trail:0.60 --max-terrain talus=0.10`. For a one-run avoid zone, pass repeated `--forbid-area closures.geojson`; each supplied GeoJSON or shapefile polygon/line overlay is forced closed in `routes/generated.graph.json`, fingerprinted in the generation manifest, and leaves the cached graph untouched. `--max-road-fraction` covers explicit road exposure plus terrain tagged `road` or `pavement`. Access restrictions are hard by default: `restricted`, `closed`, and `private` edges violate the route unless `--max-restricted-access-fraction` allows them.

Generated artifacts land in the project directory:

- `cache/graph.json`: normalized attributed graph
- `cache/graph.geojson`: edge geometry and attribution as GeoJSON
- `cache/edges.csv` and `cache/vertices.csv`: deterministic graph tables with WKT geometry for spreadsheet/GIS inspection
- `sources/manifest.json`: source adapter registry, AOI-bound acquisition recommendations, and discovered/used source files with byte counts and SHA-256 fingerprints
- `sources/discovery.md`: human-readable source coverage, acquisition plan, copyable cache command sketches, official/practical source hints, local candidates, and adapter registry
- `sources/elevation-arc-ascii.json`, `sources/elevation-geotiff.json`, `sources/elevation-vrt.json`, or `sources/elevation-mosaic.json`: applied local DEM sampler metadata
- `sources/terrain-overlays.json`: applied land-cover, surface, or user terrain overrides from GeoJSON or shapefile layers
- `sources/access-overlays.json`: composed access/closure overlays from every `apply-access --source`
- `sources/access-baseline.json`: pre-access graph state used to re-materialize dated overlays without cumulative access drift
- `sources/context-overlays.json`: applied road/hydrology context overlays from GeoJSON, shapefile, or OSM XML/PBF linework
- `routes/generated.geojson`: Pareto-ranked generated loops with persisted route scores, constraint penalties, constraint-audit margins, terrain/access fractions, access-warning edges, low-confidence and dubious segments, difficulty hotspots, and source provenance summaries
- `routes/generated.graph.json`: effective graph snapshot used by generated route exports, reports, and maps
- `routes/generated.manifest.json`: app version, random seed, requested/concrete solver, effective config, fingerprinted source manifest, one-run forbidden-area sources, graph summary, exact route edge sequences, and artifact list for reproducing a generation run
- `routes/candidate-*.geojson`: per-route GeoJSON exports with the same diagnostics as `routes/generated.geojson`
- `routes/candidate-*.gpx`: GPX exports
- `routes/candidate-*.csv`: lon/lat/elevation CSV exports
- `routes/candidate-*.kml`: KML exports
- `routes/candidate-*.kmz`: KMZ exports
- `routes/loop.lp`: optional LP/MILP loop formulation exported by `formulate-milp`
- `routes/loop.sol`: optional external solver output consumed by `import-milp-solution`
- `seeds/imports/`: archived copies of imported seed-route files
- `seeds/seeds.json`: imported seed routes snapped to the graph, with original source paths and route metadata retained for traceability
- `reports/generated.md`: route diagnostics, per-route constraint verdicts and margins, source provenance, the generation constraint envelope, and fingerprinted source manifest summary
- `reports/candidate-*.md`: one-route reports emitted during generation for direct handoff
- `reports/map.html`: self-contained interactive offline SVG map of graph terrain, edge difficulty, confidence, generated routes, constraint margins, and selected route/edge diagnostics

Use `trailgen export <project> --route candidate-1 --format gpx|geojson|csv|kml|kmz --output file [--report-output file.md]` to re-export a selected generated route after the search run, optionally with the same human-readable report sidecar emitted during generation. GeoJSON route exports carry the same route-level diagnostic fields as `routes/generated.geojson`. Use `trailgen report <project> [--route candidate-1] [--output file.md]` to render either all generated routes or one named route, including the constraint envelope used for that generation run. Use `trailgen map <project> [--output file.html]` to regenerate the interactive offline diagnostic map without rerunning the solver. These commands read `routes/generated.graph.json` when generated routes exist, so later cached-graph rerating or access-date changes do not silently reinterpret an old route.

Use `trailgen rate <project> --route completed.gpx [--output reports/completed.md]` to score a completed hike against the current graph and optionally persist the full diagnostic report. Route-file snapping is bounded by `max_route_snap_m`; use `--max-route-snap-m N` only for deliberately coarse tracks. Use `trailgen calibrate <project> --route completed.gpx --target-difficulty N --family elevation|technical|navigation` to dry-run a difficulty-weight patch from that hike; add `--write` to update `trailgen.toml` and rerate cached edge costs. `trailgen rerate <project>` reapplies hand-edited `[difficulty]` weights to every cached graph surface under `cache/`.

Use `trailgen cache-source <project> --input URL_OR_PATH --output trails.geojson` to copy or download a provider-neutral artifact into `sources/`, record its origin, fingerprint it, and add it to the source manifest. Local loose shapefile caching copies `.shp`, mandatory `.dbf`, optional `.shx`, `.prj`, and `.cpg` sidecars and fingerprints the bundle; local or remote zipped shapefile bundles can be cached with `--output name.shp`, extracting the matching or sole shapefile member. Add `--kind` and `--adapter` when filenames do not identify the source class and normalizer. `trailgen discover` writes `sources/manifest.json` and `sources/discovery.md`; recommendations include acquisition hints for official/practical source surfaces such as NPS/USFS GIS, USGS TNM/3DEP, MRLC/NLCD, PAD-US, NHD, OSM extracts, and user-supplied route archives, plus cache command sketches with the recommended local output path, kind, and adapter pinned.

For a bbox-scoped OSM fallback, `trailgen acquire-osm <project> --profile all|trails|roads|hydrology` posts an Overpass query for the project AOI, caches OSM XML under `sources/`, writes the exact `.overpassql` sidecar, validates the response against the implemented OSM adapters, fingerprints the cached XML, and registers matching trail-network, road-context, or hydrology-context candidates. `--profile all` writes one coherent XML extract and registers every source class the returned ways can satisfy. Use `--print-query` to audit the query before contacting an endpoint, or `--endpoint` to choose another Overpass instance.

Use `trailgen verify-sources <project>` before reproduction-sensitive generation runs; it recomputes source byte counts and SHA-256 hashes from `sources/manifest.json` and fails on missing, unfingerprinted, or drifted inputs. `trailgen stats <project>` audits graph terrain, access, source/provenance, confidence, seed attribution, elevation attribution, road/pavement exposure, and crossings. Generated and selected route reports include the same source manifest summary for human review.

Use `trailgen alltrails-status` to print the current manual AllTrails exchange contract. The machine-readable section exposes typed, verification-dated bridge plans for import, manual custom-route upload, manual activity upload, and the intentionally unsupported direct-write API.

See [docs/installation.md](docs/installation.md), [docs/model.md](docs/model.md), [docs/data-sources.md](docs/data-sources.md), [docs/source-adapters.md](docs/source-adapters.md), [docs/difficulty.md](docs/difficulty.md), [docs/optimizer.md](docs/optimizer.md), [docs/alltrails.md](docs/alltrails.md), and [docs/limitations.md](docs/limitations.md).

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
