# Mini Loop Demo

```sh
cargo run -p trailgen -- init demo/mini-loop --name "Mini Loop" --bbox -105.02,39.99,-104.98,40.02
cargo run -p trailgen -- discover demo/mini-loop
cargo run -p trailgen -- build demo/mini-loop --source crates/trailgen-core/tests/fixtures/mini_network.geojson
cargo run -p trailgen -- apply-elevation demo/mini-loop --source crates/trailgen-core/tests/fixtures/mini_dem.asc --confidence 0.81
cargo run -p trailgen -- apply-terrain demo/mini-loop --source crates/trailgen-core/tests/fixtures/terrain_overlay.geojson
cargo run -p trailgen -- apply-context demo/mini-loop --source crates/trailgen-core/tests/fixtures/context_overlay.geojson
cargo run -p trailgen -- import-seed demo/mini-loop --route demo/mini-loop/routes/candidate-1.gpx --name "Known Good Loop"
cargo run -p trailgen -- import-seed demo/mini-loop --route demo/mini-loop/routes/candidate-1.kmz --name "Known Good KMZ Loop"
cargo run -p trailgen -- apply-access demo/mini-loop --source crates/trailgen-core/tests/fixtures/closure_overlay.geojson --date 2026-05-15
cargo run -p trailgen -- verify-sources demo/mini-loop
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --count 4 --seed 0
cargo run -p trailgen -- export demo/mini-loop --route candidate-1 --format geojson --output demo/mini-loop/routes/candidate-1.selected.geojson
cargo run -p trailgen -- export demo/mini-loop --route candidate-1 --format csv --output demo/mini-loop/routes/candidate-1.selected.csv
cargo run -p trailgen -- report demo/mini-loop --route candidate-1 --output demo/mini-loop/reports/candidate-1.md
cargo run -p trailgen -- map demo/mini-loop --output demo/mini-loop/reports/map.html
cargo run -p trailgen -- rate demo/mini-loop --route demo/mini-loop/routes/candidate-1.kmz --output demo/mini-loop/reports/rated-candidate-1.md
cargo run -p trailgen -- calibrate demo/mini-loop --route demo/mini-loop/routes/candidate-1.gpx --target-difficulty 1020 --family elevation
cargo run -p trailgen -- rerate demo/mini-loop
```

Optional shape smoke:

```sh
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 1 --max-km 6 --shape out-and-back --max-repeated-edge-fraction 1 --count 2
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --max-ascent-m 50 --max-descent-m 50 --count 2
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --forbid-terrain pavement --min-terrain trail:0.50 --max-terrain talus=0.20 --count 2
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --max-restricted-access-fraction 0.05 --count 2
```

The fixture network, Arc/Info ASCII DEM, terrain overlay, dated seasonal closure, and road/hydrology context overlay are synthetic and public-domain-like; they exist to test topology, elevation enrichment, terrain overrides, dated access filtering, crossing inference, scoring, route export, and reporting without downloads.

`cache/graph.json`, `cache/graph.geojson`, `cache/edges.csv`, and `cache/vertices.csv` are regenerated together so the normalized graph has both machine-native and table/GIS inspection surfaces. `import-seed` archives supplied route files under `seeds/imports/` before fingerprinting them, so later generation can overwrite `routes/candidate-1.*` without mutating seed provenance. Apply access overlays after graph-building, enrichment, context, and seed imports: `sources/access-baseline.json` captures the pre-access graph state so dated closures can be re-materialized without cumulative drift. `sources/manifest.json` records both local source fingerprints and AOI-bound recommendations for the classes of data a real project should acquire. `generate` emits each candidate as GPX, GeoJSON, CSV, KML, KMZ, and a one-route Markdown report, plus aggregate route/report/map artifacts. `routes/generated.manifest.json` is the run ledger: app version, seed, requested/concrete solver, effective config, source manifest, graph summary, exact generated edge sequences, and emitted artifacts. `routes/generated.graph.json` snapshots the effective graph used by exports, selected reports, and maps for that generation run.
