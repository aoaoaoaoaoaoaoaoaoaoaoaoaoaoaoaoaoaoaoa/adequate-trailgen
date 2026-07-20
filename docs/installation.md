# Installation

`adequate-trailgen` is a Rust workspace with no mandatory external GIS binary for the CLI, GUI, fixtures, or demo. The core retains the workspace baseline in `Cargo.toml`; the native application declares its newer Poolrooms toolchain floor in the application manifests. Install a compatible toolchain, then install the unified release binary locally:

```sh
cargo build --workspace
./scripts/install-local.sh
trailgen --help
trailgen gui demo/mini-loop
```

The installer uses `cargo install --locked --force` and writes `trailgen` beneath `${TRAILGEN_INSTALL_ROOT:-$HOME/.local}/bin`. Pass another root as its first argument for an isolated installation, such as `./scripts/install-local.sh /tmp/trailgen-install`. Running `trailgen` without a subcommand opens the project in the current directory; all provenance-sensitive command surfaces remain subcommands of the same binary.

The native workbench needs a functioning Vulkan or OpenGL graphics stack and an X11 or Wayland session. It streams USGS topographic tiles into an XDG cache by default; `--offline` disables that network surface without disabling vector maps, search, candidate inspection, or elevation profiles.

Useful verification gates:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

Runtime inputs are ordinary local project files. `cache-source` can download `http://` or `https://` URLs through `reqwest` with rustls TLS, and `acquire-osm` can post bbox-scoped Overpass queries for OSM XML trail, road, hydrology, or combined extracts. Network access is needed only when acquiring remote source artifacts or displaying the optional USGS GUI basemap. GPX, GeoJSON, route JSON, KML, KMZ, CSV, OSM XML/PBF, shapefile vector layers, declared EPSG:3857 Web Mercator or WGS84/NAD83 UTM vector reprojection, Arc/Info ASCII Grid, affine WGS84/NAD83/EPSG:3857/WGS84/NAD83 UTM GeoTIFF DEMs, and simple local GDAL VRT wrappers around those DEMs are implemented in Rust crates; no GDAL install is required for those paths.

Future adapters for arbitrary projected rasters, OSM planet-diff workflows, complex turn-state routing, or agency APIs may introduce optional system dependencies. If that happens, keep them behind an adapter seam or feature flag and document the command that materializes normalized project artifacts under `sources/` or `cache/`. The current OSM XML/PBF way, route-relation, simple turn-restriction, Overpass XML acquisition, and VRT DEM adapters are pure Rust; VRT supports full-raster identity `SimpleSource` wrappers with affine WGS84/NAD83, EPSG:3857, or WGS84/NAD83 UTM `GeoTransform` sampling.
