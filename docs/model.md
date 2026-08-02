# Model

The core noun is the `WalkGraph`: vertices, edge geometry, directed-travel-capable edges, provider-neutral turn bans, and attributed edge costs. Generic vector geometries split at exact planar intersections. OSM ways are first contracted into junction-to-junction polylines, so ordinary shape points do not become routing vertices and projected crossings do not become transfers. A bounded 15 m endpoint repair then closes small provider-fragment seams: nearby endpoints cluster before any unmatched endpoint may project onto a line interior. The cluster diameter is bounded, distance is measured in local metres, and bridge, tunnel, or nonzero-layer OSM geometry is ineligible. `JunctionPolicy::ExplicitNodes` remains exact and never quantizes nearby coordinates into a junction. Every repaired edge carries `graph-builder` / `near-miss-snap` provenance and capped confidence. Multiedges are permitted. Edges default to bidirectional travel, but `EdgeTravel` can restrict traversal to geometry-forward or geometry-backward when a source adapter proves one-way movement. A `TurnBan` rejects one directed edge transition through a `via` vertex; `walk_edges`, route metric construction, and solvers all use that same legality oracle.

Provider adapters normalize their strata into `SegmentDraft`s before one graph build, so intersections and bounded near-miss repair are resolved across sources while each edge keeps its provenance. Route-file readers remain provider-neutral low-level adapters; they are not a parallel project-construction frontend.

The user-owned noun is `Trail`: a shape, an ordered sequence of geographic `SupportPoint`s, and a `RoutingLaw`. Realization snaps each support point to the graph and joins consecutive bindings by the lowest lawful routing cost. Open trails stop at the final support point; out-and-backs reverse the realized outward spine exactly; loops add the least-cost return to the trailhead. The default road aversion makes a fully exposed road meter cost three routing meters but never makes an otherwise lawful road infinite. Closed and private edges are unlawful. The manual editor works only through this parameterization. Search candidates recover a compact design when shortest lawful legs can reproduce their exact walk; graph vertices and edge IDs remain disposable realization details.

Each edge stores length, ascent, descent, mean/max grade, surrounding hill
slope, sustained steep distance, fixed-bin grade distribution, way kind,
trail standing, trail marking, terrain, raw surface, evidence/confidence,
access/provenance/confidence, road and water crossings, road exposure,
aggregate confidence, direction-specific physical traversal estimates, seed
evidence, elevation provenance, and source provenance. Standing
(`established`, `unmaintained`, `informal`, `historical`, or `unknown`)
describes the path's asserted existence and maintenance. Marking (`marked`,
`unmarked`, or `unknown`) describes deliberate wayfinding marks. Access
answers the separate legal question. `WayKind::Bushwhack` describes a
deliberate pathless route, not a faint, unofficial, or merely unmarked path.
Its physical substrate remains a separate `Terrain` such as forest, alpine,
talus, or water. The grade distribution bins sampled edge meters into flat
`<5%`, rolling `5–15%`, steep `15–30%`, and savage `≥30%` absolute grade;
when an elevation source covers only part of an edge, grade is computed from
covered spans rather than inventing flat no-data terrain. See
[physical load and moving time](physical-load.md) for the fixed population
models.

Elevation enrichment is a separate phase. `ElevationSampler` is the trait seam for automatic cached Mapzen Terrarium tiles, embedded geometry elevations, synthetic fixtures, local Arc/Info ASCII Grid DEMs, supported affine GeoTIFF DEMs and VRT wrappers, deterministic DEM mosaics, and future arbitrary projected-raster samplers. Enrichment samples a configurable densification, recomputes ascent/descent and grade statistics, then records which source produced the elevation values. If the sampler yields no evidence, the original geometry survives unchanged rather than accumulating empty interpolation points. Automatic terrain receipts are fingerprinted alongside the trail index; invalid void or bathymetric pixels become missing spans rather than false vertical cliffs.

Terrain inference is evidence-bearing: explicit source tags win with high confidence, explicit surface tags carry medium-high confidence, and adapter fallbacks such as generic OSM `highway=path` trail surfaces remain weak. Unknown surfaces may be inferred from road exposure or sampled grade, but those inferences carry lower confidence, the measured grade/road basis, and the provenance that caused the inference. A mild bushwhack with no land-cover evidence remains `Terrain::Unknown`; enrichment must never turn the absence of a path into a generic trail surface. Reapplying elevation replaces stale enrichment-derived terrain evidence instead of accumulating ghosts from prior DEMs. Terrain overlays are an intermediate-confidence evidence source for land-cover layers, surface maps, and user overrides. Polygon overlays affect every intersecting edge; line overlays affect intersecting or nearby edges within `tolerance_m`. Applied overlays update the edge terrain bucket, append terrain evidence, lower confidence according to overlay confidence, and recompute physical traversal estimates.

