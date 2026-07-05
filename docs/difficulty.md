# Difficulty

Difficulty is additive per edge. `DifficultyWeights` rates each edge into a persisted `DifficultyBreakdown`:

- `distance`: edge length in km × `distance_per_km`
- `ascent`: ascent meters × `ascent_per_m`
- `descent`: descent meters × `descent_per_m`
- `grade`: mean absolute grade × `grade_per_abs_fraction`
- `terrain`: distance × (`terrain_multiplier - 1`), using `[difficulty.terrain_multipliers]`
- `road`: road-exposure fraction × `road_penalty` × distance cost; road context crossings can raise road exposure
- `confidence`: low-confidence penalty from `1 - confidence`
- `access`: hard penalty for closed/private or restricted access

The edge stores both the scalar total and the factor breakdown. It also stores fixed-bin grade distribution meters: flat `<5%`, rolling `5–15%`, steep `15–30%`, and savage `≥30%` absolute grade. Route metrics sum difficulty factors, and reports show route-level factor shares plus the largest edge-factor contributors and grade-bin summaries for dubious segments. This makes a high scalar score inspectable instead of mystical.

Default terrain multipliers are: unknown 1.15, trail 1.0, forest 1.0, alpine 1.18, talus 1.65, scramble 2.1, pavement 0.82, road 0.9, water 2.5. Override any subset under `[difficulty.terrain_multipliers]`; missing buckets retain defaults.

Calibration starts from completed hikes. `trailgen rate <project> --route completed.gpx` snaps a supplied GPX, GeoJSON, KML, KMZ, or CSV route to the cached graph and reports its scalar difficulty plus factor mix. `trailgen calibrate <project> --route completed.gpx --target-difficulty N --family elevation` solves the positive multiplier needed for one weight family while holding other factors fixed, then prints the TOML patch as a dry run. Add `--write` to update `trailgen.toml` and rerate `cache/graph.json` / `cache/graph.geojson`.

Calibration families are `all`, `distance`, `elevation`, `ascent`, `descent`, `grade`, `terrain`, `road`, `confidence`, and `access`. `all` is a global scalar over realized factors: it scales `distance_per_km`, ascent/descent, grade, confidence, and access while leaving terrain multipliers and `road_penalty` alone, because terrain and road factors already multiply through distance cost. Family-specific terrain calibration scales each multiplier’s offset from `1.0`, preserving neutral trail/forest defaults. If a family contributes nothing on the calibration route, or a target would require a non-positive weight, calibration fails loudly.

Use `trailgen rerate <project>` after hand-editing `[difficulty]` to reapply the current weights to every cached edge without rebuilding topology or reapplying source overlays. After any calibration or rerate, rerun `generate` so route JSON, exports, manifests, and reports reflect the new scalar model.
