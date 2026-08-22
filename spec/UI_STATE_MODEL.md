# Workbench State Model

The workbench state is a product of five independent axes:

1. one primary `WorkbenchView`;
2. one session-only creator selection, `Neutral | Finder`;
3. at most one map tool;
4. background operations;
5. durable project and workbench state.

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
- `CreatorMode` is `Neutral` or `Finder`. It chooses whether the durable search
  recipe and its map artifacts are exposed; `Neutral` is the launch default.
- A `Command` is a typed application consequence with one stable declaration.
  A map or widget `Gesture` is target-relative interaction, not a Command.

“Find mode” therefore means `CreatorMode::Finder` together with `Browse` or
`Focus(Candidate)`, optionally with a search worker running. `Edit(Candidate)`
is an editor, not Find mode.

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
`EditorReturn` owns the exact view and viewport that opened Edit. Discard pops
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
the same inline rename transaction; `F3` is its keyboard entrance. Renaming
changes metadata in place; deleting removes the canonical trail. Neither
operation changes route identity or transient Results.
The `➚` control beside a Library name exports that exact durable trail as GPX.
It neither enters Focus nor changes the active view. Export is unavailable
while Edit owns an unsaved draft, and candidates have no export control: Save
is the sole transition that grants export authority. Destination choice is a
deliberate native dialog; GPX serialization and filesystem work occur on the
saved-trail export worker, whose completion changes only status and the export
receipt.
The eye beside Export latches that saved trail onto the project map without
entering Focus. Latched trails remain visible beneath Browse, Focus, and every
Edit origin at 0.5 opacity. Their activation order consumes the cartographic
comparison cycle; removing one compacts the cycle. Hovering an unlatched row
uses the next unconsumed hue at full opacity. Hovering a latched row preserves
its assigned hue and raises only its opacity to full. Visibility is session
state, survives an in-process trail-data reload, and disappears when its trail
is deleted. A comparison trail composites the union of its visible geometry
once, so coincident segments do not accumulate opacity.
Library navigation and Finder selection are inert while Edit owns an unsaved
draft; only Save or the explicit danger-colored Discard control may leave that
editor. Discard restores the exact editor return frame. Map-area names are metadata keyed by the
content-derived identity of one region snapshot; renaming cannot invalidate or
reacquire its corpus. Resizing replaces that identity transactionally while
preserving the region's inspector slot and moving its name to the successor.

## Keyboard Ownership

Each workspace routes input through one command canon before rendering its
controls. A command declaration owns its stable identity, scope, default
accelerators, optional `Alt` mnemonic, text-focus policy, label, and consequence
description. Buttons and the generated command guide read that declaration;
they may not maintain parallel shortcut prose. The canon exposes effective
bindings as the future custom-keymap seam, so persisted remapping can replace
defaults without changing application consequences or help rendering.

The command guide and Settings sheet are top modal layers. `F1` always toggles
the guide; `?` defers to focused text entry. `F2` or the platform primary
modifier plus comma toggles Settings from every workspace. While either is
open, it suspends command routing and target-relative keys, and `Escape` closes
only the top layer. No command may bleed through an opening or closing stroke.
Rename fields and other editors retain ordinary text ownership according to
each command's declared text-focus policy.
The inspector begins with `TRAILGEN`, Help, and Settings in one application
header above its panels. It never levies a window-wide top display. The active
project name and the command that opens the project deck belong together in
the first `Projects` panel.

The inspector has one active logical panel. Pointer engagement makes a panel
active. `Tab` and `Shift+Tab` cycle only through that panel, including its
disclosure header. Physical `Control+Tab` and `Control+Shift+Tab` move focus to
the next or previous panel header on every platform; “physical” distinguishes
this chord from the platform-primary modifier. Focus outside the inspector is
never captured into its loop.

Enter, Space, arrows, wheel input, map clicks, drags, and similar interactions
remain target-relative gestures unless they name an application consequence.
They appear in contextual guide sections but never enter global routing.
Plain `E` opens the focused trail editor and is rendered by the standard
single-letter command-button legend. `Alt+Delete` is the sole editor-discard
shortcut. `Escape` never leaves Edit. Saved-trail deletion first opens an
explicit confirmation menu; no first stroke or click removes durable data.

## Presentation

