# Optimizer

`RouteSolver` is the solver seam. The implemented solver is `LoopHunter`, a bounded sparse-graph heuristic over the normalized `TrailGraph`; future exact or semi-exact backends should implement the same trait and return the same `Route` objects.

`LoopHunter` expands from the selected start vertex, orders fanout by edge difficulty, and keeps a bounded outward frontier using `[search]` parameters:

- `max_hops`: maximum edge count in the outward search
- `max_frontier`: maximum expanded states before stopping
- `keep`: maximum candidates retained before the CLI truncates to `--count`

The outward frontier is closure-augmented: from each partial route, `LoopHunter` asks Dijkstra for the shortest legal return path to the start while forbidding already-used edges. This lets shallow outward exploration produce real sparse-graph loops instead of requiring DFS to stumble through the whole closure in edge order. Closed edge-simple candidates are measured as loops or figure-eights according to their vertex visits, then discarded unless their measured shape is in `constraints.allowed_shapes`. When `FigureEight` is allowed, the search may continue through the start after a closed lobe, producing deliberate multi-lobe closed routes. When `OutAndBack` is allowed, the solver mirrors each outward path into a return path, creating deliberate repeated-edge candidates only when the full edge sequence is directionally walkable. To make those mirrored candidates satisfiable, set `constraints.max_repeated_edge_fraction` above zero.

Every candidate is scored after full metric measurement and constraint judgment. The persisted route score is `constraint_penalty + 0.05 × scalar_difficulty + 10 × low_confidence_fraction`; lower is better. Constraint judgment covers distance, scalar difficulty, ascent/descent windows, road/pavement fraction, restricted-access fraction, low-confidence fraction, repeated-edge fraction, shape, and terrain mix.

Ranking is Pareto-first, scalar-second. `rank_routes` builds fronts over constraint penalty, distance-window deviation, ascent-window deviation, descent-window deviation, scalar difficulty, road/pavement fraction, low-confidence fraction, restricted-access fraction, and repeated-edge fraction. A route is demoted only when another route is no worse on every objective and strictly better on at least one. The scalar score orders routes inside the same front. Reports, GeoJSON exports, route JSON, and generation manifests expose both `score` and the integer Pareto rank/front alongside the measured shape, repeated-edge fraction, route-level difficulty decomposition, largest edge-factor contributors, crossing counts, terrain mix, access mix, access warnings, and evidence.

The current solver is heuristic, local, and deterministic. `trailgen generate --seed N` records `N` in `routes/generated.manifest.json` together with the app version, effective config, fingerprinted source manifest, graph summary, exact edge-id route sequences, and artifacts. The trait boundary is intentionally narrow so a stochastic heuristic or MILP/CP-SAT backend can later target small subgraphs without changing ingestion, route export, reports, difficulty modeling, or the reproduction contract.

Route names in `routes/generated.routes.json` are the stable post-generation selection handles for the CLI. `trailgen export` and `trailgen report` both rehydrate selected routes from that artifact and the cached graph instead of rerunning the solver.
