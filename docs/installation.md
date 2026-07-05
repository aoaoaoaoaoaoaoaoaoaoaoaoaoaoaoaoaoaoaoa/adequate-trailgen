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

Runtime inputs are ordinary local project files. `cache-source` can download `http://` or `https://` URLs through `reqwest` with rustls TLS, so network access is needed only when acquiring remote source artifacts. GPX, GeoJSON, KML, KMZ, CSV, and Arc/Info ASCII Grid support are implemented in Rust crates; no GDAL install is required for those paths.

Future adapters for GeoTIFF/VRT, shapefiles, OSM extracts, or agency APIs may introduce optional system dependencies. If that happens, keep them behind an adapter seam or feature flag and document the command that materializes normalized project artifacts under `sources/` or `cache/`.
