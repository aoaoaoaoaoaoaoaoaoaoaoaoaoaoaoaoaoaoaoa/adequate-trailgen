# Project Config

`trailgen init` writes `trailgen.toml` with an optional area of interest, graph snapping, difficulty weights, route constraints, and search parameters. Pass `--bbox west,south,east,north` to persist the project AOI; `trailgen discover --bbox ...` can override it for one discovery pass.

`area` is serialized as geographic decimal-degree bounds:

```toml
[area]
west = -105.02
south = 39.99
east = -104.98
north = 40.02
```

The AOI is not a routing constraint. It is a discovery/reproducibility contract: source recommendations in `sources/manifest.json` carry the same bounds so future acquisition of trail, elevation, terrain, access, road, hydrology, and seed-route inputs stays tied to the region that produced the graph.

`snap_tolerance_m` controls cautious graph-construction snapping. Exact segment intersections are always split. In addition, a dangling source-geometry endpoint may be projected onto another source segment when the projected point lies inside that segment and within `snap_tolerance_m`; snapped edges receive `graph-builder` / `near-miss-snap` provenance and capped confidence so reports expose the uncertainty instead of pretending the junction was source-authored.

`planning_date = "YYYY-MM-DD"` is the civil date used when applying dated access or closure overlays. `trailgen generate --date YYYY-MM-DD` records a one-run override in the effective config and materializes a generation-time graph snapshot from stored overlays. `trailgen apply-access --date YYYY-MM-DD` persists that planning date, captures `sources/access-baseline.json` before the first access mutation, restores that baseline on later access applications, and then filters overlays while mutating the cached graph. Apply access after graph-building, elevation, terrain, context, and seed imports; if graph topology or pre-access attribution changes, rebuild and reapply those phases before changing access dates. Dated overlays without a planning date are treated as active, a conservative default that avoids silently routing through unknown closures.

`solver` selects the generation backend:

- `auto`: use the bounded exact enumerator on small graphs and the sparse-graph heuristic elsewhere
- `heuristic`: always use `LoopHunter`
- `exact`: always use `ExactLoopSolver`, bounded by `[search]`

`trailgen generate --solver auto|heuristic|exact` overrides the config for one run. The manifest records both `requested_solver` and the concrete `solver`.

`max_start_snap_m` bounds trailhead snapping during `generate`, defaulting to `500`. The requested `--start lon,lat` must be within this many meters of the nearest graph vertex, otherwise generation fails before emitting routes. Override one run with `--max-start-snap-m N` only when the coordinate is deliberately coarse. `routes/generated.manifest.json` records the requested coordinate, snapped vertex, snapped coordinate, and realized `start_snap_m`.

`[difficulty]` controls additive edge rating. The supported weights are `distance_per_km`, `ascent_per_m`, `descent_per_m`, `grade_per_abs_fraction`, `road_penalty`, `technical_penalty`, `navigation_penalty`, `low_confidence_penalty`, and `closed_access_penalty`. `[difficulty.terrain_multipliers]` overrides the per-terrain distance multiplier table for `unknown`, `trail`, `forest`, `alpine`, `talus`, `scramble`, `pavement`, `road`, and `water`; omitted buckets use defaults. Use `trailgen rerate <project>` after hand edits to push the current weights into cached edge costs. Use `trailgen calibrate <project> --route completed.gpx --target-difficulty N [--family elevation] [--write]` to solve and optionally persist a completed-hike calibration. See [difficulty.md](difficulty.md) for the factor formula and calibration workflow.

Distance and elevation constraints are stored in meters. CLI `generate --min-km --max-km` overrides the distance window for that run. `generate` can also override scalar difficulty, ascent/descent, road/pavement exposure, access restriction exposure, low-confidence limits, and terrain mix with `--min-difficulty`, `--max-difficulty`, `--min-ascent-m`, `--max-ascent-m`, `--min-descent-m`, `--max-descent-m`, `--max-road-fraction`, `--max-restricted-access-fraction`, `--max-low-confidence-fraction`, `--forbid-terrain`, `--min-terrain terrain:fraction`, and `--max-terrain terrain:fraction`. Confidence is a scalar in `[0,1]`; low-confidence route fraction is measured by distance over edges below `0.6`.

