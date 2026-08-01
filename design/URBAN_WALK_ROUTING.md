# Urban Walking Contract

## Names

A `Trail` is a durable, user-owned geographic support design. A `WalkGraph` is
the canonical topology on which a pedestrian may lawfully move. A `Route` is one
realized directed walk through that topology. Finder and Manual are routing
realms over one graph, not separate engines or corpora.

`WayKind` records source-authored physical function. Pedestrian access,
direction, surface, institutional standing, crossing control, geometry claim,
and provider confidence are independent facts. No one projection may infer or
overwrite another.

Road walking has three orthogonal axes:

- `PedestrianSupport` records the predicted physical support: sidewalk,
  shoulder, or carriageway.
- `SupportEvidence` records why that prediction is believed: mapped, tagged
  proxy, or inferred.
- `Access` records legal authority independently of physical provision.

A mapped sidewalk, a road carrying sidewalk tags, and a sidewalk inferred by
the US urban-street policy therefore share sidewalk support and routing cost.
Evidence quality remains available for provenance, hover explanation, and
diagnostics; it does not silently tax an otherwise equivalent route. Shoulder
and carriageway carry successively stronger aversion. Private and closed access
remain impassable regardless of support.

## Identity

OSM topology follows OSM node identity. Two ways join when they share a source
node. Distinct source nodes remain distinct even when their coordinates agree;
layer, bridge, and tunnel tags refine geometry but do not manufacture identity.
Near-miss repair never joins two OSM members. Cross-provider conflation may
corroborate geometry but cannot erase a separately mapped sidewalk, crossing,
or side of a street.

Provider ways lacking explicit pedestrian geometry may contribute a declared
road-centerline proxy when their tags assert a sidewalk and do not assert a
separately mapped sidepath. A proxy is routable evidence with its own geometry
claim, never fabricated offset linework. `sidewalk=separate` and
`foot=use_sidepath` forbid that substitution.

## Realms

Every lawful edge belongs to Manual. Finder is the strict recreational subset,
augmented only by bounded connectors whose removal would sever admitted trail
topology. Urban circulation cannot enter Finder through an unrefined footway,
crossing, lasso, segment edict, or trailhead snap.

Context cartography does not restate the pedestrian micrograph. Footways and
pedestrian-admitted cycleways use neutral grey. Sidewalks appear only at block
scale as intrinsic, monolithic grey substrate: they have neither a trail core
nor a surface cadence and ignore trail-hue projections. Crossings enter at the
same scale through a separate routing-diagnostic stratum. Sidewalks and
crossings remain routable facts, not legend categories.

Road cartography projects support and access into one visible
`WalkDisposition`: sidewalkable, shoulder, carriageway, or forbidden. Attached,
tagged, and inferred sidewalks share one sidewalkable shade. Shoulder and
carriageway receive distinct restrained shades. Private and closed roads
override those shades with a dark, low-saturation burgundy. The projection
tints the authoritative road stroke; it never lays a trail tube, core, or
cadence over the road and never distinguishes evidence sources by color.

Realm classification is total. Graph validation rejects an edge in neither
realm, an edge in Finder but not Manual, or a search projection inconsistent
with the canonical graph. A lasso and segment edicts intersect the Finder realm;
they never replace it.

## Routing

Support points bind to ranked lawful edge projections. The durable authority is
geographic; a stable source hint may preserve an unambiguous physical support
across a corpus rebuild, but a graph slot may not enter project state. A route
may begin, end, turn around, or pass through an edge interior without cloning or
mutating the graph.

The route cost is nonnegative and satisfies `cost ≥ κ × length` for one declared
global `κ > 0`. Every goal-directed heuristic is derived from that lower bound.
Crossing delay, access, surface, stairs, quality, and road exposure may add cost;
none may invalidate the bound. Pavement is not roadway exposure.

The optimized router is judged against reference Dijkstra over the same law.
Equal-cost alternatives follow one deterministic canonical tie law. Turn bans,
pedestrian direction, barriers, partial edges, and realm restrictions are part
of optimality rather than post-hoc validation.

## Persistence

Raw provider artifacts and graph, routing, spatial, and rendering projections
are rebuildable cache. The managed graph has one compact binary authority;
GeoJSON and CSV are explicit debug exports, not synchronized replicas. Cache
schemas may break and old instances may be discarded. Projects, saved trails,
search recipes, and names are user data and must migrate coherently.

Corpus replacement rebinds every saved trail before publication. A trail either
realizes within the declared spatial-deviation tolerance or produces a named
migration fault; it may not silently choose a nearby parallel facility.

## Performance

The event loop performs no graph-scale work. Projection, realization, metrics,
and immutable render preparation execute in generation-tagged workers. A newer
editor generation makes every older result ineligible to publish.

Performance evidence separately measures routing latency, drag coalescing,
Finder search, corpus preparation wall time, peak resident memory, and presented
frame cadence. Hiding detail, reducing candidates, or moving work beyond the
measured transaction is not an optimization.

## Cutover

The pedestrian ontology, exact OSM topology, virtual support representation,
realm projections, Finder isolation, and derived-cache schema change publish as
one corpus cutover. Superseded street severance, graph incision, linear support
binding, and synchronous realization die in that cutover. No compatibility
adapter may preserve either graph dialect.

## Checkpoint Evidence

CP0 fixed this contract. CP1 replaced the old trail-only ontology, approximate
OSM junction repair, graph incision, and synchronous editor routing with one
exact pedestrian graph, immutable router, virtual supports, and explicit Manual
and Finder realms. CP2 cut the managed corpus over to that model, reingested the
live three-region dogfood project, and exposed urban ways only at useful street
zoom without diluting the recreational legend.

The CP2 corpus contains 560,108 vertices and 776,343 edges. A 9.45 km Manhattan
manual route traverses 498 urban edges and realizes in 154 ms after a 255 ms
spatial-index build. The former four graph replicas occupied about 4.96 GiB;
their single compressed authority is 134 MB, and the whole project fell from
5.0 GiB to 418 MiB. Graph decoding fell from 39.45 s to 13.12 s and now runs
behind an already-present native shell; the live X11 dogfood project presents
its interactive workbench before graph decoding and arms routing within 15 s.
Point barriers, via-way restrictions, and
conditional restrictions remain named correctness work, not silent claims of
support.
