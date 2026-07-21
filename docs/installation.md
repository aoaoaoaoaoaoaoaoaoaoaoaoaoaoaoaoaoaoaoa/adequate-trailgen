# Installation

Trailgen is a Rust workspace with no mandatory external GIS binary. Install the unified release binary locally:

```sh
./scripts/install-local.sh
trailgen
```

The installer uses `cargo install --locked --force` and writes `trailgen` beneath `${TRAILGEN_INSTALL_ROOT:-$HOME/.local}/bin`. Pass a different root as the first argument for an isolated installation.

Bare startup first honors a project in the current directory, then the last valid project explicitly chosen by the user. With neither, Trailgen opens its project deck. New projects are created beneath the XDG documents directory when available; Linux therefore honors `XDG_DOCUMENTS_DIR` exactly, including spelling and case. If the operating system provides no documents location, the deck asks for a parent folder. `trailgen gui PATH` remains the strict explicit form, and `Ctrl+O` returns to the deck.

New projects open on a low-zoom US vector map. **Add Map Area** turns a drag gesture into a durable fetch rectangle; acquisition and union indexing begin without a CLI handoff. Once trail data is ready, the workbench exposes a flat trail-family library and compact family-owned searches. Search recipes and saved trail geometry live in `library/index.json`. Candidate results are transient.

The native workbench needs a functioning Vulkan or OpenGL graphics stack and an X11 or Wayland session. Shared bootstrap and roaming vectors live beneath `$XDG_CACHE_HOME/trailgen`; content-addressed project cuts live under the project’s `cache/`. Viewport, inspector, active family, gallery, and sorting state are atomically debounced beneath `$XDG_STATE_HOME/trailgen/projects/`. `--offline` suppresses network acquisition while retaining cached maps, trails, search, and profiles. `TRAILGEN_BASEMAP_ARCHIVE=/path/to/map.pmtiles` selects a prepared vector archive.

Useful verification gates:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The shared trail-data engine uses Overpass and USGS National Digital Trails for the default rectangle-scoped corpus. Nominatim remains available only to the CLI `survey --place` debug frontend. `TRAILGEN_GEOCODER_ENDPOINT`, `TRAILGEN_OVERPASS_ENDPOINT`, and `TRAILGEN_USGS_TRAILS_ENDPOINT` replace those endpoints for private or test deployments. Network access is needed only for uncached trail acquisition, a missing project basemap, or uncached territory outside its cut.
