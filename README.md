# adequate-trailgen

`adequate-trailgen` is a CLI-first Rust workspace for designing long-day-hike loops over normalized trail graphs. It ingests provider-neutral route/network files, splits and snaps them into a routable graph, enriches each edge with transparent attributes, scores route difficulty, searches for constrained loop candidates, and exports route files plus reports.

The current implementation is intentionally local-first: use GeoJSON/GPX fixtures, official GIS exports, park layers, OSM-derived extracts, or user-supplied AllTrails exports. The internal model is provider-agnostic; AllTrails is treated as an import/export workflow, not as a privileged dependency.

## Quickstart

```sh
cargo run -p trailgen -- init demo/mini-loop --name "Mini Loop" --bbox -105.02,39.99,-104.98,40.02
cargo run -p trailgen -- discover demo/mini-loop
cargo run -p trailgen -- build demo/mini-loop --source crates/trailgen-core/tests/fixtures/mini_network.geojson
cargo run -p trailgen -- apply-elevation demo/mini-loop --source crates/trailgen-core/tests/fixtures/mini_dem.asc --confidence 0.81
cargo run -p trailgen -- apply-terrain demo/mini-loop --source crates/trailgen-core/tests/fixtures/terrain_overlay.geojson
cargo run -p trailgen -- apply-context demo/mini-loop --source crates/trailgen-core/tests/fixtures/context_overlay.geojson
cargo run -p trailgen -- apply-access demo/mini-loop --source crates/trailgen-core/tests/fixtures/closure_overlay.geojson
cargo run -p trailgen -- import-seed demo/mini-loop --route demo/mini-loop/routes/candidate-1.gpx --name "Known Good Loop"
cargo run -p trailgen -- verify-sources demo/mini-loop
cargo run -p trailgen -- stats demo/mini-loop
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 4 --max-km 9 --count 4 --seed 0
cargo run -p trailgen -- export demo/mini-loop --route candidate-1 --format gpx --output /tmp/candidate-1.gpx
cargo run -p trailgen -- report demo/mini-loop --route candidate-1 --output /tmp/candidate-1.md
```

For an out-and-back search, run `generate` with `--shape out-and-back --max-repeated-edge-fraction 1`. For a tighter climbing window, add flags such as `--min-ascent-m 500 --max-ascent-m 1800 --max-descent-m 1800`. For terrain steering, use repeated flags such as `--forbid-terrain pavement --min-terrain trail:0.60 --max-terrain talus=0.10`.

Generated artifacts land in the project directory:

- `cache/graph.json`: normalized attributed graph
- `sources/manifest.json`: source adapter registry, AOI-bound acquisition recommendations, and discovered/used source files with byte counts and SHA-256 fingerprints
- `sources/elevation-arc-ascii.json`: applied local DEM sampler metadata
- `sources/terrain-overlays.json`: applied land-cover, surface, or user terrain overrides
- `sources/access-overlays.json`: applied access/closure overlays
- `sources/context-overlays.json`: applied road/hydrology context overlays
- `routes/generated.geojson`: Pareto-ranked generated loops with persisted route scores
- `routes/generated.manifest.json`: app version, random seed, effective config, fingerprinted source manifest, graph summary, exact route edge sequences, and artifact list for reproducing a generation run
- `routes/candidate-*.gpx`: GPX exports
- `routes/candidate-*.kml`: KML exports
- `routes/candidate-*.kmz`: KMZ exports
- `seeds/imports/`: archived copies of imported seed-route files
- `seeds/seeds.json`: imported seed routes snapped to the graph, with original source paths retained for traceability
- `reports/generated.md`: route diagnostics, the generation constraint envelope, and fingerprinted source manifest summary

Use `trailgen export <project> --route candidate-1 --format gpx|geojson|kml|kmz --output file` to re-export a selected generated route after the search run. Use `trailgen report <project> [--route candidate-1] [--output file.md]` to render either all generated routes or one named route, including the constraint envelope used for that generation run.

Use `trailgen cache-source <project> --input URL_OR_PATH --output trails.geojson` to copy or download a provider-neutral artifact into `sources/`, record its origin, fingerprint it, and add it to the source manifest. Add `--kind` and `--adapter` when filenames do not identify the source class.

Use `trailgen verify-sources <project>` before reproduction-sensitive generation runs; it recomputes source byte counts and SHA-256 hashes from `sources/manifest.json` and fails on missing, unfingerprinted, or drifted inputs. Generated and selected route reports include the same source manifest summary for human review.

See [docs/model.md](docs/model.md), [docs/data-sources.md](docs/data-sources.md), [docs/difficulty.md](docs/difficulty.md), [docs/optimizer.md](docs/optimizer.md), and [docs/alltrails.md](docs/alltrails.md).
