# Native Responsiveness Doctrine

- **Status:** Trailgen incubation; candidate chapter for the future native-app
  doctrine
- **Scope:** startup publication, event-loop work, frame production,
  presentation, and interactive background activity

Responsiveness is a product contract, not a late profiling exercise. The native
shell owns a stable measurement spine. Each product names the domain work nested
inside that spine. Acceptance owns performance judgment through real input and
presented output.

## Vocabulary

**Reaction latency** runs from an admitted native gesture to the first presented
frame that visibly acknowledges it.

**Frame work** is wall time spent by the event-loop thread producing one frame.
It includes product UI, tessellation, GPU preparation and upload, submission,
presentation return, and post-present commit.

**Cadence** is the distribution of intervals between consecutive presented
frames during a sustained interaction. Frame work and cadence are related but
not interchangeable: compositor waits and scheduling can enlarge cadence, while
asynchronous GPU work can escape CPU frame work.

**Publication** is the smallest lawful product state that can be presented and
used. Publication must not wait for independent armament merely because one
constructor currently returns both.

**Armament** is prepared domain state installed by a bounded event-loop
transaction: an index, retained geometry, decoded corpus, search result, or
similar immutable projection.

**Drain** is bounded transfer from a worker queue into presented ownership.

## Laws

1. The event loop performs work proportional only to the current frame's
   visible UI, resident tiles, and bounded transfer budgets. It never performs
   work proportional to a corpus, project, result portfolio, file, or complete
   worker backlog.
2. File access, decoding, indexing, sorting, geometric preparation, statistics,
   network acquisition, cache repair, and durable writes are background work by
   presumption. An exception requires measured proof that its worst case fits
   the owning interaction budget.
3. The first useful shell and every independent substrate publish as soon as
   their own prerequisites exist. A basemap does not wait for a routing graph;
   chrome does not wait for either. Later armament replaces explicit preparing
   states without resetting the viewport or withdrawing already presented
   content.
4. Worker results carry generation identity. Event-loop drains have item and
   wall-time ceilings, request another frame when work remains, and reject stale
   generations.
5. GPU callbacks obey the same law as ordinary UI. Buffer construction and
   upload are visibility-prioritized, bounded per frame, and resumable. A GPU
   callback is not an exemption from main-thread accounting.
6. Search, acquisition, and preparation never depend on repaint animation for
   progress. Decorative motion cannot enlarge a visibility-critical
   transaction.
7. A slow action may remain slow only when its essential domain operation is
   intrinsically long. It must acknowledge immediately, remain cancellable when
   cancellation is meaningful, publish monotone progress, and preserve the last
   useful state.

## Trace Spine

The shell defines canonical spans; applications may only refine them.

```text
window.event
frame
  frame.input
  frame.ui
    product pulse and domain phases
  frame.platform_output
  frame.tessellate
  frame.water
  frame.render
    render.encoder
    render.prepare
      product GPU preparation and upload phases
    render.acquire_surface
    render.egui_pass
    render.water_compose
    render.submit
    render.water_after_submit
    render.free_textures
    render.present
  frame.after_present
```

Startup and workers use named threads plus spans for each indivisible operation.
Every span name denotes one stable semantic phase; source function names and
temporary implementation types are not trace vocabulary. Product spans nest
under shell spans on the event-loop thread. Cross-thread work remains on its
actual thread rather than being projected into fictitious main-thread time.

Trailgen currently enables this spine with:

```sh
ETERNALIST_TRACE=/tmp/trailgen-trace.json \
ETERNALIST_TRACE_SECONDS=60 \
trailgen gui /path/to/project --offline
```

The first variable arms a Chrome/Perfetto trace. The optional second variable
causes an orderly profiling exit so the artifact is complete even under an
unattended driver. Without `ETERNALIST_TRACE`, trace call sites use the disabled
`tracing` fast path. Trace collection must not be enabled in production latency
adjudication unless its overhead has been separately bounded.