| View | Privileged map content | Working shelf | Search artifacts |
| --- | --- | --- | --- |
| `Browse` | latched saved trails and hovered saved trail, otherwise candidate portfolio | result tiles after the first search attempt | only while Finder is selected |
| `Focus(Candidate)` | latched saved trails beneath one candidate | its elevation profile | only while Finder is selected |
| `Focus(Saved)` | latched saved trails beneath one saved trail | its elevation profile | hidden |
| `Edit(*)` | latched saved trails beneath the editor realization and support pins | editor elevation profile | hidden |

The editor row is absolute: an invalid or realizing draft must not fall through
to Results, Library, Focus, segment edicts, or search-boundary rendering. The
last valid editor realization and profile remain visible while a successor is
realizing or invalid. Realizing shows a named preparing state and disables Save;
failure anchors a plain-language fault to the responsible support and disables
Save. A subsequent valid generation atomically replaces both projections and
clears the fault.

A click on the current realized trail inserts a support at that routed leg.
A click away from it appends a new destination. Dragging replaces one existing
support. Holding Shift marks every support head for deletion; Shift-clicking
one removes it and immediately renumbers its successors. These operations
alter the ordered support design; they never mutate provider topology, and
each is one undoable gesture.

Alt-clicking a support toggles a coordinate callout anchored to that pin. The
callout is transient editor presentation: it follows pin movement and ordered
design edits, does not enter undo history, and is never persisted with the
trail. Showing it also copies the displayed latitude and longitude.

Alt-clicking any unclaimed map ground places one session-only coordinate probe.
The probe is world-anchored, follows pan and zoom, and replaces its predecessor.
Its exact latitude and longitude are displayed and copied immediately; it is
never project or workbench state.

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
Segment edicts own plain and Shift-click only in Find mode. Alt-click owns a
pin's coordinate callout in Edit and otherwise owns the coordinate probe.
Trailhead placement requires the explicit Place on Map tool; an existing
trailhead pin remains directly draggable.

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
discarded, because installing a successor graph may not evict authored work.

- Search is `Idle` or `Striking(serial, progress, stopping)`.
- Editor realization is `Idle` or `Realizing(serial)`. Each support, shape,
  undo, or redo gesture supersedes the prior serial. Dragging may coalesce
  pointer samples, but release must publish a generation for the visible pin
  set. Save is disabled while realizing. Undo, redo, another edit, and Discard
  never wait for an obsolete generation; its eventual result is
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
durable UI state. `CreatorMode` is session state and always launches `Neutral`;
the durable trailhead and search artifacts remain hidden until Finder is
explicitly selected.
XDG configuration owns app-wide Base Pace through one strict typed ledger.
Unknown keys and invalid values block mutation without rewriting the file;
Settings names the fault and offers explicit reload after repair. Stored route
metrics remain the population Wood estimate; GUI moving-time readouts and
time-window constraints are projections through Base Pace. XDG state owns the base Browse viewport,
result sorting, inspector position,
section shutters, trail-color projection, and an unfinished `Edit(New)` design
once it has at least one pin. The manual draft contains only its committed
name, shape, ordered support points, and editing viewport; route realization is
reconstructed from the project graph. It is cleared only by Save or explicit
Discard. Candidate and saved-trail editor drafts, candidates, focus,
undo history, profile cursor, map gestures, worker progress, and navigation
frames are session state.

Only the base Browse viewport is persisted. Focus and Edit may pan or zoom
without corrupting the viewport to which Back or Discard returns.

## Transition Laws

| Event | From | To |
| --- | --- | --- |
| open candidate tile | `Browse` | `Focus(Candidate)` |
| open saved Library row | `Browse` | `Focus(Saved)` |
| Back / Escape | `Focus(*)` | prior `Browse` viewport |
| Edit | `Focus(*)` | `Edit(*)` with exact return frame |
| select Manual | `Browse` | `Edit(New)` |
| toggle Finder | `Browse` or `Focus(*)` | same view with `CreatorMode` toggled |
| Discard / `Alt+Delete` | `Edit(*)` | exact opening view and viewport |
| Escape | `Edit(*)` | unchanged |
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
| Alt-click support | `Edit(*)` | same editor with that pin's coordinate callout toggled |
| Alt-click map ground | any trail view | same view with a transient coordinate probe |
| Close Loop | `Edit(*)` with `Open | Loop` shape | same editor, shape changed and re-realized |
| Reverse Direction | `Edit(Loop)` | same editor with exact walk inverted |
| Clear Results | `Browse` | `Browse` with no portfolio or edicts |

No transition may discard a return viewport, expose another view’s overlays,
publish a stale worker generation, or enable saving a draft whose visible
supports do not realize the saved route.
