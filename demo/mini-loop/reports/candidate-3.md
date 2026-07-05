# Generated Hiking Route

## candidate-3

- score: 3908.74
- pareto rank: 2
- shape: Loop
- distance: 4.71 km
- ascent/descent: 137 m / 137 m
- scalar difficulty: 1014.07
- road/pavement exposure: 1.3%
- low-confidence fraction: 0.0%
- restricted-access fraction: 28.3%
- repeated-edge fraction: 0.0%
- constraint verdict: violated

Violations:
- difficulty 1014.07 above maximum 90.00
- restricted-access fraction 28.3% above maximum 0.0%

Difficulty decomposition:
- access: 1000.00 (98.6%)
- distance: 4.71 (0.5%)
- technical: 3.18 (0.3%)
- terrain: 1.73 (0.2%)
- ascent: 1.64 (0.2%)
- confidence: 1.08 (0.1%)
- navigation: 0.66 (0.1%)
- grade: 0.53 (0.1%)
- descent: 0.41 (0.0%)
- road: 0.12 (0.0%)

Access mix:
- Open: 71.7%
- Closed: 28.3%

Access warnings:
- edge 3: Closed, confidence 93%, provenance fixture-closure:diagonal-closure

Crossings:
- Road: 2
- Water: 2

Terrain mix:
- Trail: 43.4%
- Talus: 56.6%

Source provenance:
- fixture:east: 1.33 km (28.3%)
- fixture:north: 1.02 km (21.7%)
- fixture:south: 1.02 km (21.7%)
- fixture:west: 1.33 km (28.3%)

Largest difficulty contributors:
- edge 3 access: 1000.00 (98.6% of route), Talus, 1334 m
- edge 3 technical: 1.59 (0.2% of route), Talus, 1334 m
- edge 1 technical: 1.59 (0.2% of route), Talus, 1334 m
- edge 3 ascent: 1.36 (0.1% of route), Talus, 1334 m
- edge 3 distance: 1.33 (0.1% of route), Talus, 1334 m

Most dubious segments:
- edge 3: 1334 m, Talus, surface unknown, grade max 9.0%, grade bins flat 0% / rolling 100% / steep 0% / savage 0%, crossings 1, confidence 0.82, seed count 2, provenance fixture:west
- edge 0: 1022 m, Trail, surface unknown, grade max 4.0%, grade bins flat 100% / rolling 0% / steep 0% / savage 0%, crossings 1, confidence 0.82, seed count 2, provenance fixture:north
- edge 1: 1334 m, Talus, surface unknown, grade max 9.0%, grade bins flat 0% / rolling 100% / steep 0% / savage 0%, crossings 1, confidence 0.82, seed count 2, provenance fixture:east
- edge 2: 1022 m, Trail, surface unknown, grade max 2.3%, grade bins flat 100% / rolling 0% / steep 0% / savage 0%, crossings 1, confidence 0.82, seed count 2, provenance fixture:south

Terrain/elevation evidence:
- edge 3: current Talus; terrain evidence Talus 90%: explicit source terrain tag (fixture:west); surface unknown; elevation sources arc-ascii-grid:mini_dem.asc
- edge 0: current Trail; terrain evidence Trail 90%: explicit source terrain tag (fixture:north); surface unknown; elevation sources arc-ascii-grid:mini_dem.asc
- edge 1: current Talus; terrain evidence Forest 90%: explicit source terrain tag (fixture:east) | Talus 87%: terrain overlay (fixture-landcover:talus-basin); surface unknown; elevation sources arc-ascii-grid:mini_dem.asc
- edge 2: current Trail; terrain evidence Trail 90%: explicit source terrain tag (fixture:south); surface unknown; elevation sources arc-ascii-grid:mini_dem.asc

## Constraint Envelope

- distance: 3.00–8.00 km
- scalar difficulty: 0.00–90.00
- ascent: 0–3000 m
- descent: 0–3000 m
- max road/pavement exposure: 12.0%
- max low-confidence fraction: 20.0%
- max restricted-access fraction: 0.0%
- max repeated-edge fraction: 0.0%
- allowed shapes: [Loop]

## Source Manifest

Coverage:
- TrailNetwork (Required): Satisfied; candidates: crates/trailgen-core/tests/fixtures/mini_network.geojson; TrailNetwork source requirement has implemented candidate(s).
- Elevation (Required): Satisfied; candidates: crates/trailgen-core/tests/fixtures/mini_dem.asc; Elevation source requirement has implemented candidate(s).
- Terrain (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/terrain_overlay.geojson; Terrain source requirement has implemented candidate(s).
- Closure (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/closure_overlay.geojson; Closure source requirement has implemented candidate(s).
- Access (Recommended): Missing; candidates: none; Access source is Recommended; acquire one of sources/access.geojson, sources/ownership.geojson, sources/access.shp.
- Road (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/context_overlay.geojson; Road source requirement has implemented candidate(s).
- Hydrology (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/context_overlay.geojson; Hydrology source requirement has implemented candidate(s).
- SeedRoute (Optional): Satisfied; candidates: demo/mini-loop/seeds/imports/known-good-kmz-loop.kmz, demo/mini-loop/seeds/imports/known-good-loop.gpx; SeedRoute source requirement has implemented candidate(s).

Candidates:
- crates/trailgen-core/tests/fixtures/closure_overlay.geojson: Closure via geojson-closure-overlay; 610 bytes, sha256 75032f041c59c46efb0f01802a4cc234b6756a5e7eed2dd79ba1e69fa355aa02
- crates/trailgen-core/tests/fixtures/context_overlay.geojson: Road via geojson-road-context; 694 bytes, sha256 5aace6064e9fb87161fc95d71183e5d639f8eee46b86c3b3c084eba09f0171bd
- crates/trailgen-core/tests/fixtures/context_overlay.geojson: Hydrology via geojson-hydrology-context; 694 bytes, sha256 5aace6064e9fb87161fc95d71183e5d639f8eee46b86c3b3c084eba09f0171bd
- crates/trailgen-core/tests/fixtures/mini_dem.asc: Elevation via arc-ascii-elevation; 169 bytes, sha256 8200f8e889b76ecff72d845148d2476a5b4f75edbf782e6082a5e300a54e0859
- crates/trailgen-core/tests/fixtures/mini_network.geojson: TrailNetwork via geojson-network; 1714 bytes, sha256 eae6f4f939c209fb1ab455e581d4e50f7dffab68d7a6d7f4c9800e65fec64857
- crates/trailgen-core/tests/fixtures/terrain_overlay.geojson: Terrain via geojson-terrain-overlay; 535 bytes, sha256 2136c1627f1898ed1d6524f923e2a1e4edbcaab9cd6058a0cccebb3dfd62e936
- demo/mini-loop/seeds/imports/known-good-kmz-loop.kmz: SeedRoute via kml-route; 2040 bytes, sha256 c4cc6ef457687665c65f34ceb78575ee99b35f24ac87584ada694cee6f34a121
- demo/mini-loop/seeds/imports/known-good-loop.gpx: SeedRoute via gpx-route; 24233 bytes, sha256 aa020e4b300d952cc8e647bb3244be92389aa612f9454f1ba1c9a19bc40a1199