`max_road_fraction` measures distance over explicit road-exposure hints plus any edge whose normalized terrain is `road` or `pavement`. A paved path with no road overlay therefore still counts against the road/pavement cap.

Elevation constraints in `[constraints]`:

- `min_ascent_m` / `max_ascent_m`: total positive elevation gain window
- `min_descent_m` / `max_descent_m`: total negative elevation window

Terrain mix constraints are part of `[constraints]`:

- `forbidden_terrain`: any presence violates the route
- `min_terrain_fraction`: per-terrain lower bounds in `[0,1]`
- `max_terrain_fraction`: per-terrain upper bounds in `[0,1]`

Recognized terrain names are `unknown`, `trail`, `forest`, `alpine`, `talus`, `scramble`, `pavement`, `road`, and `water`. Repeated CLI terrain flags are allowed: `--forbid-terrain pavement --forbid-terrain road --min-terrain trail:0.60 --max-terrain talus=0.10`.

Access restrictions use `[constraints].max_restricted_access_fraction`, defaulting to `0.0`. The measured restricted fraction is distance over `restricted`, `closed`, or `private` edges divided by route distance; `unknown` access is handled by confidence/provenance rather than treated as a legal ban. Access overlays may carry `active_from`/`active_to` dates, `start_date`/`end_date`, or equivalent DBF fields; only overlays active on the planning date are applied. Set `--max-restricted-access-fraction 0.05` only when restricted access is explicitly acceptable.

Shape constraints are also stored in `[constraints]`:

- `allowed_shapes`: route-shape whitelist; defaults to `["loop"]`
- `max_repeated_edge_fraction`: required for useful `out-and-back` generation, because out-and-back routes intentionally traverse edges twice

The CLI can override shape for one generation run with repeated `--shape` flags: `loop`, `figure-eight`, `out-and-back`, or `open`. Solvers filter emitted candidates by their measured shape. Figure-eights are deliberate multi-lobe closures through the start; out-and-backs are deliberate mirrored paths. Override repeated-edge tolerance with `--max-repeated-edge-fraction`; `--shape out-and-back --max-repeated-edge-fraction 1` is the permissive smoke-test setting. `generate --seed N` records the random seed in `routes/generated.manifest.json`; built-in solvers are deterministic today, but the manifest schema already preserves the seed needed by future stochastic solvers.

`[enrichment]` controls the graph enrichment phase:

- `sample_spacing_m`: maximum spacing between elevation samples along each graph edge
- `steep_grade_threshold`: absolute grade threshold used to accumulate sustained steep distance

`apply-elevation --confidence` sets the confidence carried by a local Arc/Info ASCII or GeoTIFF DEM sample. Edge confidence is capped by sampled elevation confidence, because precise grade and ascent claims should not outvote a weak raster.

`apply-terrain --source terrain.geojson` accepts FeatureCollections with Polygon, MultiPolygon, or LineString geometries. Each feature should carry a recognized `terrain`, `surface`, `landcover`, or `land_cover` tag plus optional `confidence`, `tolerance_m`, `source`, `layer`, `id`/`name`, and `license`. Recognized terrain tags normalize through the same `Terrain` enum as network ingestion.

Generated route selection is name-based. After `generate`, `trailgen export <project> --route candidate-1 --format gpx|geojson|csv|kml|kmz --output file` loads `routes/generated.routes.json` and the generation graph snapshot, then emits only the requested route. `trailgen report <project> --route candidate-1 --output file.md` renders a one-route diagnostic report; omit `--route` to render the full generated set. `trailgen map <project> --output file.html` renders a self-contained offline SVG map from the effective graph and generated route set. Reports load the effective constraints from `routes/generated.manifest.json`, so one-off CLI overrides such as `--min-km`, `--max-km`, shape flags, date flags, or elevation bounds remain visible in later selected reports.