## Budgets

Functional timeouts and performance budgets are distinct. A timeout prevents a
hung test from living forever; it cannot excuse missed latency.

Each product declares latency classes in its contract. The minimum set is:

- **Immediate reaction:** ordinary control, mode, and selection feedback.
- **Durable reaction:** visible acknowledgment plus completion of one local
  durable mutation.
- **Sustained canvas:** pan, zoom, scroll, drag, or scrub cadence over a fixed
  action sequence.
- **First publication:** process launch to the first useful presented surface.

Budgets terminate at surface presentation or another external effect, not at
model mutation or frame construction. Sustained budgets report at least p50,
p95, and worst cadence plus p95 frame work. Representative host graphics
adjudicate shipping latency; deterministic software graphics remains useful for
functional evidence.

The shell may emit an over-budget trace event, but the external acceptance
harness decides pass or fail. Product instrumentation is evidence, never its
own oracle.

## Rectification Protocol

1. Lock one representative workload, semantic envelope, graphics path, and
   latency claim.
2. Capture a settled baseline and a complete trace. Synthetic fixtures cannot
   stand in for corpus scale when the suspected cost is scale-dependent.
3. Rank main-thread phases by exclusive wall time, p95, worst case, and
   interaction correlation. Inspect worker overlap and queue arrival separately.
4. Prove causality by changing one boundary or cost center. Deleting behavior,
   hiding detail, reducing results, or moving work beyond the measured interval
   is not optimization.
5. Repair the ownership boundary that admitted the cost. Prefer retained state,
   background preparation, bounded drains, and progressive publication over
   local micro-optimization.
6. Re-run the identical workload, standing functional stories, visual states,
   and disabled-trace control. Retain the before/after ledger.

## Evidence And Promotion

A responsiveness change is incomplete without:

- a representative trace naming the dominant cost before the change;
- an identical after trace and wall-clock distribution;
- acceptance evidence for the affected user story;
- proof that the disabled instrumentation path did not create a material
  regression;
- failure artifacts containing the action transcript, frame journal, logs,
  screenshot, and trace when trace collection was armed.

The shell phase vocabulary may move into shared middleware after a second
application uses it without product-specific branches. Latency classes and
trace-artifact conventions may enter the fleet skill after that same trial.
Product phase helpers, automatic trace capture on budget failure, and stronger
budget types require evidence from at least two distinct workloads. Proc macros
remain deferred until repeated handwritten declarations reveal a stable
language.

## Trailgen Evidence

The initial live trace used the 560,108-vertex, 776,343-edge NJ–NYS project. It
found two ownership defects:

1. The basemap source was constructed only after a 134 MiB compressed routing
   graph had been decoded, indexed, and tessellated off thread. Graph decode
   took 11.5–12.1 s and atlas preparation another 2.1 s. The worker kept the
   event loop alive but withheld an independent, useful map until the entire
   bundle completed.
2. The nominally 2 ms basemap drain called a complete parking-to-trail spatial
   projection while absorbing one tile. A single indivisible event-loop item
   therefore took 44.3 ms; live panning reached 75.3 ms p95 and 79.4 ms worst
   cadence.

Trailgen now raises the regional basemap independently and presents it about
0.83 s after launch while graph armament continues. Parking projection belongs
to a bounded forge and the event loop installs at most sixteen completed tiles
per frame. The loading map itself is transferred into the armed workbench, so
publication neither decodes a second substrate nor surrenders resident tiles.
Across two identical host-GPU after-runs, panning measured 39.0–45.3 ms p95 and
39.3–53.6 ms worst; `basemap.absorb` remained below 0.023 ms. The residual
variation belongs to X11 surface-presentation cadence: final product UI work
peaked at 6.6 ms, while `frame.render` including presentation return reached
65.0 ms. This is the pattern the doctrine intends: repair publication and
ownership boundaries before tuning instructions.
