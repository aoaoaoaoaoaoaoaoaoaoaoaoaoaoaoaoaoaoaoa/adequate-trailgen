# Workbench State Model

The workbench state is a product of four independent axes:

1. one primary `WorkbenchView`;
2. at most one map tool;
3. background operations;
4. durable project and workbench state.

Only the primary view chooses the toolbar, working shelf, privileged trail
overlay, and ordinary map-click meaning. Background work never becomes a view.
Stored candidates never confer focus by their presence alone.

## Names

- A `Trail` is a user-owned support-point design.
- A `Candidate` is a transient generated route with an exact editable design.
- `Search` is an operation. Its retained output is the Results shelf.
- `Focus` is full-map inspection of one candidate or saved trail.
- `Edit` is one support-point editor. Its origin determines save and return
  behavior; it does not create separate rendering implementations.
- `Browse` is the base workbench. The inspector always owns the saved-trail
  Library; the lower shelf always owns transient Results.

“Find mode” therefore means `Browse` or `Focus(Candidate)`, optionally with a
search worker running. `Edit(Candidate)` is an editor, not Find mode.

## Primary View

`WorkbenchView` is a closed sum:

```text
Browse
Focus(Candidate(identity) | Saved(trail_id))
Edit(origin, draft, return_frame)
```

`origin` is `New`, `Candidate`, or `Saved(trail_id)`. A generated and a manually
started editor share routing, interaction, rendering, undo, profile, and save
machinery. Their only differences are initial supports, save destination, and
return target.

The navigation stack has depth at most three:

```text
Browse(base viewport)
  └─ Focus(item, detail viewport)
       └─ Edit(draft, editor viewport)
```

`FocusFrame` owns the Browse viewport while Focus is active.
`EditorReturn` owns the exact view and viewport that opened Edit. Cancel pops
one frame. Leaving Focus pops the remaining frame. Entering another focused
item replaces the top item without changing the stored Browse viewport.
Candidate focus stores the portfolio’s stable identity, never its mutable
ranking slot. A warmed portfolio may reorder a retained candidate without
changing Focus. If it removes that identity, Focus returns to Results; an
editor opened from that candidate instead changes its return target to Results.

Saving Edit enters `Focus(Saved)`. A new manual trail establishes its
pre-editor viewport as the Browse return frame. Editing a candidate or saved
trail preserves the existing Browse return frame.

## Spatial Ownership

The left inspector is durable project memory: saved trails, search intent, and
downloaded map areas. The bottom is transient working memory: search results,
the focused trail’s profile, editor profile, status, and contextual help.
Durable objects must not require switching the bottom shelf into an alternate
deck. The inspector presents Saved Trails before Trail Creator, followed by Map
Areas.

The Library is one projection of the project’s canonical saved-trail store, not
a second collection. Hovering a Library row exposes its prepared miniature and
temporarily previews that trail on the map. Clicking enters `Focus(Saved)`.
The Library’s Rename action and the pencil beside a focused trail’s name open
the same inline rename transaction; `F2` is its keyboard entrance. Renaming
changes metadata in place; deleting removes the canonical trail. Neither
operation changes route identity or transient Results.
Library navigation is inert while Edit owns an unsaved draft; only Save,
Cancel, or explicitly selecting Finder may leave that editor. Selecting Finder
is a visible cancellation and restores the exact editor return frame. Map-area names are metadata keyed by the
content-derived identity of one region snapshot; renaming cannot invalidate or
reacquire its corpus. Resizing replaces that identity transactionally while
preserving the region's inspector slot and moving its name to the successor.

## Presentation

| View | Privileged map content | Working shelf | Search artifacts |
| --- | --- | --- | --- |
| `Browse` | hovered saved trail, otherwise candidate portfolio | result tiles after the first search attempt | boundary and segment edicts |
| `Focus(Candidate)` | one candidate | its elevation profile | boundary and segment edicts |
| `Focus(Saved)` | one saved trail | its elevation profile | hidden |
| `Edit(*)` | editor realization and support pins | editor elevation profile | hidden |

The editor row is absolute: an invalid or realizing draft must not fall through
to Results, Library, Focus, segment edicts, or search-boundary rendering. The
last valid editor realization and profile remain visible while a successor is
realizing or invalid. Realizing shows a named preparing state and disables Save;
failure shows the fault and disables Save. A subsequent valid generation
atomically replaces both projections.

