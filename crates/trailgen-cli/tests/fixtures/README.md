# Harriman Route Fixtures

`harriman-south-lows.csv` and `harriman-west.csv` are original routes created and owned by the repository owner. They are standing integration fixtures under the repository's MIT OR Apache-2.0 license.

`harriman-west.csv` drops one initial point that duplicated its final sub-meter approach in reverse. The raw recorder trace therefore becomes the simple topological loop the fixture is meant to assert, without materially changing its line.

The integration test builds a route-derived graph from each owned CSV trace, generates one tightly constrained exact loop, verifies the generation ledger, and requires at least 99.5% of the source points to survive within 2 m. These are deterministic, network-free replay fixtures.

Public-source coverage is tested separately because live providers are mutable and unsuitable for CI. The `trailgen coverage` diagnostic was run on 2026-07-21 against a fresh current-OSM Harriman shard: both fixtures had zero remote segments and zero disconnected transitions within a 40 m observation radius. Maximum separation was 2.74 m for South Lows and 5.23 m for West. The default OSM + USGS corpus used the same provider, normalization, conflation, and graph engine; USGS enriched the surrounding network but did not improve either owned trace.
