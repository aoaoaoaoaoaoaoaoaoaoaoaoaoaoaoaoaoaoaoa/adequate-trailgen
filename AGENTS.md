adequate_trailgen: Trailgen, native workbench for finding, drawing, inspecting, keeping long-day hiking routes

## gate
- Feature done only after `scripts/install-local.sh` installs release build and PATH-resolved `trailgen` is verified; install failure blocks release

## invariants
- GUI = product frontend; CLI = debug frontend; one engine; GUI semantics win where frontends diverge
- Renderer work incomplete until many-candidate dogfood trace and its visual states are checked
- Acceptance scenarios are full user stories; xdotool choreography forbidden
- CI: thin scheduler over app-owned, independently runnable evidence units
- Native host and optional inspector live in pinned `eternalist-apps`; never recreate local event-loop, water-composition, trace-spine, inspector-layout rivals
- Never manufacture shared machinery before its common law is proved

## routes
- Changing map, trail, gallery, annotation, or GPU code → `docs/rendering.md`
- Changing workbench navigation, tools, search sessions, overlays, persistence → `spec/UI_STATE_MODEL.md`
- Changing startup publication, event loop, background drains, frame production, latency evidence → `$eternalist-apps`; keep evidence in `docs/rendering.md`
- Changing pedestrian ingestion, graph topology, support binding, routing, routing realms → `docs/routing.md`
- Changing critical GUI behavior or its witness surface → `spec/GUI_ACCEPTANCE.md`
- Changing CI, release gates, platforms, install, removal → `docs/releasing.md`
- Extracting lifecycle, app primitives, physical controls, tester machinery → `$eternalist-apps` plus current Apps, Poolrooms, egui-tester contracts
