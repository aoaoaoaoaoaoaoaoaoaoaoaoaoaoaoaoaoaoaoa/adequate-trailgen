# Installation

`adequate-trailgen` is a Rust workspace with no mandatory external GIS binary for the current CLI, fixtures, and demo. Install a Rust toolchain at or above the workspace `rust-version` in `Cargo.toml`, then build or run with Cargo:

```sh
cargo build --workspace
cargo run -p trailgen -- --help
```

Useful verification gates:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

Runtime inputs are ordinary local project files. `cache-source` can download `http://` or `https://` URLs through `reqwest` with rustls TLS, and `acquire-osm` can post bbox-scoped Overpass queries for OSM XML trail, road, hydrology, or combined extracts. Network access is needed only when acquiring remote source artifacts. GPX, GeoJSON, route JSON, KML, KMZ, CSV, OSM XML/PBF, shapefile vector layers, declared EPSG:3857 Web Mercator vector reprojection, Arc/Info ASCII Grid, affine WGS84/EPSG:3857/WGS84 UTM GeoTIFF DEMs, and simple local GDAL VRT wrappers around those DEMs are implemented in Rust crates; no GDAL install is required for those paths.

Future adapters for arbitrary projected rasters, OSM planet-diff workflows, turn restrictions, or agency APIs may introduce optional system dependencies. If that happens, keep them behind an adapter seam or feature flag and document the command that materializes normalized project artifacts under `sources/` or `cache/`. The current OSM XML/PBF way and route-relation adapters, Overpass XML acquisition, and VRT DEM adapters are pure Rust; VRT supports full-raster identity `SimpleSource` wrappers with affine WGS84, EPSG:3857, or WGS84 UTM `GeoTransform` sampling.
