# Project Config

`trailgen init` writes `trailgen.toml` with an optional area of interest, graph snapping, route-constraint defaults, and advanced search parameters. The GUI owns `[trail_data].regions`: every rectangle drawn with **ADD MAP AREA** is persisted with a content-derived identity and the routable graph is clipped to their geometric union.

```toml
[trail_data]
managed = true
providers = [
  "ny-state-parks",
  "osm",
  "texas-state-parks",
  "usgs-national-trails",
]

[[trail_data.regions]]
id = "8cd28c863327f323e563b851"

[trail_data.regions.bounds]
west = -74.20
south = 41.10
east = -74.00
north = 41.35
```

Region IDs are content-derived spatial receipts, not provider IDs or labels; do not hand-edit them.
Add and excise rectangles in the workbench so source sequestration, the union graph, and its index
receipt change together. Each rectangle is currently limited to four square degrees to keep provider
requests bounded. `providers` names the automatic acquisition batch; the default US batch is OSM,
USGS, and the spatially applicable New York and Texas state authorities, while the normalized graph
remains provider-neutral. Projects carrying the former two-provider default migrate automatically.

`managed = true` records that the live-region corpus owns the project graph. It remains true after the last rectangle is excised, preventing an obsolete generated-route snapshot from masquerading as the current graph. Manual CLI-built projects leave it false.

Pass `trailgen init --bbox west,south,east,north` to persist a manual project AOI; `trailgen discover --bbox ...` can override it for one discovery pass.

`area` is serialized as geographic decimal-degree bounds:

```toml
[area]
west = -105.02
south = 39.99
east = -104.98
north = 40.02
```

For a GUI-managed corpus, `[area]` is the derived bounding hull of the live rectangles. It remains a compatibility surface for discovery and provider-neutral CLI tooling; `[trail_data].regions` is the exact acquisition and routing boundary. In a manual project with no live rectangles, the AOI is not a routing constraint. In both cases source recommendations in `sources/manifest.json` carry the hull so future elevation, terrain, access, road, hydrology, and seed-route acquisition stays tied to the territory that produced the graph.

`snap_tolerance_m` controls cautious graph-construction repair and defaults to `15`. Generic lines still split at exact planar intersections. Provider-aware polylines retain only authored junctions as topology, then nearby eligible endpoints form bounded clusters before any unmatched endpoint may project onto a line interior. Distances use local metres; `ExplicitNodes` inputs and grade-separated OSM bridges, tunnels, or nonzero layers are never repaired. Snapped edges receive `graph-builder` / `near-miss-snap` provenance and capped confidence. `trailgen build --snap-tolerance-m N` persists this law before writing `cache/graph.bin` so later rebuilds reproduce the topology.

`planning_date = "YYYY-MM-DD"` is the civil date used when applying dated or recurring seasonal access/closure overlays. `planning_time = "HH:MM"` is the local civil time used for hourly windows. `trailgen assemble --date YYYY-MM-DD --time HH:MM` and `trailgen apply-access --source ownership.geojson --source closures.geojson --date YYYY-MM-DD --time HH:MM` persist that planning moment while mutating the cached graph. `trailgen generate --date YYYY-MM-DD --time HH:MM` records a one-run override in the effective config and materializes a generation-time graph snapshot from stored overlays. Access application captures `sources/access-baseline.json` before the first access mutation, restores that baseline on later access applications, and then filters the composed overlay set. Apply access after graph-building, elevation, terrain, context, and seed imports; if graph topology or pre-access attribution changes, rebuild and reapply those phases before changing access dates or times. `assemble` performs those phases from `sources/manifest.json` in one deterministic sequence. Dated, seasonal, weekday, or hourly overlays without enough planning context are treated as active, a conservative default that avoids silently routing through unknown closures.

`solver` selects the generation backend:

- `auto`: use the bounded exact enumerator on small graphs and the sparse-graph heuristic elsewhere
- `heuristic`: always use `LoopHunter`
- `exact`: always use `ExactLoopSolver`, bounded by `[search]`

`trailgen generate --solver auto|heuristic|exact` overrides the config for one run. `--max-hops`, `--max-frontier`, `--keep`, and `--closure-paths` similarly override `[search]` only for that run. The manifest records both `requested_solver` and the concrete `solver`, and stores the effective search envelope under `effective_config.search`.

`generation_source_gate = "off" | "required" | "recommended"` controls whether `generate` must prove source coverage before emitting route artifacts. The default `off` preserves scratch-project ergonomics. `required` verifies source fingerprints and requires implemented trail-network plus elevation candidates. `recommended` additionally requires terrain, access, closure, road, and hydrology candidates. `trailgen generate --source-gate required|recommended|off` overrides this for one run, and `routes/generated.manifest.json` records the effective policy under `effective_config.generation_source_gate`.

