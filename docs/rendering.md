# Rendering Hygiene

Trailgen’s renderer is a retained system. Immutable geographic work is prepared
when its corpus changes; a frame supplies only camera, disclosure, interaction,
and other genuinely volatile state. New rendering code must preserve that
division.

Eternalist Apps owns event-loop measurement, startup publication, and trace
law. This document owns only Trailgen's renderer boundaries, product budgets,
and measured evidence.

## Frame Law

A camera-only frame may:

- select already-prepared spatial tiles;
- update bounded uniforms and instance data;
- submit resident geometry;
- paint screen-space UI whose cost is proportional to visible UI.

It may not rebuild geographic geometry, rescan complete routes, retessellate
immutable map lines through egui, or allocate per edge. Work proportional to
`routes × edges × route points` is categorically a preparation bug.

Spatial predicates use a prepared index owned by the active corpus. Tile
annotation, parking adjacency, picking, and viewport selection may query that
index; they may not scan the graph once per feature, tile, label, or frame.

GPU geometry is keyed by corpus and detail band. Multiple callbacks must own
independent camera uniforms, protect the union of the current frame’s resources
from eviction, and remain bounded by the shared residency ceiling. A new corpus
must not alias an old corpus merely because its tile coordinates agree.
Cadence laws belong once to their trail corpus, and transition opacity belongs
once to its detail layer. Replicating either into every spatial tile turns a
zoom boundary into an unbounded queue-write storm.

## Responsiveness

The event loop never performs work proportional to a graph, route portfolio,
tile corpus, or file. Search completion, sorting, project refresh, cache
construction, geometry preparation, and derived statistics run off-thread.
Their UI events may install prepared ownership, update bounded state, and
request a repaint; they may not finish the work on receipt.

Delayed content is lawful and must expose a named preparing state. A frozen
window is not. Background work carries generation identity, cooperates with
cancellation between indivisible operations, and cannot publish stale results.
Decorative animation must never enlarge the visibility-critical transaction.

Event absorption and GPU publication are separately budgeted. Visible tiles
upload before speculative refinements; exact resident refinements may prewarm
behind the currently presented fallback. When the budget is exhausted, the
remainder stays queued and requests another frame. One frame may not drain an
unbounded worker backlog or upload every newly available tile.

Corpus retargeting is a monotone presentation handoff. The last presented
basemap and relief remain resident and drawable while the successor source is
acquired and prepared. Successor tiles replace retained fallbacks only after
they are resident under a distinct corpus identity. Adding, resizing, or
refreshing a map area must never clear the map to advertise progress.

## Detail

Level of detail is an analytic cartographic schedule, not a frame-by-frame
guess:

- simplify against a fixed screen-space error budget;
- make finer geometries nested refinements of coarser ones;
- choose bands from settled zoom with hysteresis;
- fade uniformly across a band boundary;
- let patterns and cores recede before their colored tubes;
- retain exact geometry at the declared deep-detail ceiling.

Do not add dynamic “is this feature under one pixel now?” scans when a fixed
zoom threshold can be derived once from feature width and map scale.

World-anchored phenomena stay in world coordinates. Dash phase, label anchors,
and tile identity must not depend on viewport history, provider segmentation,
or the order in which frames arrive.

## Redundancy

Never submit a primitive whose identical successor wholly occludes it. Candidate
routes sharing one canonical physical support may collapse to the topmost
occurrence when width and styling semantics make the discarded draws
pixel-equivalent. Distinct parallel supports must survive.

The routable walking corpus exclusively owns trail, cycleway, footway,
sidewalk, and crossing geometry inside live areas. The basemap must not submit
lower-fidelity copies with an independent disclosure schedule. Duplicate
ownership creates nonmonotone visibility and misrepresents dead areas as
routable.

Privileged routes are style-continuous chains, not bags of provider or graph
fragments. Fuse consecutive supports through degree-two vertices; split at
branches, visual-law changes, and ownership changes. Bound selected-line miters
and close true chain endpoints, so width cannot turn graph seams into gaps or
acute bends into spikes.

The same principle applies before the GPU: normalize provider identity once,
cache derived projections, and share immutable buffers. Do not conceal work in
an unbounded cache or defer it beyond the measured interaction.

## Galleries

Tiles are small rasterization targets, not miniature GIS engines.

- Cull off-screen tiles before painting their contents.
- Compute route bounds and projection once per immutable candidate.
- Merge contiguous legs with the same visual law.
- Simplify the prepared miniature below a quarter-point error.
- Preserve endpoints, topology, route color, and wayfinding marks.

Scrolling may change tile translation and hover state; it must not regenerate
route geometry.

## Composition

Cartographic order is contractual. Ground, fills, relief, transport, trail
context, privileged routes, labels, parking marks, controls, and help surfaces
must retain their intentional order. GPU promotion is not permission to move a
layer across labels or symbols.

Selected routes dominate context by width and chroma. Overview disclosure may
remove their internal pattern before the tube, but selection, candidate
identity, and access warnings must remain legible.

