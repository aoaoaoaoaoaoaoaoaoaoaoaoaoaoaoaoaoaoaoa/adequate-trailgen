# Trailgen

Trailgen is a native Rust workbench for finding and keeping long-day hiking routes. Its vector map, trail acquisition, route search, and project library share one engine; the CLI is a diagnostic frontend over that engine.

## Install

```sh
./scripts/install-local.sh
trailgen
```

The installer places the unified `trailgen` binary under `~/.local/bin`. Bare `trailgen` resumes the last explicitly chosen project or opens the project deck. New projects conventionally live beneath the operating system’s XDG documents directory in `trailgen/`; Trailgen honors its exact spelling and never invents `~/Documents`.

Projects are ordinary portable directories rooted by `trailgen.toml`. Per-project viewport, inspector, gallery, and sorting state lives under `$XDG_STATE_HOME/trailgen`. Shared map cache belongs under `$XDG_CACHE_HOME/trailgen`; no state is scattered through the project or home directory.

## Workflow

1. Create a project and pan to its broad territory.
2. Draw one or more bronze-framed map areas. Trailgen downloads and indexes the union automatically; ground outside that union remains dimmed.
3. Press **Draw a Trail** to build one directly from draggable support points, with no search required.
4. For discovery, place the project trailhead, choose distance, climb, target difficulty, and shape, then press **Find Trails**.
5. Open any result for a smooth full-map view and elevation profile. Save it, or edit any candidate whose exact walk admits a support-point design.

Saved trails belong directly to the project library. Deleting a trail removes it deliberately from the project; search results remain transient until saved.

Trail types use high-contrast dual strokes and a map legend. Focused routes add an inner terrain trace. The bronze trailhead pin and restrained water response follow the Dwemer Poolrooms design language.

The default US corpus combines OpenStreetMap/Overpass, USGS National Digital Trails, spatially
applicable state-park authorities, and cached Mapzen terrain tiles. New York and Texas are the first
admitted state providers. Every rectangle-provider response is independently sequestered beneath
`sources/`, fingerprinted, and rebuilt through one graph path before `cache/graph.json` becomes ready.
Informal, unmaintained, historical, and unmarked paths remain searchable and visually distinct;
standing, wayfinding, and legal access never masquerade as one another. See
[data sources](docs/data-sources.md).

## Debug Frontend

`trailgen --help` exposes acquisition, indexing, verification, generation, import, and export commands for diagnosis and reproducibility. For example:

```sh
trailgen init /tmp/harriman --name "Harriman"
trailgen survey /tmp/harriman --place "Harriman State Park, NY"
trailgen coverage /tmp/harriman --route owned-route.csv --output coverage.json
trailgen stats /tmp/harriman
```

Product behavior is specified by the GUI. Debug commands use the same core and data machinery.

## Verification

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
scripts/test-gui
```

The standing Harriman cases replay two user-owned CSV traces through route-derived graphs and require deterministic generation, executable artifact verification, and at least 99.5% point survival within 2 m. A separate 2026-07-21 public-source probe found complete current-OSM geometry and topology coverage for both traces: South Lows remained within 2.74 m and West within 5.23 m, with no disconnected transition. USGS enriched the surrounding corpus but did not improve either owned trace.

`scripts/test-gui` is the hermetic native acceptance gate. Four complete user
stories exercise GUI project creation and provider acquisition, saved-trail
refinement with undo/redo and restart, twelve-candidate comparison under
cadence budgets, and manual loop drawing with profile interaction. The harness
drives the optimized binary through native input and adjudicates durable files
outside its one-way UI witness.

See [installation](docs/installation.md), [model](docs/model.md), [configuration](docs/config.md), [source adapters](docs/source-adapters.md), and [known limitations](docs/limitations.md).

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
