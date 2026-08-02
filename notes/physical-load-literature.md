# Physical Load And Moving-Time Literature

## Name and Dimension

An extensive difficulty measure can be stated as **flat-gravel-equivalent kilometers** (`FGE km`):

> `x FGE km` requires the modeled work of walking `x km` on level, packed dirt or gravel.

The route value must be additive over traversed segments. Intensity, risk, source confidence, legality, and route quality are different dimensions.

“Equivalent” has three incompatible meanings in existing practice:

| Family | Quantity preserved | Representative methods |
|--------|--------------------|------------------------|
| Travel time | elapsed moving time | Naismith, Tobler, DIN 33466 |
| Convention | race or expedition class | UTMB kilometer-effort, Petzoldt energy miles |
| Metabolic work | measured energy per mass | Minetti slope costs, Soule–Goldman terrain factors, Pandolf load carriage |

Only the last family directly matches an energy-equivalent kilometer. Travel time should remain a separate estimate.

## Ascent Conventions

The following values convert ascent into additional flat distance. They are not interchangeable calibrations.

| Method | Published rule | Added flat equivalent per 100 m ascent | Meaning |
|--------|----------------|-----------------------------------------|---------|
| Naismith, original | 1 h per 3 mi plus 1 h per 2,000 ft ascent | 0.79 km | Time |
| Naismith, common metric form | 5 km/h plus 10 min per 100 m | 0.83 km | Time |
| Petzoldt energy mile | 2 mi per 1,000 ft ascent | 1.06 km | Energy heuristic |
| Troy–Phipps measurement | 1.6 mi per 1,000 ft ascent | 0.84 km | Aggregate measured caloric equivalence |
| UTMB kilometer-effort | 1 km per 100 m ascent | 1.00 km | Race classification |
| DIN 33466, pure ascent | 300 vertical m/h against 4 flat km/h | 1.33 km | Time; composition is nonadditive |

DIN 33466 separately computes horizontal and vertical times, then adds half the smaller to the larger. Its pure-ascent conversion therefore cannot be applied as an additive route coefficient without changing the standard.

## Grade: Minetti et al.

Minetti et al. measured net walking energy at the energy-minimizing speed on a treadmill. The last column divides each mean by the measured level cost, 1.64 J·kg⁻¹·m⁻¹.

| Grade | Cost, J·kg⁻¹·m⁻¹ | Level-energy factor |
|------:|------------------:|--------------------:|
| −45% | 3.46 | 2.11× |
| −40% | 3.23 | 1.97× |
| −35% | 2.65 | 1.62× |
| −30% | 2.18 | 1.33× |
| −20% | 1.30 | 0.79× |
| −10% | 0.81 | 0.49× |
| 0% | 1.64 | 1.00× |
| +10% | 4.68 | 2.85× |
| +20% | 8.07 | 4.92× |
| +30% | 11.29 | 6.88× |
| +35% | 12.72 | 7.76× |
| +40% | 14.75 | 8.99× |
| +45% | 17.33 | 10.57× |

This gives the correct qualitative shape absent from a linear ascent/descent penalty: a mild descent reduces metabolic work per meter, while steep descent raises it again. It does not measure delayed muscle damage, caution, technical footing, or risk. The sample comprised ten trained male mountain runners.

## Terrain: Soule–Goldman and Pandolf

The conventional terrain coefficients scale locomotion energy relative to blacktop. The final column renormalizes them to packed dirt/gravel so that one level gravel kilometer is one `FGE km`.

| Terrain | Published coefficient | Flat-gravel factor |
|---------|----------------------:|-------------------:|
| Blacktop | 1.0 | 0.91× |
| Dirt or gravel road | 1.1 | 1.00× |
| Light brush | 1.2 | 1.09× |
| Heavy brush | 1.5 | 1.36× |
| Swampy bog | 1.8 | 1.64× |
| Loose sand | 2.1 | 1.91× |

These are old load-carriage coefficients, not a modern taxonomy of trail surfaces. Field validation found that the nominal 1.1 dirt-road factor fit most graded-gravel trials but underpredicted one rough +8.6% loaded condition; speed, load, grade, traction, and roughness interact.

## MET Table

The 2024 Adult Compendium supplies standardized intensity values:

| Activity | MET |
|----------|----:|
| Level firm surface, 2.8–3.4 mph | 3.8 |
| Normal hiking through fields and hillsides, no load | 5.3 |
| Cross-country hiking | 6.0 |
| Backpacking | 7.0 |
| Organized hiking with daypack | 7.8 |
| Hills, 6–10% grade, no load, moderate-to-brisk | 7.0 |
| Hills, 11–20% grade, no load, slow-to-moderate | 8.8 |
| Hills, 30% grade, below 1.2 mph | 8.5 |
| Hills, 30–40% grade, 1.2–1.8 mph | 15.5 |

MET is metabolic power, not total work. Equivalent distance requires multiplying by duration, and the listed activities do not hold speed constant. The table is a sanity check, not a segment cost law.

## Candidate Backbone

A physically named first approximation is:

`FGE km = Σ segment_length_km × grade_energy_factor × terrain_factor / 1.1`

where the grade factor comes from Minetti's walking-cost curve and `1.1` makes packed dirt or gravel the identity terrain. Before adoption it requires route-fixture calibration and explicit decisions for:

- downhill muscle damage beyond metabolic cost;
- technical footing, scrambling, and bushwhacking;
- carried load and user calibration;
- interactions between grade and loose or obstructed ground.

