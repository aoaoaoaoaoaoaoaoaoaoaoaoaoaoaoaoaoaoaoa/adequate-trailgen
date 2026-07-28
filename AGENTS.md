# Trailgen Agent Guidance

The GUI is the product frontend; the CLI is its debug frontend. Preserve one
engine and prefer GUI semantics whenever frontend concerns diverge.

Before changing map, trail, gallery, annotation, or GPU code, read and obey
[RENDERING_HYGIENE.md](RENDERING_HYGIENE.md). Renderer work is incomplete until
the many-candidate dogfood trace and its visual states have been checked.
