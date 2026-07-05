# Known Limitations

The app is useful today as a local-first route generator over normalized project data, but several seams are intentionally incomplete.

- Discovery recommends source classes and caches explicit URLs or files; it does not yet crawl every regional GIS portal automatically.
- GeoTIFF/VRT DEMs and shapefiles are planned seams, not implemented adapters. Current elevation sampling supports Arc/Info ASCII Grid.
- Input geometries are assumed to be geographic lon/lat decimal degrees. There is no CRS detection or reprojection layer yet.
- Graph directionality is not represented. Edges are bidirectional unless a future adapter adds a proven one-way travel model.
- The current solver is a bounded deterministic heuristic behind `RouteSolver`, not a MILP/CP-SAT backend and not yet a stochastic annealer.
- The local map UI is not implemented; diagnostics are CLI, JSON/GeoJSON, GPX/KML/KMZ, and Markdown reports.
- AllTrails write-back uses only manual-compatible exports. The core deliberately avoids brittle private APIs.
- Terrain inference is transparent but coarse. Explicit tags, overlays, road context, slope, and confidence are preserved; they do not replace field judgment.
- Large real regions will need careful source curation and search-parameter tuning. The fixture demo proves the pipeline, not global performance.

These are product boundaries, not excuses. New work should reduce this list only by adding reproducible code, tests, docs, and demo evidence.