A click on the current realized trail inserts a support at that routed leg.
A click away from it appends a new destination. Dragging replaces one existing
support. Holding Shift marks every support head for deletion; Shift-clicking
one removes it and immediately renumbers its successors. These operations
alter the ordered support design; they never mutate provider topology, and
each is one undoable gesture.

Support points are ordered from zero everywhere, including visible pin labels.
For any editable `Open` or `Loop` trail, `Close Loop` changes the design between
those topologies; `Loop` realization routes from the last support back to
support 0. `Loop` is authored intent: its sole topological promise is a lawful
closed walk. The realization may revisit vertices or retrace edges, and its
measured morphology may therefore be `Loop`, `FigureEight`, or `OutAndBack`.
Closure preserves every support exactly, including support 0; it never rounds
or silently moves a pin. Closure remains transactional: if no lawful return
exists, the toggle remains off and the prior trail, pins, profile, and Save
state remain unchanged while an actionable notice explains the rejection.
Editor provenance never removes this capability.
Reversing a loop preserves support 0 as its trailhead and inverts the exact
realized walk. It first reverses the existing support tail. Where those controls
would select another walk, it retains them and adds a compact set of visible
supports that compels the exact reversal; the editor reports the added count.
Any one-way segment rejects reversal without changing the draft. Shape and
reversal changes are ordinary undoable editor gestures.

The elevation profile and map share one route-distance cursor. Hover chooses the
nearest represented profile distance and paints a hollow map ring at the
corresponding route coordinate. Primary click locks that distance; another
primary click moves the lock; secondary click releases it and immediately
restores hover-following. A lock belongs to one focused trail or editor and is
discarded when that owner changes.

Opening a shelf or profile must not crop the map extent that preceded it. A
contracting canvas zooms out just enough to retain that extent; removing the
surface reveals more map without an inverse zoom. Explicit Focus fitting and
user navigation still take precedence.

## Map Tools

The map-tool lane is subordinate to `Browse`:

```text
Idle | SelectMapArea | AdjustMapArea(region, corner) |
DrawSearchBoundary | PlaceTrailhead | DragTrailhead
```

Arming one tool disarms every other tool and dissolves Focus in place: the
current detail camera becomes the Browse camera and the obsolete return frame
is discarded. Ordinary Back still restores the camera that preceded Focus.
Edit owns primary click and pin dragging, so no map tool may be armed there.
Segment edicts own plain and Shift-click only in Find mode. Alt-click owns
trailhead placement only when neither Edit, Focus, nor a scribe has the
pointer.

The present scribe implementations retain their own gesture data, so exclusivity
is enforced at their arming boundary rather than by one enum. The invariant is
still singular: two active tools are a bug.

Map-area gestures publish their geometry before acquisition. A completed area
selection enters the inspector and map immediately as desired project state;
its trail-data worker cannot withhold that acknowledgement. Dragging a corner
shows a live rectangular preview, then validates and replaces the region on
release. An invalid drag restores the original rectangle. Selection and
adjustment preserve the currently presented basemap and routable corpus until
their prepared successors are atomically installed.

## Background Lanes

Search, editor realization, trail-data mutation, vector acquisition, relief
preparation, and debounced persistence are orthogonal workers.

Opening a ready project publishes its native shell before decoding the graph
or forging routing, spatial, and rendering projections. There is no surrogate
loading workspace: the real `TrailApp` immediately owns the library, recipe,
regions, camera, and basemap, while one worker forges the optional graph
armament. Publication installs that capability into the same workbench without
replacing its live state. The witness names this capability deficit
`Workspace::Preparing`; it is not a sibling workspace, trail-data acquisition,
or Survey. Library edits, saved-trail focus, and camera motion remain valid and
durable throughout preparation. Graph-dependent gestures remain visibly
disabled until armament arrives.
An unfinished manual design is published with this shell: its pins and editing
viewport do not depend on the graph. Armament later realizes the same design in
place. An automatic provider refresh waits until that editor is saved or
cancelled, because installing a successor graph may not evict authored work.

- Search is `Idle` or `Striking(serial, progress, stopping)`.
- Editor realization is `Idle` or `Realizing(serial)`. Each support, shape,
  undo, or redo gesture supersedes the prior serial. Dragging may coalesce
  pointer samples, but release must publish a generation for the visible pin
  set. Save is disabled while realizing. Undo, redo, another edit, Cancel, and
  leaving Manual never wait for an obsolete generation; its eventual result is
  discarded. The prior valid route and profile remain visible until one current
  generation atomically replaces them.
