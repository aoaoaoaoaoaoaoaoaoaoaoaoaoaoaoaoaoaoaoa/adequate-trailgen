# Harriman Route Fixtures

`harriman-south-lows.csv` and `harriman-west.csv` are original routes created and owned by the repository owner. They are standing integration fixtures under the repository's MIT OR Apache-2.0 license.

The integration test builds a route-derived graph from each CSV, generates one tightly constrained exact loop, verifies the generation ledger, and requires at least 99.5% of the source points to survive within 2 m. These cases prove deterministic real-route generation and replay; they do not claim independent rediscovery from public OSM.