Travel-time prediction should use a separately validated model. Source confidence belongs to uncertainty or quality; access belongs to admissibility; neither belongs in `FGE km`.

Trailgen's current score is only a proto-equivalent distance. Its one-point-per-kilometer base gives it the right extensive shape, but ascent, descent, mean grade, confidence, and access penalties are independently accumulated without a common empirical unit. Calling those points `FGE km` now would overstate the model.

## Musculoskeletal Load

Metabolic work, travel time, perceived lower-limb exertion, force-based tissue
load, and injury risk are distinct quantities. No retained method supplies a
validated prospective “foot and leg punishment” scalar from map geometry and
trail metadata.

The closest established outcome measure is differential session RPE. A
completed activity can be assigned a lower-limb rating on a Borg category-ratio
scale, then multiplied by moving minutes:

`lower-limb session load = local or leg RPE × moving minutes`

This is an internal, retrospective measure in arbitrary units. Differential RPE
can separate perceived leg-muscle exertion from breathlessness, but controlled
low- and high-impact protocols show that leg RPE does not isolate mechanical
impact. It must not be presented as measured tissue damage.

Force-based cumulative load is closer to mechanical fatigue. Scheltinga et al.
sum the ninth powers of sensor-estimated peak vertical ground-reaction forces,
but the exponent is tissue- and model-dependent, sensor errors are amplified,
and the result disagreed with both session RPE and heart-rate load in their
small outdoor-running study. A route map lacks the gait, speed, footwear, pack,
body, and per-step force data needed to compute it.

MIDE supplies the strongest off-the-shelf route descriptor. Its movement axis
distinguishes smooth ground, regular paths, irregular or unstable ground,
hands-for-balance terrain, and climbing. It is deliberately a one-through-five
maximum grade, not cumulative load, so it should describe footing or supply a
predictor input rather than serve as the route total.

The least speculative product model therefore has standard boundaries and a
small explicit inference layer:

1. Present one extensive **lower-limb load** estimate rather than separate foot
   and muscle values.
2. Express it as **flat-gravel lower-limb-equivalent kilometers** (`FGLE km`):
   the distance on flat gravel predicted to produce the same lower-limb session
   load. `FGLE km` is a musculoskeletal equivalence, not the metabolic `FGE km`
   defined above.
3. Base the unpersonalized estimate on traversed distance by MIDE-like movement
   class, ascent, descent, and carried load. Treat technicality and hazards as
   separate route characteristics.
4. Optionally collect one post-hike lower-limb RPE and use completed routes for
   monotone personal calibration. With sparse history, compare against known
   completed routes instead of claiming universal capacity thresholds.
5. Keep expected moving time, hydration demand, and route risk separate.

The displayed ability judgment should be relative to personal evidence: for
example, “1.1× Long Proof” or “above your hardest completed route.” A universal
load score describes the route; it cannot determine whether a particular body,
on a particular day, can absorb it.

## Population Default

Without personal history, the most defensible scalar is
**flat-gravel joint-work-equivalent kilometers** (`FGJW km`). It describes
mass-normalized external lower-limb dose, not capacity, pain, damage, or injury
risk.

For grade `g`, let `P⁺(g)` and `P⁻(g)` be positive and negative lower-limb joint
power per kilogram. Define the grade factor:

`J(g) = (P⁺(g) + |P⁻(g)|) / (P⁺(0) + |P⁻(0)|)`

Nuckols et al. provide the following walking prior at 1.25 m/s:

| Grade | `J(g)` |
|------:|-------:|
| −15% | 1.69 |
| −10% | 1.24 |
| 0% | 1.00 |
| +10% | 1.18 |
| +15% | 1.57 |

A shape-preserving interpolation between these knots gives an asymmetric,
U-shaped grade curve without independent ascent, descent, and absolute-grade
penalties. Values outside the observed interval should carry low confidence
rather than polynomial extrapolation.

The route total is:

`FGJW km = Σ segment_length_km × J(segment_grade) × R(segment_roughness)`

Flat gravel defines `R = 1`. The retained uneven-terrain experiment supports
an initial `R ≈ 1.28` anchor for modest irregularity, but does not justify the
present talus, scramble, water, or bushwhack multipliers. MIDE movement grades
should remain a visible ordinal descriptor. More severe terrain can initially
raise uncertainty and technicality without manufacturing ratio-scale physics.

The score should be mass-normalized and require no sex, height, or body-weight
input:

- body mass scales both route work and the reference flat-gravel work, so it
  cancels in the equivalent distance;
- height changes gait and step count, but the same person also supplies the
  reference gait, and route metadata cannot recover enough of the residual to
  justify the input;
- sex and absolute body mass bear on capacity and injury susceptibility, which
  this route-dose score deliberately does not predict.

Carried load is different. If pack load becomes a planning input, body mass is
needed only to form a load ratio. A first-order factor
`(body_mass + pack_mass) / body_mass` is consistent with approximately
proportional increases in walking mechanics and ground-reaction forces, but
heavy-load damage remains nonlinear and outside an ordinary day-hike model.

Expected time remains a separate Wood-style prediction. A single personal
flat-ground-speed ratio may scale that prediction without changing its terrain
or slope response; Pitman et al. establish this as a useful first-order
personalization. Hydration, hazards, navigation, source confidence, access,
and route quality remain separate dimensions.

## Sources

See [the reference corpus](../references/README.md) for exact artifacts, provenance, neutral synopses, and source limitations.
