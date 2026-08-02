# Trailgen Agent Guidance

The GUI is the product frontend; the CLI is its debug frontend. Preserve one
engine and prefer GUI semantics whenever frontend concerns diverge.

Before changing visible controls, labels, legends, navigation, or feedback,
load and obey `$ui-doctrine`.

Before changing map, trail, gallery, annotation, or GPU code, read and obey
[RENDERING_HYGIENE.md](RENDERING_HYGIENE.md). Renderer work is incomplete until
the many-candidate dogfood trace and its visual states have been checked.

Before changing workbench navigation, tools, search sessions, overlays, or
persistence, read and obey [spec/UI_STATE_MODEL.md](spec/UI_STATE_MODEL.md).

Before changing startup publication, event-loop work, background drains, frame
production, or latency evidence, read and obey
[design/RESPONSIVENESS_DOCTRINE.md](design/RESPONSIVENESS_DOCTRINE.md).

Before changing pedestrian ingestion, graph topology, support binding, routing,
or routing realms, read and obey
[design/URBAN_WALK_ROUTING.md](design/URBAN_WALK_ROUTING.md).

Before changing a critical GUI behavior or its witness surface, read and obey
[spec/GUI_ACCEPTANCE.md](spec/GUI_ACCEPTANCE.md). Acceptance scenarios are
full user stories; xdotool choreography is forbidden.

Before changing CI, release gates, supported platforms, installation, or
removal, read and obey [design/CI_DOCTRINE.md](design/CI_DOCTRINE.md). CI is a
thin scheduler over app-owned, independently runnable evidence units.

Before extracting shared application-shell, contract, accessibility, or tester
porcelain machinery, read
[design/FLEET_APP_ARCHITECTURE.md](design/FLEET_APP_ARCHITECTURE.md). The
terminal architecture is evidence-gated; do not manufacture deferred crates or
macros before their promotion gates are satisfied.

A feature is not done until its release build has been installed locally with
`scripts/install-local.sh` and the `trailgen` executable resolved through
`PATH` has been verified. An installation failure is a release blocker.
