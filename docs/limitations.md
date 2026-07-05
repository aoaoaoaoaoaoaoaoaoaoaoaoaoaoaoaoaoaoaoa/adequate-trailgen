# Known Limitations

The app is useful today as a local-first route generator over normalized project data, but several seams are intentionally incomplete.

- Discovery recommends source classes and caches explicit URLs or files; it does not yet crawl every regional GIS portal automatically.
- Projected GeoTIFFs and rotated/sheared raster DEMs are planned seams, not implemented adapters. Current elevation sampling supports Arc/Info ASCII Grid, north-up geographic single-band GeoTIFF DEMs, and simple GDAL VRT wrappers around those GeoTIFF DEMs. Shapefile vector support exists for trail-network polylines, terrain overlays, access/closure overlays, and road/hydrology context linework.
- Input geometries are assumed to be geographic lon/lat decimal degrees. There is no CRS detection or reprojection layer yet.
- Turn restrictions and time/seasonal direction rules are not represented. Edges are bidirectional by default, with one-way travel preserved only when GeoJSON or shapefile source attributes prove it.
- The current solver is a bounded deterministic heuristic behind `RouteSolver`, not a MILP/CP-SAT backend and not yet a stochastic annealer.
- The local map UI is not implemented; diagnostics are CLI, JSON/GeoJSON, GPX/KML/KMZ, and Markdown reports.
- AllTrails write-back uses only manual-compatible exports. The core deliberately avoids brittle private APIs.
- Terrain inference is transparent but coarse. Explicit tags, overlays, road context, slope, and confidence are preserved; they do not replace field judgment.
- Large real regions will need careful source curation and search-parameter tuning. The fixture demo proves the pipeline, not global performance.

These are product boundaries, not excuses. New work should reduce this list only by adding reproducible code, tests, docs, and demo evidence.
