# Rendering Hygiene

Trailgen’s renderer is a retained system. Immutable geographic work is prepared
when its corpus changes; a frame supplies only camera, disclosure, interaction,
and other genuinely volatile state. New rendering code must preserve that
division.

## Frame Law

A camera-only frame may:

- select already-prepared spatial tiles;
- update bounded uniforms and instance data;
- submit resident geometry;
- paint screen-space UI whose cost is proportional to visible UI.

It may not rebuild geographic geometry, rescan complete routes, retessellate
immutable map lines through egui, or allocate per edge. Work proportional to
`routes × edges × route points` is categorically a preparation bug.

GPU geometry is keyed by corpus and detail band. Multiple callbacks must own
independent camera uniforms, protect the union of the current frame’s resources
from eviction, and remain bounded by the shared residency ceiling. A new corpus
must not alias an old corpus merely because its tile coordinates agree.

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

## Evidence

Every material renderer change must be exercised in at least these states:

1. project map with no transient results;
2. a full candidate portfolio on the map and in the gallery;
3. one focused candidate at trail-scale zoom;
4. pan and zoom across at least one detail-band boundary;
5. return to the prior viewport.

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
also focuses one candidate and proves Back restores the settled viewport
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
