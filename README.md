# Trailgen

Trailgen is a native Rust workbench for finding, drawing, inspecting, and keeping long-day hiking routes. The GUI is the product frontend. Its vector map, trail acquisition, route search, editor, project Library, and GPX export share one engine.

## Install

```sh
cargo install trailgen --locked
trailgen
```

Cargo places the unified `trailgen` binary under its configured binary root.
For a checkout build, `./scripts/install-local.sh` installs the same binary
beneath `~/.local/bin`. Bare `trailgen` resumes the last chosen project or
opens the project deck. New projects conventionally live beneath the operating
system’s XDG documents directory in `trailgen/`; Trailgen honors its exact
spelling and never invents `~/Documents`.

Linux/X11 is the sole 1.0 release-tested platform coordinate. Wayland and
other operating systems are not part of the present support claim.

Projects are portable directories rooted by `trailgen.toml`. Project content, including the canonical saved-trail Library, stays in that directory. View and window state belongs under `$XDG_STATE_HOME/trailgen`, app-wide preferences under `$XDG_CONFIG_HOME/trailgen`, and shared map cache under `$XDG_CACHE_HOME/trailgen`.

## Workflow

1. Create a project and pan to its territory.
2. Draw one or more bronze-framed map areas. Trailgen acquires and indexes their union.
3. Draw a trail directly from support points, or place a trailhead and search by distance, moving time, climb, target lower-limb load, and shape.
4. Inspect a trail on the full map and elevation profile. Save it when it deserves durable project identity.
5. In **Saved Trails**, press `↥` beside its name to export GPX. Unsaved candidates and unfinished edits cannot be exported.

The GPX carries one contiguous hiking track, the Library name, elevations where known, and the saved measurements in its description. Upload that file to AllTrails as a custom route, then open it under **Saved → Lists → Custom routes & maps** for navigation. See the [AllTrails handoff](docs/alltrails.md).

The default US corpus combines OpenStreetMap/Overpass, USGS National Digital Trails, spatially applicable state-park authorities, and cached Mapzen terrain tiles. New York and Texas are the first admitted state providers. Provider responses are independently sequestered beneath `sources/`, fingerprinted, and rebuilt through one graph path before `cache/graph.bin` becomes ready. Informal standing, wayfinding, terrain, and legal access remain distinct facts. See [data sources](docs/data-sources.md).

## Debug Shell

The shell is deliberately incomplete. It exposes only operations already owned by the GUI application service:

```sh
trailgen gui [PROJECT] [--offline]
trailgen saved PROJECT
trailgen export PROJECT --trail NAME_OR_ID --output route.gpx
```

Bare `trailgen` and `trailgen gui` launch the same native workbench. `saved` prints durable Library identities and names. `export` invokes the same saved-trail GPX writer as the `↥` control; it does not regenerate, reinterpret, or select transient candidates. New shell commands belong here only when a concrete debugging need appears and shared product logic already owns the operation.

## Verification

```sh
scripts/check
scripts/audit
scripts/verify-install
scripts/test-gui
```

`scripts/verify-install` proves a sterile non-default install and Cargo-tracked
uninstall. `scripts/test-gui` is the hermetic native acceptance gate. Its
complete user stories cover GUI project creation and provider acquisition,
saved-trail refinement and restart, twelve-candidate comparison under cadence
budgets, manual trail design, saved GPX export, and productive work while graph
armament is deliberately stalled.

`scripts/package` fully verifies every archive whose internal dependencies are
already on crates.io and names the downstream archives Cargo must defer.
`scripts/release VERSION publish` requires the pushed commit and a valid signed
tag, repeats every gate, publishes the five-crate graph in dependency order,
then verifies the complete registry-resolved package graph.

See [installation](docs/installation.md), [project state](docs/config.md), [model](docs/model.md), [physical load and moving time](docs/physical-load.md), and [known limitations](docs/limitations.md).

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
