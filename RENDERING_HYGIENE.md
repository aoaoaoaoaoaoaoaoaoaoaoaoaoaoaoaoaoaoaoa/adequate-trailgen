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

Use the isolated X11 harness; never test against the live desktop. Set
`TRAILGEN_PROFILE_FRAMES=/tmp/frames.csv` for per-frame interval, paint time,
shape count, and primitive count. Set `TRAILGEN_PROFILE_TRAILS=1` for trail
forge and GPU upload telemetry.

For the standing candidate trace:

```sh
scripts/profile-candidates /tmp/trailgen-profile /path/to/project
```

The script raises its own private lavapipe/Xvfb session and writes timing,
telemetry, and screenshots beneath the named output directory.

Record p50, p95, and worst frame paint time, egui shape count, uploaded bytes,
resident bytes, and the decisive screenshot. Compare a settled repeated trace,
not a lucky frame. A change is not faster if it reduces candidates, hides a
layer, weakens detail, changes the viewport, or moves work outside the measured
interval.

The standing many-result dogfood workload is the saved broad Harriman loop
search in the `nj-nys` project. It currently returns twelve ranked candidates.
Its result view must keep egui shape count proportional to UI and visible
thumbnails, never to the combined route geometry.
