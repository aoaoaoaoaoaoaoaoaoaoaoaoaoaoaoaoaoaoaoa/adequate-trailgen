# Harriman Route Fixtures

`harriman-south-lows.csv` and `harriman-west.csv` are original routes created and owned by the repository owner. They are standing integration fixtures under the repository's MIT OR Apache-2.0 license.

The integration test builds a route-derived graph from each owned CSV trace, generates one tightly constrained exact loop, verifies the generation ledger, and requires at least 99.5% of the source points to survive within 2 m. These are replay fixtures, not source-coverage fixtures: they prove deterministic real-route generation without claiming that a public provider independently contains every trail transition.
