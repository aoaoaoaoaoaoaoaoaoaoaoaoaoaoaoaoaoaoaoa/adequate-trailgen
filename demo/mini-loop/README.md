# Mini Loop Demo

```sh
cargo run -p trailgen -- init demo/mini-loop --name "Mini Loop" --bbox -105.02,39.99,-104.98,40.02
cargo run -p trailgen -- discover demo/mini-loop
cargo run -p trailgen -- build demo/mini-loop --source crates/trailgen-core/tests/fixtures/mini_network.geojson
cargo run -p trailgen -- apply-elevation demo/mini-loop --source crates/trailgen-core/tests/fixtures/mini_dem.asc --confidence 0.81
cargo run -p trailgen -- apply-terrain demo/mini-loop --source crates/trailgen-core/tests/fixtures/terrain_overlay.geojson
cargo run -p trailgen -- apply-context demo/mini-loop --source crates/trailgen-core/tests/fixtures/context_overlay.geojson
cargo run -p trailgen -- apply-access demo/mini-loop --source crates/trailgen-core/tests/fixtures/closure_overlay.geojson
cargo run -p trailgen -- import-seed demo/mini-loop --route demo/mini-loop/routes/candidate-1.gpx --name "Known Good Loop"
cargo run -p trailgen -- import-seed demo/mini-loop --route demo/mini-loop/routes/candidate-1.kmz --name "Known Good KMZ Loop"
cargo run -p trailgen -- verify-sources demo/mini-loop
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --count 4 --seed 0
cargo run -p trailgen -- export demo/mini-loop --route candidate-1 --format geojson --output demo/mini-loop/routes/candidate-1.selected.geojson
cargo run -p trailgen -- report demo/mini-loop --route candidate-1 --output demo/mini-loop/reports/candidate-1.md
cargo run -p trailgen -- rate demo/mini-loop --route demo/mini-loop/routes/candidate-1.kmz
```

Optional shape smoke:

```sh
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 1 --max-km 6 --shape out-and-back --max-repeated-edge-fraction 1 --count 2
cargo run -p trailgen -- generate demo/mini-loop --start=-105.0000,40.0000 --min-km 3 --max-km 8 --max-ascent-m 50 --max-descent-m 50 --count 2
```

The fixture network, Arc/Info ASCII DEM, terrain overlay, and road/hydrology context overlay are synthetic and public-domain-like; they exist to test topology, elevation enrichment, terrain overrides, crossing inference, scoring, route export, and reporting without downloads.

`import-seed` archives supplied route files under `seeds/imports/` before fingerprinting them, so later generation can overwrite `routes/candidate-1.*` without mutating seed provenance. `sources/manifest.json` records both local source fingerprints and AOI-bound recommendations for the classes of data a real project should acquire. `routes/generated.manifest.json` is the run ledger: app version, seed, effective config, source manifest, graph summary, exact generated edge sequences, and emitted artifacts.