Unbounded categorical color belongs to the shared perceptual cycler, not to
feature-local RGB arithmetic. A use site declares an OKLCH volume and lawful
hue arcs; the cycler supplies a deterministic sparse prefix and then maximizes
nearest ΔE in OKLab. Cartographic cycles exclude hues that alias dominant map
ground. Ordinals come from stable semantic order, never frame arrival order.
Trail comparison cycles also exclude hydrological blue, which remains context
rather than candidate identity.

Trail hue is a user-selected semantic projection: class, formal/informal, or
terrain. Switching it is a uniform-only operation over retained geometry.
Surface and wayfinding own the core cadence independently; no hue projection
may rebuild, replace, phase-shift, or otherwise reinterpret solid, dashed,
dash-dot, or dotted marks. Access alarms continue to outrank every projection.

## Living Waits

Real background work must leave its visible waiting surface alive. Paint one
thin, bottom-centered plaque over the map and claim that exact rectangle
through `eternalist_apps::LivingWait`; its four-corner raft supplies restrained
motion and excites the latent water table. Do not call Poolrooms' loading
show/hide pair directly from competing panels. The plaque vanishes and the raft
settles automatically when no work claims the next frame.

Only actual in-flight work may animate. Empty, failed, and completed states are
still. Waiting motion is decorative evidence of liveness, never a scheduler or
an excuse to enlarge the main-thread transaction.

## Evidence

Every material renderer change must be exercised in at least these states:

1. project map with no transient results;
2. a full candidate portfolio on the map and in the gallery;
3. one focused candidate at trail-scale zoom;
4. pan and zoom across at least one detail-band boundary;
5. return to the prior viewport;
6. add or resize a map area while the prior basemap remains continuously
   presented and the camera remains responsive.

Use the isolated native acceptance harness; never test against the live
desktop:

```sh
scripts/test-gui
```

Story 3 forges a deterministic dense graph, obtains twelve candidates, pans
while search is active, then sustains pan and zoom across the full portfolio.
Its lossless frame journal records frame begin, semantic observation,
and surface-present return. `CadenceBudget` uses those raw product timestamps
and rejects median, p95, worst, and p95 paint regressions; no guessed
instrumentation multiplier or post-hoc witness correction is admissible.
Witness serialization runs asynchronously after surface present. The story
also focuses one candidate and proves Escape restores the settled viewport
exactly in semantic state; pointer-normalized map pixels must return near the
baseline and decisively away from the focused-view control.

The canonical host-GPU contract is p50 cadence ≤ 40 ms, p95 cadence ≤ 50 ms,
worst cadence ≤ 180 ms, and p95 product frame work ≤ 40 ms. A passing run prints
the observed distributions. Failure bundles retain the action transcript,
last witness, frame journal, application logs, private product state, and
latest screenshot beneath `TRAILGEN_ACCEPTANCE_ARTIFACTS`.

Compare a settled repeated trace, not a lucky frame. A change is not faster if
it reduces candidates, hides a layer, weakens detail, changes the viewport, or
moves work outside the measured interval. The result view must keep CPU and
egui work proportional to visible UI and resident tiles, never to combined
route geometry.

## Performance Evidence

The standing large-corpus workload has 560,108 vertices and 776,343 edges in
the NJ–NYS project. The original host withheld the basemap behind 11.5–12.1 s
of graph decode and 2.1 s of atlas preparation; one ostensibly bounded tile
absorption also performed a complete parking projection and consumed 44.3 ms
on the event-loop thread. Panning reached 75.3 ms p95 and 79.4 ms worst
cadence.

After separating publication and preparing parking projections in a bounded
forge, the regional map presents at about 0.83 s while graph armament
continues. A later isolated trace raised the workbench in 61.7 ms, began its
first UI frame at 258 ms, and completed graph armament at 14.86 s. The first
Inspector pass cost 5.0 ms, the first map pass 1.0 ms, and `basemap.absorb`
stayed below 0.023 ms. Two host-GPU repeats measured 39.0–45.3 ms p95 and
39.3–53.6 ms worst cadence; product UI work peaked at 6.6 ms while presentation
return enlarged `frame.render` to 65.0 ms.

An August 2026 idle-residency inquest found an overnight process at 44.9 GiB
RSS, including 44.5 GiB of private dirty memory in 350 128 MiB mappings through
`/dev/nvidiactl`. The Rust heap was about 182 MiB and device-local allocation
was below 500 MiB. A 7,200-frame offscreen run settled at 2.52 GiB RSS and was
flat after frame 900, isolating the proved causal envelope to perpetual surface
presentation plus driver host mappings rather than an application-heap leak.
The repair bounds tension wakes, denies frame-originated continuation while
unfocused, stops known concealed presentation, uses foreground-only streaming
wakes, bounds result drains, suppresses unchanged uniform uploads, requests one
frame in flight, and performs nonblocking device maintenance after present.
Release evidence must cover settled focused, unfocused, and concealed rests
plus an RSS soak.
