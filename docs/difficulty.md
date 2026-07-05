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

Calibration starts by changing `[difficulty]` in `trailgen.toml`, then rerating or rebuilding the graph. A practical workflow is to import completed hikes as seed routes, compare the reported factor mix against personal effort, and adjust weights one family at a time: first distance/elevation, then terrain and grade, then confidence/access penalties.
