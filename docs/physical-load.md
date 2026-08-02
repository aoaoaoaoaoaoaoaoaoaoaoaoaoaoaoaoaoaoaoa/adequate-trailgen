# Physical Load And Moving Time

Trailgen keeps two population estimates separate:

- **Lower-limb load** is an extensive route dose, expressed in flat-gravel
  joint-work-equivalent kilometers (`FGJW km`). It is the targetable search
  scalar.
- **Moving time** is an expected duration. Search exposes independent lower
  and upper bounds.

Neither quantity predicts pain, injury, hydration, technical risk, or whether
a particular person can finish the route.

## Lower-Limb Load

For path grade `g`, Trailgen uses the total positive-plus-absolute-negative
ankle, knee, and hip joint-power ratios measured by Nuckols et al.:

| Grade | Joint-work factor `J(g)` |
| ---: | ---: |
| −15% | 1.69 |
| −10% | 1.24 |
| 0% | 1.00 |
| +10% | 1.18 |
| +15% | 1.57 |

A shape-preserving monotone cubic interpolates each side of level. Values
outside the measured interval clamp to its terminal factor; Trailgen does not
manufacture extreme-slope physics by polynomial extrapolation.

The directional route total is:

```text
FGJW km = Σ segment_length_km × J(segment_grade) × R(surface)
```

Flat gravel, compacted ground, pavement, and roads define `R = 1`. Other
surfaces currently use `R = 1.28`, the modest uneven-ground anchor supported
by Voloshina et al. That factor is intentionally not a talus-to-swamp severity
table. Technicality, wayfinding, access, confidence, and desirability retain
their own representations rather than polluting a physically named unit.

Downhill and uphill traversal differ. Every graph edge therefore stores
forward and reverse load estimates, and route measurement follows the actual
directed walk.

## Moving Time

For each segment, Trailgen applies the population model fitted by Wood et al.
to almost 88,000 km of recorded UK walks:

```text
speed_km_h = exp(a + b × hill_slope_deg
                   + c × walking_slope_deg
                   + d × walking_slope_deg²)
moving_time = Σ segment_length_km / speed_km_h
```

| Ground class | `a` | `b` | `c` | `d` |
| --- | ---: | ---: | ---: | ---: |
| Paved road | 1.580 | −0.00389 | −0.00726 | −0.00218 |
| Unpaved road or path | 1.580 | −0.00389 | −0.00965 | −0.00248 |
| Off-road, obstruction unknown | 1.536 | −0.00731 | −0.00965 | −0.00187 |
| Off-road, light obstruction | 1.580 | −0.00731 | −0.00965 | −0.00187 |
| Off-road, heavy obstruction | 1.443 | −0.00731 | −0.00965 | −0.00187 |

Terrain enrichment estimates the surrounding hill slope independently of
along-path grade. Without complete DEM context, the geometrically necessary
minimum `|walking slope|` is used. The stored model is a population point
estimate: the source reports roughly 13.5–15.5% mean whole-route error across
the compared methods.

The GUI projects that estimate through one app-wide **Base Pace**, defaulting
to 5.0 km/h. The Wood model's flat-path intercept is `exp(1.580) ≈ 4.855 km/h`,
so displayed duration is:

```text
personal_moving_time = population_moving_time × 4.855 / Base Pace
```

The same inverse conversion is applied to GUI time constraints before search.
This scalar calibration preserves Wood's terrain and grade response while
matching the user's flat-ground speed.

## Product Contract

The GUI persists a moving-time window and one lower-limb-load target in the
project library. It persists Base Pace separately in XDG configuration because
the calibration belongs to the user, not the project. The project and debug CLI
may also impose hard load bounds:
`min_lower_limb_load_km`, `max_lower_limb_load_km`,
`target_lower_limb_load_km`, `min_moving_time_s`, and `max_moving_time_s`.
CLI one-run overrides use hours for time:
`--min-moving-time-h`, `--max-moving-time-h`, and
`--target-lower-limb-load-km`.

`trailgen rate <project> --route completed.gpx` reports both population
estimates for an imported route. Base Pace calibrates time only; sex, height,
and body mass do not turn route geometry into a defensible capacity or injury
forecast.

See [the literature ledger](../notes/physical-load-literature.md) and
[reference corpus](../references/README.md) for the derivation, alternatives,
and evidentiary limits.
