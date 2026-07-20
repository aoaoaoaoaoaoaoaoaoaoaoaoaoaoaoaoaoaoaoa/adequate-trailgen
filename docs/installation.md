# Installation

`adequate-trailgen` is a Rust workspace with no mandatory external GIS binary for the CLI, GUI, fixtures, or demo. The core retains the workspace baseline in `Cargo.toml`; the native application declares its newer Poolrooms toolchain floor in the application manifests. Install a compatible toolchain, then install the unified release binary locally:

```sh
cargo build --workspace
./scripts/install-local.sh
trailgen --help
trailgen
```

The installer uses `cargo install --locked --force` and writes `trailgen` beneath `${TRAILGEN_INSTALL_ROOT:-$HOME/.local}/bin`. Pass another root as its first argument for an isolated installation, such as `./scripts/install-local.sh /tmp/trailgen-install`. All provenance-sensitive command surfaces remain subcommands of the same binary.

Bare startup first honors an initialized project in the current directory, then the last valid project explicitly chosen by the user. With neither, Trailgen opens its Poolrooms project deck and writes nothing. Entering a US place or trailhead creates a project beneath the chosen parent, resolves the place, acquires the default OSM trail network, and indexes it without a CLI handoff. Existing projects without a graph open the same acquisition surface and automatically resume a configured demand when online. The library is resolved through `directories::UserDirs`; Linux therefore honors `XDG_DOCUMENTS_DIR` from `user-dirs.dirs` exactly, including its spelling and case. An unavailable Documents location merely requires the user to choose a parent folder. Explicit `trailgen gui PATH` remains strict and portable, while `Ctrl+O` opens the project deck without sacrificing the active workbench.

The native workbench needs a functioning Vulkan or OpenGL graphics stack and an X11 or Wayland session. Its first online opening extracts the graph's padded bounds through HTTP range requests from the latest Protomaps daily PMTiles build, writes an atomic `cache/basemap.pmtiles` project artifact through z15, then memory-maps that archive on later launches. Panning beyond those bounds ranges missing vectors and retains them in a 512 MiB `$XDG_CACHE_HOME/trailgen/protomaps-v4` cache. Each project's viewport, inspector/gallery state, search draft, and saved-candidate visibility are atomically debounced into a path-keyed slate beneath `$XDG_STATE_HOME/trailgen/projects/`. Search edits do not rewrite project configuration and have an explicit **RESET PROJECT DEFAULTS** action; clearing candidates hides them without deleting route artifacts and can be reversed with **RESTORE SAVED**. `--offline` disables acquisition and basemap display without disabling measured trail vectors, search, candidate inspection, or elevation profiles. `TRAILGEN_BASEMAP_ARCHIVE=/path/to/map.pmtiles` selects an already prepared archive and suppresses automatic project extraction.

Useful verification gates:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

Runtime inputs are ordinary local project files. The shared trail-data engine uses Nominatim for US place resolution and Overpass for bounded OSM trail acquisition; `TRAILGEN_GEOCODER_ENDPOINT` and `TRAILGEN_OVERPASS_ENDPOINT` replace those endpoints for private deployments. `cache-source` and `acquire-osm` expose lower-level debug acquisition. Network access is needed only when resolving or acquiring uncached trail data, forging a missing project basemap, or viewing uncached territory outside that cut. GPX, GeoJSON, route JSON, KML, KMZ, CSV, OSM XML/PBF, shapefile vector layers, declared EPSG:3857 Web Mercator or WGS84/NAD83 UTM vector reprojection, Arc/Info ASCII Grid, affine WGS84/NAD83/EPSG:3857/WGS84/NAD83 UTM GeoTIFF DEMs, and simple local GDAL VRT wrappers around those DEMs are implemented in Rust crates; no GDAL install is required for those paths.

Future adapters for arbitrary projected rasters, OSM planet-diff workflows, complex turn-state routing, or agency APIs may introduce optional system dependencies. If that happens, keep them behind an adapter seam or feature flag and document the command that materializes normalized project artifacts under `sources/` or `cache/`. The current OSM XML/PBF way, route-relation, simple turn-restriction, Overpass XML acquisition, and VRT DEM adapters are pure Rust; VRT supports full-raster identity `SimpleSource` wrappers with affine WGS84/NAD83, EPSG:3857, or WGS84/NAD83 UTM `GeoTransform` sampling.