- The Results shelf is dormant until a valid Find Trails action launches a search.
  Clearing Results or replacing the trail corpus makes it dormant again.
- A new search may keep the previous portfolio visible.
- Only events matching the active serial may publish.
- Stopping preserves the previous portfolio.
- Parameter or segment-edict changes schedule a warmed replacement search.
- Segment edicts own one bounded undo log; undo and redo are routed to the
  editor only while Edit owns the workbench.
- Search completion installs a prepared portfolio; it does not force Focus.
- Trail-data mutation first commits desired region state, then acquires and
  prepares the successor corpus off-thread. The current graph, basemap, relief,
  and camera remain active throughout preparation; clearing visible map state
  is not a loading state. Installation is one bounded ownership swap. A
  saved-trail Focus and its exact camera survive because the saved design owns
  its geometry independently of the graph. Candidate Focus dissolves in place
  because candidate identity belongs to the old graph.

Worker progress may change status text and repaint demand. It may not change
the primary view implicitly or perform graph-scale work on the event loop.

## Persistence

The project owns its library, search recipe, downloaded regions, civic areas,
and graph. Civic areas are durable named context, independent of the primary
view and routing graph. Adding one never moves the camera; clicking its ready
inspector row is an explicit fit command. XDG state owns only the Overlays
shutter, while completion text, suggestions, acquisition progress, hover, and
prepared render projections remain session state.
Search intent is geographic and graph-independent: trailhead, boundary,
distance and moving-time windows, climb window, lower-limb-load target, shape,
and segment edicts. Graph vertex IDs and solver frontier controls never enter
durable UI state.
XDG state owns the base Browse viewport, result sorting, inspector position,
section shutters, trail-color projection, and an unfinished `Edit(New)` design
once it has at least one pin. The manual draft contains only its committed
name, shape, ordered support points, and editing viewport; route realization is
reconstructed from the project graph. It is cleared by Save, Cancel, or
selecting Finder. Candidate and saved-trail editor drafts, candidates, focus,
undo history, profile cursor, map gestures, worker progress, and navigation
frames are session state.

Only the base Browse viewport is persisted. Focus and Edit may pan or zoom
without corrupting the viewport to which Back or Cancel returns.

## Transition Laws

| Event | From | To |
| --- | --- | --- |
| open candidate tile | `Browse` | `Focus(Candidate)` |
| open saved Library row | `Browse` | `Focus(Saved)` |
| Back / Escape | `Focus(*)` | prior `Browse` viewport |
| Edit Trail | `Focus(*)` | `Edit(*)` with exact return frame |
| select Manual | `Browse` | `Edit(New)` |
| select Finder | `Edit(*)` | exact return view and viewport, discarding the draft |
| Cancel / Escape | `Edit(*)` | exact opening view and viewport |
| Save | `Edit(*)` | `Focus(Saved)` |
| restart with unfinished manual pins | process launch | `Edit(New)` at the editing viewport; realization follows graph armament |
| previous / next | `Focus(kind)` | adjacent `Focus(kind)` |
| parameter change | Find mode | prior results remain; warmed search scheduled |
| click / Shift-click segment | Find mode | edict toggled; warmed search scheduled |
| arm a map tool | `Focus(*)` | `Browse` at the current detail viewport |
| complete map-area selection | `Browse + SelectMapArea` | `Browse + desired region visible + acquisition running` |
| drag map-area corner | `Browse + Idle` | `Browse + AdjustMapArea`, then replacement acquisition or exact rollback |
| refresh trail data | `Focus(Saved)` | same focus and viewport while a successor corpus is prepared and installed |
| Shift-click support | `Edit(*)` | same editor with that support removed and successors renumbered |
| Close Loop | `Edit(*)` with `Open | Loop` shape | same editor, shape changed and re-realized |
| Reverse Direction | `Edit(Loop)` | same editor with exact walk inverted |
| Clear Results | `Browse` | `Browse` with no portfolio or edicts |

No transition may discard a return viewport, expose another view’s overlays,
publish a stale worker generation, or enable saving a draft whose visible
supports do not realize the saved route.