`max_start_snap_m` bounds trailhead snapping during `generate`, defaulting to `500`. The requested `--start lon,lat` must be within this many meters of the nearest graph vertex, otherwise generation fails before emitting routes. Override one run with `--max-start-snap-m N` only when the coordinate is deliberately coarse. `routes/generated.manifest.json` records the requested coordinate, snapped vertex, snapped coordinate, and realized `start_snap_m`.

`max_route_snap_m` bounds connected route matching during `rate` and `import-seed`, defaulting to `100`. Every supplied GPX, GeoJSON, KML, KMZ, or CSV route segment must lie inside this distance and consecutive oriented anchors must admit a legal local connector through the graph; otherwise the command fails with snap or disconnected-transition diagnostics. Override one run with `--max-route-snap-m N` only when the source route is deliberately coarse or generalized. Imported seeds persist their snap statistics beside point count, snapped edge IDs, and metrics.

Lower-limb load and moving time use fixed, population-level physical models,
not project-tunable coefficients. Their direction-specific estimates are
recomputed whenever geometry, elevation, or terrain changes. See
[physical load and moving time](physical-load.md) for formulas, evidence, and
limits.

Distance and elevation constraints are stored in meters; moving-time
constraints are stored in seconds; lower-limb load is stored in `FGJW km`.
Each project persists one compact GUI search recipe in `library/index.json`:
optional geographic trailhead and boundary, distance range, moving-time
range, climb range, target lower-limb load, and shape. That recipe overlays
the advanced defaults in `trailgen.toml`; graph-local vertex IDs and solver
controls are never durable GUI state.

CLI `generate --min-km --max-km` overrides distance for one diagnostic run.
It can also override load and time with
`--min-lower-limb-load-km`, `--max-lower-limb-load-km`,
`--target-lower-limb-load-km`, `--min-moving-time-h`, and
`--max-moving-time-h`. Ascent/descent, road exposure, access exposure,
low-confidence limits, terrain mix, and one-run forbidden areas remain
available through `--min-ascent-m`, `--max-ascent-m`,
`--min-descent-m`, `--max-descent-m`, `--max-road-fraction`,
`--max-restricted-access-fraction`, `--max-low-confidence-fraction`,
`--forbid-terrain`, `--forbid-area path.geojson|path.shp`,
`--min-terrain terrain:fraction`, and `--max-terrain terrain:fraction`.
Confidence is a scalar in `[0,1]`; low-confidence route fraction is measured
by distance over edges below `0.6`.

The GUI lower-limb value is a ranking target, not a capacity claim or hard
ceiling. `constraints.min_lower_limb_load_km` and
`constraints.max_lower_limb_load_km` create a hard project window only when
set explicitly. Moving-time bounds are always a window; the GUI gives a new
recipe a practical `0–48 h` envelope when project defaults are unbounded.

`closure_paths` controls how many legal return paths `LoopHunter` tries from each outward frontier before ranking full candidates. Raising it explores non-shortest closures that may avoid road, access, terrain, elevation, or confidence violations; lowering it makes the heuristic more parsimonious. `[search.routing].road_aversion` is the finite detour cost for road exposure: the default `2.0` makes a fully road-bound meter cost three routing meters, without banning it. Closed and private edges remain unlawful. With no explicit `[search]` table, the GUI uses an interactive envelope of 5,000 frontier states, one shortest closure, 12 retained candidates, diversification seed 2, and the default routing law. A project may opt into a larger diagnostic search in TOML; the CLI retains its exhaustive debug-oriented defaults.

`max_road_fraction` measures distance over explicit road-exposure hints plus
edges whose normalized terrain is `road`. Paved sidewalks remain pavement,
not roadway exposure. The default `1.0` leaves ordinary road use to finite
routing aversion and route quality; an explicit lower cap remains available as
a hard project constraint.

Elevation constraints in `[constraints]`:

- `min_ascent_m` / `max_ascent_m`: total positive elevation gain window
- `min_descent_m` / `max_descent_m`: total negative elevation window

Terrain mix constraints are part of `[constraints]`:

- `forbidden_terrain`: any presence violates the route
- `min_terrain_fraction`: per-terrain lower bounds in `[0,1]`
- `max_terrain_fraction`: per-terrain upper bounds in `[0,1]`

Recognized terrain names are `unknown`, `trail`, `forest`, `alpine`, `talus`, `scramble`, `pavement`, `road`, and `water`. Repeated CLI terrain flags are allowed: `--forbid-terrain pavement --forbid-terrain road --min-terrain trail:0.60 --max-terrain talus=0.10`.

