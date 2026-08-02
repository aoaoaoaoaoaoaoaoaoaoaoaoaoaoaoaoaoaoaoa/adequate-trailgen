# Known Limitations

- The automatic US corpus currently combines OSM/Overpass, USGS National Digital Trails, and admitted New York and Texas state-park authorities. Each additional authority still needs provider-native lifecycle, walking-use, access, licensing, and acceptance evidence.
- OSM ingestion admits a broad pedestrian graph for manual design and a recreational projection for Finder. Simple node-via foot turn restrictions are enforced; complex via-way or conditional restrictions, non-US implicit-access defaults, historical diffs, live permits, quotas, and booking inventory are not.
- Input vectors normalize to geographic longitude/latitude. Native WGS84/NAD83/CRS84, declared EPSG:3857, and WGS84/NAD83 UTM are supported; other projected CRSs are rejected rather than guessed.
- The exact solver is a bounded edge-simple enumerator for small graphs. Large graphs use the deterministic `LoopHunter` heuristic. There is no parallel MILP export/import backend.
- The manual editor changes a support-point trail design, not provider topology. It cannot synthesize safe bushwhack corridors from blank land cover.
- Automatic elevation uses bounded-zoom bare-earth Mapzen terrain. Raster voids and bathymetric outliers are rejected, but bridge decks may inherit ground or water elevation beneath them.
- GPX is the sole product export. Export requires a saved trail and uses a manual AllTrails handoff; there is no direct account integration, private API automation, activity upload, or timestamp fabrication.
- Terrain inference is transparent but coarse. Explicit tags, overlays, roads, slope, and confidence do not replace field judgment.
- Large regions can still exceed practical search envelopes. The workbench bounds acquisition rectangles and solver work, but it cannot make every continental-scale query useful.

These are product boundaries, not shadow CLI opportunities. A new capability belongs in the shared application engine and GUI first; the debug shell may project it later when a concrete need exists.
