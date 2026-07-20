# Installation

`adequate-trailgen` is a Rust workspace with no mandatory external GIS binary for the CLI, GUI, fixtures, or demo. The core retains the workspace baseline in `Cargo.toml`; the native application declares its newer Poolrooms toolchain floor in the application manifests. Install a compatible toolchain, then install the unified release binary locally:

```sh
cargo build --workspace
./scripts/install-local.sh
trailgen --help
trailgen
```

The installer uses `cargo install --locked --force` and writes `trailgen` beneath `${TRAILGEN_INSTALL_ROOT:-$HOME/.local}/bin`. Pass another root as its first argument for an isolated installation, such as `./scripts/install-local.sh /tmp/trailgen-install`. All provenance-sensitive command surfaces remain subcommands of the same binary.

Bare startup first honors a `trailgen.toml` in the current directory, then the last valid project recorded under the platform state directory. With neither, Trailgen resolves the operating system's Documents directory through `directories::UserDirs` and creates `trailgen/starter-loop` there. Linux therefore honors `XDG_DOCUMENTS_DIR` from `user-dirs.dirs` exactly, including its spelling and case. An unavailable or unwritable managed location opens a Poolrooms project-root prompt; it never causes a loose `trailgen.toml` to appear in the home or working directory. Explicit `trailgen gui PATH` remains strict and portable.

The native workbench needs a functioning Vulkan or OpenGL graphics stack and an X11 or Wayland session. Its first online opening extracts the graph's padded bounds through HTTP range requests from the latest Protomaps daily PMTiles build, writes an atomic `cache/basemap.pmtiles` project artifact through z15, then memory-maps that archive on later launches. Panning beyond those bounds ranges missing vectors and retains them in a 512 MiB `$XDG_CACHE_HOME/trailgen/protomaps-v4` cache. The viewport and transient inspector/gallery state are atomically debounced into `$XDG_STATE_HOME/trailgen/slate.toml`. `--offline` disables acquisition and basemap display without disabling measured trail vectors, search, candidate inspection, or elevation profiles. `TRAILGEN_BASEMAP_ARCHIVE=/path/to/map.pmtiles` selects an already prepared archive and suppresses automatic project extraction.

Useful verification gates:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

Runtime inputs are ordinary local project files. `cache-source` can download `http://` or `https://` URLs through `reqwest` with rustls TLS, and `acquire-osm` can post bbox-scoped Overpass queries for OSM XML trail, road, hydrology, or combined extracts. Network access is needed only when acquiring remote source artifacts, forging a missing project basemap, or viewing uncached territory outside that cut. GPX, GeoJSON, route JSON, KML, KMZ, CSV, OSM XML/PBF, shapefile vector layers, declared EPSG:3857 Web Mercator or WGS84/NAD83 UTM vector reprojection, Arc/Info ASCII Grid, affine WGS84/NAD83/EPSG:3857/WGS84/NAD83 UTM GeoTIFF DEMs, and simple local GDAL VRT wrappers around those DEMs are implemented in Rust crates; no GDAL install is required for those paths.

Future adapters for arbitrary projected rasters, OSM planet-diff workflows, complex turn-state routing, or agency APIs may introduce optional system dependencies. If that happens, keep them behind an adapter seam or feature flag and document the command that materializes normalized project artifacts under `sources/` or `cache/`. The current OSM XML/PBF way, route-relation, simple turn-restriction, Overpass XML acquisition, and VRT DEM adapters are pure Rust; VRT supports full-raster identity `SimpleSource` wrappers with affine WGS84/NAD83, EPSG:3857, or WGS84/NAD83 UTM `GeoTransform` sampling.
