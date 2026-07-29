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
deck. The inspector presents Saved Trails before Find Trails, followed by Map
Areas.

The Library is one projection of the project’s canonical saved-trail store, not
a second collection. Hovering a Library row exposes its prepared miniature and
temporarily previews that trail on the map. Clicking enters `Focus(Saved)`.
The Library’s Rename action and the pencil beside a focused trail’s name open
the same inline rename transaction; `F2` is its keyboard entrance. Renaming
changes metadata in place; deleting removes the canonical trail. Neither
operation changes route identity or transient Results.
Library navigation is inert while Edit owns an unsaved draft; only Save or
Cancel may leave that editor.

## Presentation

| View | Privileged map content | Working shelf | Search artifacts |
| --- | --- | --- | --- |
| `Browse` | hovered saved trail, otherwise candidate portfolio | result tiles | boundary and segment edicts |
| `Focus(Candidate)` | one candidate | its elevation profile | boundary and segment edicts |
| `Focus(Saved)` | one saved trail | its elevation profile | hidden |
| `Edit(*)` | editor realization and support pins | editor elevation profile | hidden |

The editor row is absolute: an invalid draft must not fall through to Results,
Library, Focus, segment edicts, or search-boundary rendering. The last valid
editor realization and profile remain visible while an intermediate draft is
invalid; the fault is shown and Save is disabled. A subsequent valid edit
atomically replaces both projections.

A click on the current realized trail inserts a support at that routed leg.
A click away from it appends a new destination. Dragging replaces one existing
support. These operations alter the ordered support design; they never mutate
provider topology.

Support points are ordered from zero everywhere, including visible pin labels.
For any editable `Open` or `Loop` trail, `Close Loop` changes the design between
those topologies; `Loop` realization routes from the last support back to
support 0. If support 0 lies on a terminal spur, closure may round it to the
nearest endpoint of that same trail segment within 20 m, but only when the
complete rounded design realizes a proper loop. The pin movement and topology
change form one undoable gesture. Closure is transactional: if neither the
exact nor rounded design forms a loop, the toggle remains off and the prior
trail, pins, profile, and Save state remain unchanged while an actionable
notice explains the rejection. Editor provenance never removes this capability.
Reversing a loop preserves support 0 as its trailhead and inverts the exact
realized walk. Shape and reversal changes are ordinary undoable editor gestures.

The elevation profile and map share one route-distance cursor. Hover chooses the
nearest represented profile distance and paints a hollow map ring at the
corresponding route coordinate. Primary click locks that distance; another
primary click moves the lock; secondary click releases it and immediately
restores hover-following. A lock belongs to one focused trail or editor and is
discarded when that owner changes.

## Map Tools

The map-tool lane is subordinate to `Browse`:

```text
Idle | SelectMapArea | DrawSearchBoundary | PlaceTrailhead | DragTrailhead
```

Arming one tool disarms every other tool and leaves Focus. Edit owns primary
click and pin dragging, so no map tool may be armed there. Segment edicts own
plain and Shift-click only in Find mode. Alt-click owns trailhead placement
only when neither Edit, Focus, nor a scribe has the pointer.

The present scribe implementations retain their own gesture data, so exclusivity
is enforced at their arming boundary rather than by one enum. The invariant is
still singular: two active tools are a bug.

## Background Lanes

Search, trail-data mutation, vector acquisition, relief preparation, and
debounced persistence are orthogonal workers.

- Search is `Idle` or `Striking(serial, progress, stopping)`.
- A new search may keep the previous portfolio visible.
- Only events matching the active serial may publish.
- Stopping preserves the previous portfolio.
- Parameter or segment-edict changes schedule a warmed replacement search.
- Search completion installs a prepared portfolio; it does not force Focus.
- Trail-data replacement requests a project reload and invalidates all
  graph-bound session state at the workspace boundary.

Worker progress may change status text and repaint demand. It may not change
the primary view implicitly or perform graph-scale work on the event loop.

## Persistence

The project owns its library, search recipe, downloaded regions, and graph.
XDG state owns the base Browse viewport, result sorting, inspector position,
and section shutters. Candidates, focus, editor drafts, undo history, profile
cursor, map gestures, worker progress, and navigation frames are session state.

Only the base Browse viewport is persisted. Focus and Edit may pan or zoom
without corrupting the viewport to which Back or Cancel returns.

## Transition Laws

| Event | From | To |
| --- | --- | --- |
| open candidate tile | `Browse` | `Focus(Candidate)` |
| open saved Library row | `Browse` | `Focus(Saved)` |
| Back / Escape | `Focus(*)` | prior `Browse` viewport |
| Edit Trail | `Focus(*)` | `Edit(*)` with exact return frame |
| Draw a Trail | `Browse` | `Edit(New)` |
| Cancel / Escape | `Edit(*)` | exact opening view and viewport |
| Save | `Edit(*)` | `Focus(Saved)` |
| previous / next | `Focus(kind)` | adjacent `Focus(kind)` |
| parameter change | Find mode | prior results remain; warmed search scheduled |
| click / Shift-click segment | Find mode | edict toggled; warmed search scheduled |
| Close Loop | `Edit(*)` with `Open | Loop` shape | same editor, shape changed and re-realized |
| Reverse Direction | `Edit(Loop)` | same editor with exact walk inverted |
| Clear Results | `Browse` | `Browse` with no portfolio or edicts |

No transition may discard a return viewport, expose another view’s overlays,
publish a stale worker generation, or enable saving a draft whose visible
supports do not realize the saved route.