Imported seed routes are first-class project artifacts. A seed stores source path, original source path, format, provider-neutral route metadata when present, point count, snap statistics, snapped edge IDs, loop-closure status, metrics, and provenance. Seed imports are bounded by `max_route_snap_m`, raise `seed_count`, `popularity`, and confidence on touched edges, and closed connected seeds are included as route candidates during generation.

The managed data pipeline stores one rebuildable graph cache at
`cache/graph.bin`. It does not multiply the corpus into automatic JSON,
GeoJSON, and CSV replicas.

Access overlays are applied after graph construction. A GeoJSON or shapefile overlay feature declares `access` or `status`, optional `travel`/`direction`, confidence, provenance fields, optional absolute active dates, optional recurring seasonal month-day windows, optional weekday recurrence, optional daily time windows, and either a Polygon/MultiPolygon area or a LineString corridor. Polygon overlays affect every intersecting edge; line overlays affect intersecting or nearby edges within `tolerance_m`. Applied overlays set edge access status, may impose directed travel, record access provenance, lower confidence, and rebuild directed adjacency when travel changes. Access changes admissibility and quality, not physical load or moving time.

Context overlays are also applied after graph construction. GeoJSON or shapefile road/hydrology LineString features declare `kind` or `context`; shapefile filenames such as `roads.shp` or `streams.shp` can supply the context kind when DBF rows omit it. OSM XML/PBF context adapters normalize highway ways into road context and `waterway=stream|river|canal|drain|ditch|brook` ways into hydrology context. Segment intersections with graph edges are counted as crossings. Road crossings raise road-exposure hints, road and water crossings carry provenance, and route metrics aggregate crossing counts for reports and exports.

Route metrics classify shape as `Loop`, `FigureEight`, `OutAndBack`, or
`Open`. Shape is measured from the traversed vertex/edge sequence, not from a
caller-supplied label: repeated edges imply out-and-back, repeated junctions
inside a closed route imply figure-eight, and non-closing routes are open.
Lower-limb load is ranked by distance from the requested target. Moving time
obeys an independent search window. Quality is a length-normalized 0–100
desirability that falls with road exposure, uncertainty, or dubious access
and is always maximized. Route metrics also carry terrain/access mixes,
elevation coverage, sustained steep meters, grade distribution,
road exposure, and repeated-edge exposure. Pavement is terrain, not evidence
that the walker occupies a roadway. Exact constraint matches
always precede near misses; Pareto fronts then compare distance, elevation,
moving-time, and load deviations with quality loss, restricted access, and
repetition. Scalar score refines a front as
`constraint penalty + 0.1 × quality loss`. The constraint audit remains a row
ledger with metric, measured value, requirement, signed margin, and pass/fail
state, so downstream tools do not need to parse prose violations.

Route generation is behind the `RouteSolver` trait. Out-and-backs use one road-aware shortest-path frontier and emit one canonical outward spine per reachable turnaround; this prevents microscopic DFS perturbations from consuming the result set. Loops use the bounded `LoopHunter` k-closure heuristic or `ExactLoopSolver` on small graphs. After constraint/Pareto ranking, a length-weighted edge-overlap portfolio admits distinct route basins first and progressively relaxes its exclusion radius only to fill otherwise empty slots. This is diversity during selection over a broad frontier, not random tie noise.

The native workbench is a projection of this same model, not a second routing system. It loads `cache/graph.bin`; search and manual editing both invoke the shared realization machinery. Promotion captures the canonical support design plus a geometry/attribute snapshot into `library/index.json`, so a trail stays editable while surviving later graph refreshes without persisting graph-local identities. Legacy geometry-only saves remain viewable but cannot masquerade as editable designs. Saved trails and one compact geographic search recipe belong directly to the project. A project Protomaps cut plus an XDG-cached remote fallback supply disposable vector context through a batched wgpu renderer; selected trail geometry and elevation profiles remain independent, locally measured vector data.
