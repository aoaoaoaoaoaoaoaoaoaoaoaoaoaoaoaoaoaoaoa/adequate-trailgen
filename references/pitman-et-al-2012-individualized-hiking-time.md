# Pitman et al. (2012): Individualized Hiking Time Estimation

**Citation.** Arthur Pitman, Markus Zanker, Johann Gamper, and Periklis
Andritsos. “Individualized Hiking Time Estimation.” *2012 23rd International
Workshop on Database and Expert Systems Applications*, 2012, pp. 101–105.

- Work identity: DOI 10.1109/DEXA.2012.51
- Canonical source: https://doi.org/10.1109/DEXA.2012.51
- Local artifact: none; no redistribution license was identified
- Version and status: published conference paper; author manuscript inspected at https://www.cs.toronto.edu/~periklis/pubs/dexa12.pdf
- Retrieved: 2026-08-02
- SHA-256: not applicable
- Access and retention: metadata only; publicly readable author manuscript
- Synopsis basis: full-text inspection

## Synopsis

The authors fit a linear hiking-speed model to 360 GPS traces from South Tyrol.
Its predictors include directional grade, elapsed route fraction, cumulative
ascent and descent, and total route length. Stops and very slow segments are
removed, so the outcome is moving time rather than elapsed itinerary time.

Personalization is one multiplicative factor derived from the hiker's average
speed on segments between −5° and +5° grade. During a hike, the factor can
instead be updated from the ratio between elapsed measured and predicted time.
Against a withheld half of the traces, the progressively individualized model
reduces mean absolute relative error by as much as 23% versus Naismith's rule
between 20% and 80% route completion.

The model also includes route-progress and route-length effects. The latter may
encode selection, because experienced hikers preferentially undertake longer
routes. Terrain and weather are absent. Most traces came from different hikers
and routes, and the web-derived cohort is acknowledged to skew young and
technically inclined; the study therefore supports scalar calibration as a
useful first approximation rather than a stable physiological law.

## Source Assessment

The paper directly establishes flat-ground speed as a practical one-scalar
personalization seam. Its richer population function predates larger modern
terrain-aware datasets and should not displace them wholesale.
