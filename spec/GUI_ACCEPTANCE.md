# GUI Acceptance

Trailgen's executable GUI contract lives in `trailgen-acceptance`. Each
scenario drives an optimized product binary through native X11 input, uses a
one-way observation only for targeting and synchronization, and reaches a
durable or rendered oracle before passing. Tests never call product internals.

## Standing Stories

1. **Discover and keep.** Create a project, draw a map area, acquire local OSM,
   USGS, and terrain fixtures; switch class → formal/informal → terrain colors
   while rendered line cadence remains fixed; place a trailhead with Alt-click,
   lasso a search boundary, start with Enter, observe eager candidate promotion,
   save, arm Add Map Area without camera travel, refresh the corpus without
   unloading the saved trail, and recover the result and color projection after
   restart.
2. **Refine deliberately.** Open and rename a saved trail, drag a support onto
   another branch, prove recomputation occurs after release, verify the native
   window title, undo, redo, cancel without disk mutation, repeat, save, and
   recover the changed geometry after restart.
3. **Compare without lag.** Generate twelve alternatives on a dense graph, pan
   during search and after promotion, zoom across detail, enforce host cadence
   distributions, focus and return to the settled viewport, warm-revise
   parameters, arm a search boundary without camera travel, require and forbid
   segments with undo and redo, stop without discarding promoted results, then
   save a candidate.
4. **Draw from nothing.** Enter the manual editor without search, place a
   partial-edge support, undo and redo, close and reverse a loop, exercise the
   elevation reticle, reject torn editor/focus presentation during save, and
   recover the loop after restart.

The provider server, basemap, terrain, small graph, and dense graph are local,
deterministic fixtures. Network denial is universal. The discovery story
exercises ordinary provider clients against a filesystem Unix socket visible
only inside the disposable `/test` tree.

## Evidence Law

Every consequential step has three separate facts:

1. a native X11 gesture through typed `Story<Observation>` porcelain;
2. a later, frame-coherent observation used only to release the wait;
3. an external oracle such as private project state, pixels, process state, or
   a later cold start.

Temporal eligibility is not causation. A witness route signature may fence a
durable-geometry assertion; it cannot satisfy that assertion.

Before the first injected input, each story verifies
`trailgen-contract::UI_FINGERPRINT`. GUI and acceptance consume the same Target
and wire-state enums. Dynamic Targets such as `Support(1)` are values, not raw
string construction.

The standard witness is one launch-sealed, length-framed semantic journal.
`Probe` walks complete records in order and keeps the newest locally, so brief
states survive polling and no atomic snapshot can disagree with the journal.
An asynchronous publisher keeps serialization and filesystem I/O off the UI
thread.

## Timing Classes

Functional stories run under pinned software graphics with
`ReactionBudget::functional`. This is a bounded progress contract, not a
latency verdict.

The comparison story runs against host graphics. Its
`ReactionBudget::performance` assertions measure from the result-triggering
input through completed product work or the surface-present call.
`FrameProbe` and `CadenceBudget` govern sustained pan and zoom. Neither endpoint
claims compositor scanout, and no timeout or instrumentation multiplier may
dilate a production threshold.

## Execution

`scripts/test-gui` first builds the ordinary uninstrumented release binary and
the acceptance driver, then proves that the product launches to nontrivial
pixels. It next builds the observational feature and runs each story
independently, so one failure has a named scenario and bounded residue. The
host-performance story runs last, after all compilation and deterministic
software-graphics stories, so build load cannot counterfeit product frame
cost.

```console
scripts/test-gui
```

`TRAILGEN_ACCEPTANCE_ARTIFACTS` selects a persistent sink for logs, witness and
frame journals, captures, diagnostics, and explicitly retained private files.
All private-file oracles use confined `Testbed` operations that reject
application-created symlinks.

## Platform Doctrine

X11 is the sole release-tested GUI vertical. It owns private display authority,
native input, capture, surface-present fencing, performance evidence, and
failure artifacts end to end. Wayland expansion is deferred until this
architecture survives another application adoption.

No Trailgen test may reintroduce checked-in xdotool choreography or touch the
live desktop. Missing kernel capabilities are recorded as design defects. The
present frontier is:

- `Ctrl+O` project-deck and native folder-picker cancellation;
- roaming-vector cache fill, offline restart, and provider refresh;
- contour disclosure and cross-scale label stability;
- saved-library hover preview and destructive deletion;
- multi-window/dialog, tray, clipboard/IME, and window move/resize support;
- AccessKit selectors that can replace ordinary hand-authored targets.
