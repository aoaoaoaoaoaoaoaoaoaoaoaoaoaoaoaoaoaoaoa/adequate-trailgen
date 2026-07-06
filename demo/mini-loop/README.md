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
cargo run -p trailgen -- apply-access demo/mini-loop --source crates/trailgen-core/tests/fixtures/access_overlay.geojson --source crates/trailgen-core/tests/fixtures/closure_overlay.geojson --date 2026-05-15
cargo run -p trailgen -- verify-sources demo/mini-loop
cargo run -p trailgen -- vet-sources demo/mini-loop --level recommended
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --count 4 --seed 0 --source-gate recommended
cargo run -p trailgen -- verify-generation demo/mini-loop
cargo run -p trailgen -- export demo/mini-loop --route candidate-1 --format geojson --output demo/mini-loop/routes/candidate-1.selected.geojson --report-output demo/mini-loop/reports/candidate-1.selected.md
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
cargo run -p trailgen -- formulate-milp demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --output demo/mini-loop/routes/loop.lp
cargo run -p trailgen -- import-milp-solution demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --solution demo/mini-loop/routes/loop.sol
```

The fixture network, Arc/Info ASCII DEM, terrain overlay, ownership/access overlay, dated seasonal closure, and road/hydrology context overlay are synthetic and public-domain-like; they exist to test topology, elevation enrichment, terrain overrides, dated access filtering, crossing inference, scoring, route export, and reporting without downloads.

`cache/graph.json`, `cache/graph.geojson`, `cache/edges.csv`, and `cache/vertices.csv` are regenerated together so the normalized graph has both machine-native and table/GIS inspection surfaces. `import-seed` archives supplied route files under `seeds/imports/` before fingerprinting them, so later generation can overwrite `routes/candidate-1.*` without mutating seed provenance. Apply access overlays after graph-building, enrichment, context, and seed imports: `sources/access-baseline.json` captures the pre-access graph state so ownership restrictions and dated closures can be re-materialized together without cumulative drift. `sources/manifest.json` records both local source fingerprints and AOI-bound recommendations for the classes of data a real project should acquire; `sources/discovery.md` renders that manifest as a human acquisition plan with copyable cache command sketches and combined or per-class `acquire-osm` sketches for bbox-scoped Overpass XML trail, road, and hydrology fallbacks. `vet-sources --level recommended` proves every required and recommended source class in this fixture is backed by fingerprinted bytes before generation; `generate --source-gate recommended` repeats that check inside the artifact-producing command and records the policy in the manifest. `generate` emits each candidate as GPX, GeoJSON, CSV, KML, KMZ, and a one-route Markdown report with explicit constraint-audit margins; GeoJSON carries structured route diagnostics, while GPX/KML/KMZ descriptions and CSV comment headers carry the compact score/rank/constraint summary. `verify-generation` recomputes emitted artifact fingerprints, run metadata, seed-ledger state, snapshotted source fingerprints, and manifest route edge walks, then replays metrics, verdicts, scores, Pareto ranks, and native solver output from the generated graph, effective constraints, snapped start, seed, and seed-route ledger to catch post-run drift. `formulate-milp` emits `routes/loop.lp`, a deterministic connected-loop LP/MILP formulation for external solver experiments; after an external solver writes selected `z_e{edge}_v{from}_v{to}` variables to `routes/loop.sol`, `import-milp-solution` imports that incumbent through the normal generated artifact path. The offline map lets route clicks dim rivals, expose constraint margins, and inspect edge diagnostics without network tiles. `routes/generated.manifest.json` is the run ledger: app version, seed, requested/concrete solver, requested/snapped start, effective config, source-manifest snapshot, coverage summary, seed-route ledger fingerprint, graph topology/elevation summary, directed-travel edge count, turn-ban count/provenance, exact generated edge sequences, emitted artifact list, and artifact fingerprints. `routes/generated.graph.json` snapshots the effective graph used by exports, selected reports, and maps for that generation run.