Access restrictions use `[constraints].max_restricted_access_fraction`, defaulting to `0.0`. The measured restricted fraction is distance over `restricted`, `closed`, or `private` edges divided by route distance; `unknown` access is handled by confidence/provenance rather than treated as a legal ban. Access overlays may carry `active_from`/`active_to` dates, `start_date`/`end_date`, or equivalent DBF fields; recurring seasonal closures may carry paired month-day fields such as `seasonal_from = "04-15"` and `seasonal_to = "06-30"` or `active_month_from`/`active_month_to`. Seasonal ranges may wrap the year boundary, for example `11-15` through `03-31`. Weekday recurrence accepts `weekdays`, `weekday`, `days`, `day_of_week`, `active_weekdays`, or `active_days` as strings such as `weekends`, `mon-fri`, `sat,sun`, or GeoJSON string arrays. Hourly recurrence accepts paired `time_from`/`time_to`, `active_time_from`/`active_time_to`, `start_time`/`end_time`, `starts_at`/`ends_at`, `hour_from`/`hour_to`, or `hours_from`/`hours_to` fields as `HH:MM`; time ranges may wrap midnight. If `access` or `status` is absent, boolean-like `permit_required`, `requires_permit`, `reservation_required`, `requires_reservation`, `timed_entry_required`, `timed_entry`, or `quota_required` fields normalize active overlays to `restricted` when true and `open` when explicitly false; shapefile DBF aliases such as `permit_req`, `reserv_req`, `timed_req`, and `quota_req` are also accepted. Active overlays may also carry `travel`, `travel_direction`, `direction`, `oneway`, or `one_way` to impose `both`, `forward`, or `backward` traversal while the overlay is active. Only overlays active on the planning moment are applied. Set `--max-restricted-access-fraction 0.05` only when restricted access is explicitly acceptable.

For a temporary avoid zone, repeated `generate --forbid-area path` loads the same GeoJSON or shapefile polygon/line geometry accepted by access overlays, forces every overlay to `closed`, applies it only to the in-memory generation graph, and records path, adapter, fingerprint, overlay count, and touched-edge count in `routes/generated.manifest.json`. It fails if a supplied forbidden-area source touches no graph edges, because a silent no-op ban is worse than an explicit error. Use `apply-access` instead when the closure should become part of the cached project state.

Shape constraints are also stored in `[constraints]`:

- `allowed_shapes`: route-shape whitelist; defaults to `["loop"]`
- `max_repeated_edge_fraction`: required for useful `out-and-back` generation, because out-and-back routes intentionally traverse edges twice

The CLI can override shape for one generation run with repeated `--shape` flags: `loop`, `figure-eight`, `out-and-back`, or `open`. Solvers filter emitted candidates by their measured shape. Figure-eights are deliberate multi-lobe closures through the start; out-and-backs are deliberate mirrored paths. Override repeated-edge tolerance with `--max-repeated-edge-fraction`; `--shape out-and-back --max-repeated-edge-fraction 1` is the permissive smoke-test setting. `generate --seed N` records the seed in `routes/generated.manifest.json` and threads it into the effective `[search]` config. `LoopHunter` uses it for reproducible branch diversification; `ExactLoopSolver` ignores it because exact enumeration is canonical.

`[enrichment]` controls the graph enrichment phase:

- `sample_spacing_m`: maximum spacing between elevation samples along each graph edge
- `steep_grade_threshold`: absolute grade threshold used to accumulate sustained steep distance

`apply-elevation --confidence` sets the confidence carried by a local Arc/Info ASCII or GeoTIFF DEM sample. Edge confidence is capped by sampled elevation confidence, because precise grade and ascent claims should not outvote a weak raster.

`apply-terrain --source terrain.geojson` accepts FeatureCollections with Polygon, MultiPolygon, or LineString geometries. Each feature should carry a recognized `terrain`, `surface`, `landcover`, `land_cover`, `nlcd`, `nlcd_code`, `gridcode`, `class`, or `class_name` tag plus optional `confidence`, `tolerance_m`, `source`, `layer`, `id`/`name`, and `license`. Recognized terrain tags normalize through the same `Terrain` enum as network ingestion; land-cover fields also accept common NLCD numeric codes and class names such as `31`/barren rock, `41`/deciduous forest, developed classes, open water, and wetlands.

Generated route selection is name-based. After `generate`, `trailgen export <project> --route candidate-1 --format gpx|geojson|csv|kml|kmz --output file [--report-output file.md]` loads `routes/generated.routes.json` and the generation graph snapshot, then emits only the requested route plus an optional selected-route report sidecar. `trailgen report <project> --route candidate-1 --output file.md` renders the same one-route diagnostic report; omit `--route` to render the full generated set. `trailgen map <project> --output file.html` renders a self-contained interactive offline SVG map from the effective graph and generated route set. Reports load the generation ledger, effective constraints, and source-manifest snapshot from `routes/generated.manifest.json`, and print the exact route edge/vertex sequence, so the app version, seed, requested/concrete solver, start snap, one-off CLI overrides such as `--min-km`, `--max-km`, shape flags, date flags, or elevation bounds, route graph identity, directed-travel and turn-ban counts, graph elevation coverage, and the original source-coverage evidence remain visible in later selected reports. Re-running `generate` deletes stale files that were owned by the previous generated manifest but disappeared from the new artifact list; manual route exports and selected-route handoff files outside that ledger are left untouched.
