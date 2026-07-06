# Known Limitations

The app is useful today as a local-first route generator over normalized project data, but several seams are intentionally incomplete.

- Discovery recommends source classes, emits executable cache/acquire sketches, caches explicit URLs or files, and can acquire bbox-scoped OSM XML through Overpass. It does not yet crawl every regional GIS portal automatically.
- Arbitrary projected GeoTIFF DEMs remain a planned seam. Current elevation sampling supports Arc/Info ASCII Grid, affine WGS84/NAD83, EPSG:3857, or WGS84/NAD83 UTM (EPSG:326xx/327xx/269xx) single-band GeoTIFF DEMs including rotated/sheared `ModelTransformationTag` rasters, and full-raster identity GDAL VRT wrappers around those GeoTIFF DEMs with affine `GeoTransform` sampling. Shapefile vector support exists for trail-network polylines, terrain overlays, access/closure overlays, and road/hydrology context linework.
- OSM XML/PBF ingestion normalizes walkable ways into graph drafts and preserves hiking/foot/walking route-relation membership as way provenance/confidence evidence. Overpass acquisition can materialize XML extracts for implemented trail, road, and hydrology profiles. The app does not yet consume OSM turn restrictions or planet-diff workflows.
- Input vector geometries normalize to geographic lon/lat decimal degrees inside the adapters. Native WGS84/NAD83/CRS84, declared EPSG:3857 Web Mercator, and WGS84/NAD83 UTM (EPSG:326xx/327xx/269xx) are supported; other projected CRS inputs are rejected instead of silently ingested.
- Dated, recurring seasonal, and recurring weekday access/closure overlays are represented with `active_from`/`active_to`, `seasonal_from`/`seasonal_to`, `weekdays`/`day_of_week`, and a project planning date. Turn restrictions, hourly rules, seasonal direction rules, and timed reservation systems are not represented. Edges are bidirectional by default, with one-way travel preserved only when source attributes prove it.
- The exact generation backend is a bounded edge-simple enumerator for small graphs. `formulate-milp` can export a connected simple-loop LP/MILP formulation and `import-milp-solution` can ingest selected directed-arc incumbents from an external solver, but the CLI does not yet invoke MILP/CP-SAT solving itself. Large graphs use the deterministic k-shortest-closure `LoopHunter` heuristic, not a stochastic annealer.
- The map UI is an interactive offline SVG/HTML diagnostic, not a tile-backed planner or editor.
- AllTrails write-back uses only manual-compatible exports. The core deliberately avoids brittle private APIs.
- Terrain inference is transparent but coarse. Explicit tags, overlays, road context, slope, and confidence are preserved; they do not replace field judgment.
- Large real regions will need careful source curation and search-parameter tuning. The fixture demo proves the pipeline, not global performance.

These are product boundaries, not excuses. New work should reduce this list only by adding reproducible code, tests, docs, and demo evidence.
