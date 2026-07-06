# Known Limitations

The app is useful today as a local-first route generator over normalized project data, but several seams are intentionally incomplete.

- Discovery recommends source classes and caches explicit URLs or files; it does not yet crawl every regional GIS portal automatically.
- Arbitrary projected GeoTIFFs and rotated/sheared raster DEMs are planned seams, not implemented adapters. Current elevation sampling supports Arc/Info ASCII Grid, north-up WGS84 or EPSG:3857 single-band GeoTIFF DEMs, and simple GDAL VRT wrappers around geographic GeoTIFF DEMs. Shapefile vector support exists for trail-network polylines, terrain overlays, access/closure overlays, and road/hydrology context linework.
- Input vector geometries normalize to geographic lon/lat decimal degrees inside the adapters. Native WGS84/CRS84 and declared EPSG:3857 Web Mercator are supported; other projected CRS inputs are rejected instead of silently ingested.
- Dated access/closure overlays are represented with `active_from`/`active_to` windows and a project planning date. Turn restrictions, hourly rules, recurring seasonal direction rules, and timed reservation systems are not represented. Edges are bidirectional by default, with one-way travel preserved only when GeoJSON or shapefile source attributes prove it.
- The exact backend is a bounded edge-simple enumerator for small graphs, not a MILP/CP-SAT formulation and not a stochastic annealer. Large graphs still rely on the deterministic `LoopHunter` heuristic.
- The map UI is an interactive offline SVG/HTML diagnostic, not a tile-backed planner or editor.
- AllTrails write-back uses only manual-compatible exports. The core deliberately avoids brittle private APIs.
- Terrain inference is transparent but coarse. Explicit tags, overlays, road context, slope, and confidence are preserved; they do not replace field judgment.
- Large real regions will need careful source curation and search-parameter tuning. The fixture demo proves the pipeline, not global performance.

These are product boundaries, not excuses. New work should reduce this list only by adding reproducible code, tests, docs, and demo evidence.
