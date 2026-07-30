# GUI Acceptance

Trailgen’s executable GUI contract lives in `trailgen-acceptance`. Each
scenario drives the optimized product through native display-server input,
uses a one-way witness only for targeting and synchronization, and reaches a
durable or rendered oracle before passing. Tests never call product internals.

## Standing Stories

1. **Discover and keep.** Create a project in the project deck, draw a map
   area, acquire the local OSM, USGS, and terrain providers, place a trailhead
   with Alt-click, draw a free-hand boundary, start search with Enter, observe
   eager promotion, save a candidate, and recover it after restart.
2. **Refine deliberately.** Open and rename a saved trail, drag a support onto
   another branch, prove live route recomputation, undo, redo, cancel without
   disk mutation, repeat, save, and recover the refined design after restart.
3. **Compare without lag.** Generate twelve alternatives on a dense graph,
   pan during search and after promotion, zoom across detail, enforce cadence
   distributions, focus and return to the exact settled viewport, warm-revise
   parameters, require and forbid segments, stop without losing promoted
   results, and save the chosen trail.
4. **Draw from nothing.** Enter the manual editor without search, place a
   partial-edge support, undo and redo, close and reverse a loop, hover, lock,
   and release the elevation reticle, save, and recover the loop after restart.

The provider server, basemap, terrain, small graph, and dense graph are local,
deterministic fixtures inside the testbed. Network denial is the default;
story 1 grants host networking only to its loopback fixture server.

## Evidence Law

Every step has three separate facts:

1. an `X11Session` gesture;
2. a causally fresh, final-pass witness observation;
3. an external oracle such as private project state, pixels, process state, or
   a later cold start.

`PerformanceBudget` governs reaction latency. `FrameProbe` and
`CadenceBudget` govern sustained interaction. Smoothed product kinetics must
cross `JsonProbe::wait_stable` before a viewport or similar value becomes a
baseline. Functional timeouts never enlarge performance budgets.

Run the complete contract with `scripts/test-gui`. On failure, inspect the
retained transcript, witness, frame journal, logs, private filesystem, and
screenshot under `TRAILGEN_ACCEPTANCE_ARTIFACTS`.

## Coverage Frontier

The former xdotool scripts are deleted; no Trailgen test may revive coordinate
scripts against the live desktop. The following useful stories remain
unwritten on the present harness:

- `Ctrl+O` project-deck round trip;
- roaming-vector cache fill, offline restart, and provider refresh;
- cartographic disclosure and label stability through contour detail bands;
- saved-library hover preview and destructive delete confirmation.

The shared tester still lacks generic Wayland input, native window
move/resize and multi-window or tray choreography, accessibility-tree
selectors, clipboard/IME and non-Latin-1 text, and a serializable recording
timeline. Native folder-picker cancellation is blocked specifically on the
multi-window/dialog gap. These are tester design defects; see its adoption
guide. Until each is implemented, a Trailgen story needing it must remain
explicitly parked rather than falling back to xdotool.
