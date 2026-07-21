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
3. Create flat trail families such as `long`, `climby`, or `easy`.
4. Select a family, place its trailhead on the map, choose distance, climb, and shape, then press **Find Trails**.
5. Open any result for a smooth full-map view and elevation profile. Save it into the family to make it durable.

Saved trails form a project-owned library. A trail may belong to several families. Deleting a family preserves its trails in **Unfiled**; deleting a trail removes it deliberately from the whole project. Search results remain transient.

Trail types use high-contrast dual strokes and a map legend. Focused routes add an inner terrain trace. The bronze trailhead pin and restrained water response follow the Dwemer Poolrooms design language.

The default trail source is the US-scoped OpenStreetMap/Overpass path described in [data sources](docs/data-sources.md). Raw rectangle responses are independently sequestered under `sources/osm/`; `cache/graph.json` is the deterministic index of their exact union. The internal model remains provider-neutral so official GIS layers and user-owned traces can be added without creating another application pipeline.

## Debug Frontend

`trailgen --help` exposes acquisition, indexing, verification, generation, import, and export commands for diagnosis and reproducibility. For example:

```sh
trailgen init /tmp/harriman --name "Harriman"
trailgen survey /tmp/harriman --place "Harriman State Park, NY"
trailgen stats /tmp/harriman
```

Product behavior is specified by the GUI. Debug commands use the same core and data machinery.

## Verification

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The standing Harriman cases replay two user-owned CSV traces through route-derived graphs. They prove deterministic generation, artifact verification, and geometric fidelity. They do not yet prove independent public-source recovery: the current OpenStreetMap comparison has three unmatched transitions. Provider coverage is therefore a separate, explicit acceptance target rather than an embellished claim.

See [installation](docs/installation.md), [model](docs/model.md), [configuration](docs/config.md), [source adapters](docs/source-adapters.md), and [known limitations](docs/limitations.md).

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
