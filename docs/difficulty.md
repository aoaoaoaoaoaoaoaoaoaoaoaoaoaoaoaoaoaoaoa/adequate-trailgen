# Difficulty

Difficulty is additive per edge. `DifficultyWeights` rates each edge into a persisted `DifficultyBreakdown`:

- `distance`: edge length in km × `distance_per_km`
- `ascent`: ascent meters × `ascent_per_m`
- `descent`: descent meters × `descent_per_m`
- `grade`: mean absolute grade × `grade_per_abs_fraction` × edge length in km
- `terrain`: edge length in km × (`terrain_multiplier - 1`), using `[difficulty.terrain_multipliers]`
- `road`: road-exposure fraction × `road_effort_penalty` × edge length in km; this explicit effort override defaults to zero
- `technical`: rough-terrain and steep-grade pressure × `technical_penalty` × edge length in km
- `navigation`: weak terrain evidence, unknown terrain, and crossing complexity × `navigation_penalty` × edge length in km
- `bushwhack`: pathless-route indicator × `bushwhack_penalty` × edge length in km
- `confidence`: `(1 - confidence)` × `low_confidence_penalty` × edge length in km
- `access`: closed/private or restricted pressure × `closed_access_penalty` × edge length in km

The edge stores both the scalar total and the factor breakdown. It also stores fixed-bin grade distribution meters for elevation-covered spans: flat `<5%`, rolling `5–15%`, steep `15–30%`, and savage `≥30%` absolute grade. Route metrics sum difficulty factors, sustained steep meters, and grade distribution bins. Reports show route-level factor shares, route-level grade-bin summaries, largest edge-factor contributors, and grade-bin summaries for dubious segments. This makes a high scalar score inspectable instead of mystical.

Default terrain multipliers are: unknown 1.15, trail 1.0, forest 1.0, alpine 1.18, talus 1.65, scramble 2.1, pavement 0.82, road 0.9, water 2.5. Override any subset under `[difficulty.terrain_multipliers]`; missing buckets retain defaults. Roads are therefore usually physically easier, while route quality and `search.routing.road_aversion` express their aesthetic cost. `road_effort_penalty` exists only for a user who deliberately considers road walking harder. The legacy TOML name `road_penalty` remains a read alias. Bushwhacks add a separate default `bushwhack_penalty` of 3.0 per km, so substrate and pathlessness compose: talus bushwhacking retains talus's technical cost rather than collapsing into a generic off-trail bucket.

Calibration starts from completed hikes. `trailgen rate <project> --route completed.gpx [--output reports/completed.md]` snaps a supplied GPX, GeoJSON, KML, KMZ, or CSV route to the cached graph and reports scalar difficulty, factor mix, constraint verdicts and margins, dubious segments, access warnings, terrain mix, and source manifest evidence. Route snapping is bounded by `max_route_snap_m`; pass `--max-route-snap-m N` only for deliberately coarse or generalized tracks. `trailgen calibrate <project> --route completed.gpx --target-difficulty N --family elevation` solves the positive multiplier needed for one weight family while holding other factors fixed, then prints the TOML patch as a dry run. Add `--write` to update `trailgen.toml` and rerate all cached graph surfaces, including JSON, GeoJSON, and CSV tables.

Calibration families are `all`, `distance`, `elevation`, `ascent`, `descent`, `grade`, `terrain`, `road`, `technical`, `navigation`, `bushwhack`, `confidence`, and `access`. Every family has an independent linear basis. `all` deliberately scales every realized factor, including terrain offsets and road, technical, navigation, bushwhack, confidence, and access weights. Family-specific terrain calibration scales each multiplier’s offset from `1.0`, preserving neutral trail/forest defaults. If a family contributes nothing on the calibration route, or a target would require a non-positive weight, calibration fails loudly.

Use `trailgen rerate <project>` after hand-editing `[difficulty]` to reapply the current weights to every cached edge without rebuilding topology or reapplying source overlays. After any calibration or rerate, rerun `generate` so route JSON, exports, manifests, and reports reflect the new scalar model.
