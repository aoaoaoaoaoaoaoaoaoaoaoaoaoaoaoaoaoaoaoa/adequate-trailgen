# Source Discovery

AOI: west -105.020000, south 39.990000, east -104.980000, north 40.020000

## Coverage

Coverage:
- TrailNetwork (Required): Satisfied; candidates: crates/trailgen-core/tests/fixtures/mini_network.geojson; TrailNetwork source requirement has implemented candidate(s).
- Elevation (Required): Satisfied; candidates: crates/trailgen-core/tests/fixtures/mini_dem.asc; Elevation source requirement has implemented candidate(s).
- Terrain (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/terrain_overlay.geojson; Terrain source requirement has implemented candidate(s).
- Closure (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/closure_overlay.geojson; Closure source requirement has implemented candidate(s).
- Access (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/access_overlay.geojson; Access source requirement has implemented candidate(s).
- Road (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/context_overlay.geojson; Road source requirement has implemented candidate(s).
- Hydrology (Recommended): Satisfied; candidates: crates/trailgen-core/tests/fixtures/context_overlay.geojson; Hydrology source requirement has implemented candidate(s).
- SeedRoute (Optional): Satisfied; candidates: demo/mini-loop/seeds/imports/known-good-kmz-loop.kmz, demo/mini-loop/seeds/imports/known-good-loop.gpx; SeedRoute source requirement has implemented candidate(s).

## Acquisition Plan

### TrailNetwork (Required, Satisfied)

The normalized graph cannot exist without a routable trail network.

Acceptance: LineString or MultiLineString trail geometries covering the AOI, with names, access/surface tags when available, and enough topology to build junctions.

Suggested cache paths:
- sources/trails.geojson
- sources/network.geojson
- sources/trails.shp

Adapter ids:
- geojson-network
- shapefile-network

Search terms:
- official trail GIS line layer
- OSM hiking path extract
- park trail network GeoJSON

Acquisition hints:
- NPS official GIS open data: https://www.nps.gov/subjects/gisandmapping/tools-and-data.htm [GeoJSON, Shapefile, Feature Service]. Use first for National Park Service units; cache exported trail linework under sources/ with provenance intact.
- USFS geospatial data discovery: https://data-usfs.hub.arcgis.com/ [Shapefile, File Geodatabase, Feature Service]. Use for National Forest roads/trails and agency-managed transportation layers before falling back to volunteered data.
- Geofabrik OpenStreetMap extracts: https://download.geofabrik.de/ [OSM PBF, Shapefile]. Use as a broad fallback extract, then filter hiking paths/tracks and normalize to GeoJSON or shapefile.

### Elevation (Required, Satisfied)

Long-day route quality depends on ascent, descent, grade, and sustained steepness.

Acceptance: DEM coverage intersects every trail edge; vertical units and CRS are documented before enrichment.

Suggested cache paths:
- sources/dem.asc
- sources/dem.tif

Adapter ids:
- arc-ascii-elevation
- geotiff-elevation
- vrt-elevation

Search terms:
- USGS 3DEP DEM
- Copernicus DEM
- local elevation raster for hiking area

Acquisition hints:
- USGS The National Map Downloader: https://www.usgs.gov/tools/download-data-maps-national-map [GeoTIFF, IMG]. Use 3DEP DEM products for United States AOIs; prefer GeoTIFF tiles that cover the graph envelope.
- USGS TNMAccess API: https://apps.nationalmap.gov/tnmaccess/ [GeoTIFF, JSON metadata]. Use for scripted 3DEP product discovery by bounding box before caching selected raster downloads.

### Terrain (Recommended, Satisfied)

Terrain multipliers are inspectable only when roughness evidence is explicit instead of magical.

Acceptance: Terrain or surface features can be normalized into known buckets and carry confidence/provenance.

Suggested cache paths:
- sources/terrain.geojson
- sources/landcover.geojson
- sources/terrain.shp

Adapter ids:
- geojson-terrain-overlay
- shapefile-terrain-overlay

Search terms:
- land cover polygons
- trail surface GIS layer
- alpine talus scramble terrain map

Acquisition hints:
- MRLC NLCD data: https://www.mrlc.gov/data [GeoTIFF, Raster service]. Use for broad land-cover evidence, then convert relevant classes into terrain overlays with explicit confidence.
- Agency surface or land-cover GIS: https://data-usfs.hub.arcgis.com/ [Shapefile, GeoJSON, Feature Service]. Prefer local agency surface, land-cover, or trail-condition attributes when available.

### Closure (Recommended, Satisfied)

A beautiful generated loop is trash if it crosses a closed trail or forbidden parcel.

Acceptance: Closure, private, restricted, and open statuses can be attached to graph edges with dated provenance.

Suggested cache paths:
- sources/closures.geojson
- sources/access.geojson
- sources/closures.shp

Adapter ids:
- geojson-closure-overlay
- shapefile-closure-layer

Search terms:
- official trail closure layer
- park access restriction GIS
- seasonal closure boundary GeoJSON

Acquisition hints:
- Agency closure and alert GIS: https://public-nps.opendata.arcgis.com/ [GeoJSON, Shapefile, Feature Service]. Use current official closure/restriction features; preserve dates and alert provenance in cached overlays.
- Local park or forest alerts: https://www.nps.gov/subjects/gisandmapping/tools-and-data.htm [GeoJSON, Shapefile, Web page]. When no machine layer exists, hand-normalize official closure geometry into a small GeoJSON overlay.

### Access (Recommended, Satisfied)

Access and ownership boundaries are distinct from temporary closures and should be visible in route legality diagnostics.

Acceptance: Open, restricted, private, or unknown access statuses can be attached to graph edges with provenance.

Suggested cache paths:
- sources/access.geojson
- sources/ownership.geojson
- sources/access.shp

Adapter ids:
- geojson-access-overlay
- shapefile-access-overlay

Search terms:
- public access boundary GeoJSON
- land ownership parcel open space GIS
- park access status trail layer

Acquisition hints:
- USGS PAD-US data download: https://www.usgs.gov/programs/gap-analysis-project/science/pad-us-data-download [File Geodatabase, Shapefile]. Use protected-area ownership/manager data as access context; normalize to open/restricted/private where justified.
- PAD-US protected areas overview: https://www.usgs.gov/programs/gap-analysis-project/science/protected-areas [Metadata, Download links]. Use to understand PAD-US scope before treating ownership as a route legality signal.

### Road (Recommended, Satisfied)

Road exposure and road crossings are hard constraints for many hikes.

Acceptance: Road context lines cover the AOI and can identify crossings or road-exposed trail segments.

Suggested cache paths:
- sources/roads.geojson
- sources/context-roads.geojson
- sources/roads.shp

Adapter ids:
- geojson-road-context
- shapefile-road-context

Search terms:
- road centerline GeoJSON
- street context lines
- OSM road extract

Acquisition hints:
- USFS roads data: https://data.fs.usda.gov/geodata/edw/datasets.php?dsetCategory=transportation [Shapefile, File Geodatabase, Map service]. Use for National Forest road exposure and crossings; cache centerlines as road context.
- The National Map transportation: https://apps.nationalmap.gov/tnmaccess/ [Shapefile, GeoPackage, JSON metadata]. Use TNM transportation products when local road centerlines are absent.
- Geofabrik OpenStreetMap roads: https://download.geofabrik.de/ [OSM PBF, Shapefile]. Use as a fallback road/street extract, then filter and normalize to context linework.

### Hydrology (Recommended, Satisfied)

Water crossings are route diagnostics, risk signals, and useful report context.

Acceptance: Hydrology linework intersects likely crossings and carries source confidence where known.

Suggested cache paths:
- sources/hydrology.geojson
- sources/streams.geojson
- sources/hydrology.shp

Adapter ids:
- geojson-hydrology-context
- shapefile-hydrology-context

Search terms:
- NHD stream lines
- hydrology GeoJSON
- river creek crossing layer

Acquisition hints:
- USGS National Hydrography products: https://www.usgs.gov/national-hydrography/access-national-hydrography-products [Shapefile, File Geodatabase]. Use NHD/3DHP stream and waterbody linework to infer water crossings.
- The National Map hydrography: https://apps.nationalmap.gov/tnmaccess/ [Shapefile, File Geodatabase, JSON metadata]. Use TNMAccess to locate hydrography products by AOI before caching selected linework.

### SeedRoute (Optional, Satisfied)

Seeds improve confidence/popularity hints and provide validation loops without contaminating the provider-neutral model.

Acceptance: Seed routes snap to the current graph and their provenance is preserved.

Suggested cache paths:
- sources/seeds/completed.gpx
- sources/seeds/completed.csv
- sources/seeds/alltrails-export.gpx
- sources/seeds/reference.geojson
- sources/seeds/app-export.json

Adapter ids:
- gpx-route
- geojson-route
- json-route
- csv-route
- kml-route

Search terms:
- personal completed hike GPX
- AllTrails export GPX
- app route JSON export
- reference route KML

Acquisition hints:
- AllTrails import/export support: https://support.alltrails.com/hc/en-us/sections/360006411352-Importing-and-exporting-files [GPX, GeoJSON, KML, KMZ, CSV]. Use user-supplied exports as seed routes only; never couple core graph semantics to private AllTrails APIs.
- Personal GPS archives: file://local-user-supplied-routes [GPX, GeoJSON, KML, KMZ, CSV]. Cache completed hikes under sources/seeds or import them directly so provenance and fingerprints are preserved.

## Cache Command Sketches

Replace `<artifact-url-or-path>` with a concrete downloaded artifact or local file selected from the listed source surface; keep the explicit kind and adapter when provider filenames are ambiguous.

### TrailNetwork

```sh
trailgen cache-source <project> --input '<artifact-url-or-path>' --output trails.geojson --kind trail-network --adapter geojson-network
```

Primary source surface: NPS official GIS open data (https://www.nps.gov/subjects/gisandmapping/tools-and-data.htm)
Alternate surfaces: USFS geospatial data discovery, Geofabrik OpenStreetMap extracts

### Elevation

```sh
trailgen cache-source <project> --input '<artifact-url-or-path>' --output dem.asc --kind elevation --adapter arc-ascii-elevation
```

Primary source surface: USGS The National Map Downloader (https://www.usgs.gov/tools/download-data-maps-national-map)
Alternate surfaces: USGS TNMAccess API

### Terrain

```sh
trailgen cache-source <project> --input '<artifact-url-or-path>' --output terrain.geojson --kind terrain --adapter geojson-terrain-overlay
```

Primary source surface: MRLC NLCD data (https://www.mrlc.gov/data)
Alternate surfaces: Agency surface or land-cover GIS

### Closure

```sh
trailgen cache-source <project> --input '<artifact-url-or-path>' --output closures.geojson --kind closure --adapter geojson-closure-overlay
```

Primary source surface: Agency closure and alert GIS (https://public-nps.opendata.arcgis.com/)
Alternate surfaces: Local park or forest alerts

### Access

```sh
trailgen cache-source <project> --input '<artifact-url-or-path>' --output access.geojson --kind access --adapter geojson-access-overlay
```

Primary source surface: USGS PAD-US data download (https://www.usgs.gov/programs/gap-analysis-project/science/pad-us-data-download)
Alternate surfaces: PAD-US protected areas overview

### Road

```sh
trailgen cache-source <project> --input '<artifact-url-or-path>' --output roads.geojson --kind road --adapter geojson-road-context
```

Primary source surface: USFS roads data (https://data.fs.usda.gov/geodata/edw/datasets.php?dsetCategory=transportation)
Alternate surfaces: The National Map transportation, Geofabrik OpenStreetMap roads

### Hydrology

```sh
trailgen cache-source <project> --input '<artifact-url-or-path>' --output hydrology.geojson --kind hydrology --adapter geojson-hydrology-context
```

Primary source surface: USGS National Hydrography products (https://www.usgs.gov/national-hydrography/access-national-hydrography-products)
Alternate surfaces: The National Map hydrography

### SeedRoute

```sh
trailgen cache-source <project> --input '<artifact-url-or-path>' --output seeds/completed.gpx --kind seed-route --adapter gpx-route
```

Primary source surface: AllTrails import/export support (https://support.alltrails.com/hc/en-us/sections/360006411352-Importing-and-exporting-files)
Alternate surfaces: Personal GPS archives

## Local Candidates

Candidates:
- crates/trailgen-core/tests/fixtures/access_overlay.geojson: Access via geojson-access-overlay; 542 bytes, sha256 7acb7fd8292df30f79ce3abd53bfa9c944c09cdc8586ccfd81a2ba7228972ff0
- crates/trailgen-core/tests/fixtures/closure_overlay.geojson: Closure via geojson-closure-overlay; 610 bytes, sha256 75032f041c59c46efb0f01802a4cc234b6756a5e7eed2dd79ba1e69fa355aa02
- crates/trailgen-core/tests/fixtures/context_overlay.geojson: Road via geojson-road-context; 694 bytes, sha256 5aace6064e9fb87161fc95d71183e5d639f8eee46b86c3b3c084eba09f0171bd
- crates/trailgen-core/tests/fixtures/context_overlay.geojson: Hydrology via geojson-hydrology-context; 694 bytes, sha256 5aace6064e9fb87161fc95d71183e5d639f8eee46b86c3b3c084eba09f0171bd
- crates/trailgen-core/tests/fixtures/mini_dem.asc: Elevation via arc-ascii-elevation; 169 bytes, sha256 8200f8e889b76ecff72d845148d2476a5b4f75edbf782e6082a5e300a54e0859
- crates/trailgen-core/tests/fixtures/mini_network.geojson: TrailNetwork via geojson-network; 1714 bytes, sha256 eae6f4f939c209fb1ab455e581d4e50f7dffab68d7a6d7f4c9800e65fec64857
- crates/trailgen-core/tests/fixtures/terrain_overlay.geojson: Terrain via geojson-terrain-overlay; 535 bytes, sha256 2136c1627f1898ed1d6524f923e2a1e4edbcaab9cd6058a0cccebb3dfd62e936
- demo/mini-loop/seeds/imports/known-good-kmz-loop.kmz: SeedRoute via kml-route; 2040 bytes, sha256 c4cc6ef457687665c65f34ceb78575ee99b35f24ac87584ada694cee6f34a121
- demo/mini-loop/seeds/imports/known-good-loop.gpx: SeedRoute via gpx-route; 24233 bytes, sha256 aa020e4b300d952cc8e647bb3244be92389aa612f9454f1ba1c9a19bc40a1199

## Adapter Registry

- geojson-network (TrailNetwork, Implemented): consumes geojson, json; produces SegmentDraft, TrailGraph; Provider-neutral LineString and MultiLineString network ingestion.
- shapefile-network (TrailNetwork, Implemented): consumes shp, dbf, shx; produces SegmentDraft, TrailGraph; Official/agency polyline shapefile trail-network ingestion with DBF attribute normalization.
- geojson-route (SeedRoute, Implemented): consumes geojson; produces LineString, snapped route metrics; GeoJSON seed route import.
- json-route (SeedRoute, Implemented): consumes json; produces LineString, snapped route metrics; Provider-neutral route JSON import for coordinate arrays and point-object app exports.
- gpx-route (SeedRoute, Implemented): consumes gpx; produces LineString, snapped route metrics; GPX route import/export, including user-supplied app exports.
- csv-route (SeedRoute, Implemented): consumes csv; produces LineString, snapped route metrics; CSV lon/lat/elevation route import/export for manual app exchange.
- kml-route (SeedRoute, Implemented): consumes kml, kmz; produces LineString, snapped route metrics; KML/KMZ route import/export for manual map-app exchange.
- arc-ascii-elevation (Elevation, Implemented): consumes asc; produces sampled elevation profile, edge ascent/descent; Arc/Info ASCII Grid DEM sampling for local elevation enrichment.
- geotiff-elevation (Elevation, Implemented): consumes tif, tiff; produces sampled elevation profile, edge ascent/descent; North-up geographic single-band GeoTIFF DEM sampling.
- vrt-elevation (Elevation, Implemented): consumes vrt; produces sampled elevation profile, edge ascent/descent; GDAL VRT SimpleSource wrapper around a north-up geographic GeoTIFF DEM.
- geojson-terrain-overlay (Terrain, Implemented): consumes geojson, json; produces terrain overrides, confidence/provenance; GeoJSON land-cover, surface, or user terrain overlays applied after graph construction.
- shapefile-terrain-overlay (Terrain, Implemented): consumes shp, dbf, shx; produces terrain overrides, confidence/provenance; Polygon or line shapefile land-cover, surface, or terrain overlays applied after graph construction.
- geojson-access-overlay (Access, Implemented): consumes geojson, json; produces access overrides, confidence/provenance; GeoJSON access/status overlay applied after graph construction.
- shapefile-access-overlay (Access, Implemented): consumes shp, dbf, shx; produces access overrides, confidence/provenance; Polygon or line shapefile access/status overlay applied after graph construction.
- geojson-closure-overlay (Closure, Implemented): consumes geojson, json; produces access overrides, confidence/provenance; GeoJSON closure/restriction overlay applied after graph construction.
- geojson-road-context (Road, Implemented): consumes geojson, json; produces road crossings, road exposure hints; GeoJSON road/street context lines used to infer trail crossings.
- shapefile-road-context (Road, Implemented): consumes shp, dbf, shx; produces road crossings, road exposure hints; Shapefile road/street centerlines used to infer trail crossings.
- geojson-hydrology-context (Hydrology, Implemented): consumes geojson, json; produces water crossings; GeoJSON stream/river context lines used to infer water crossings.
- shapefile-hydrology-context (Hydrology, Implemented): consumes shp, dbf, shx; produces water crossings; Shapefile stream/river centerlines used to infer water crossings.
- shapefile-closure-layer (Closure, Implemented): consumes shp, dbf, shx; produces access overrides, confidence/provenance; Official park/agency shapefile closure and restriction overlays.
