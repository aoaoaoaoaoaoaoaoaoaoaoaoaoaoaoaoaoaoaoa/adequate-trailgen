# Harriman Source Audit

Audited 2026-07-22 against the owned `harriman-south-lows.csv` and
`harriman-west.csv` traces. Distances below are measured along those traces.

## Verdict

No route interval is geometrically absent from the current public corpus. Every South Lows trace
segment lies within 2.74 m of the current OSM graph and every West segment within 5.23 m; neither
route contains a disconnected transition. The apparent holes are instead unnamed paths without
route relations, marking evidence, or useful condition tags. OSM alone therefore leaves their
wayfinding unknown. Trailgen now represents marked versus unmarked independently of standing and
can accept the recovered state classification without turning it into a current access warranty.

The present Harriman integration tests are deterministic solver replays over graphs derived from
the owned traces. They are not discovery tests over the public Harriman graph. A heuristic search
over a fresh public South graph, constrained only by length, ascent, descent, shape, and the former
prototype difficulty score,
found a different loop with only 41 of 922 owned trace segments within 20 m. Public-corpus
dogfooding therefore needs support points, not narrower scalar constraints.

## Semantically Missing Intervals

| Route interval | Length | Current OSM | Recovered evidence |
| --- | ---: | --- | --- |
| South 3.620–3.805 km | 186 m | unnamed `path`, way [`157767744`](https://www.openstreetmap.org/way/157767744/history) | NYS OPRHP 2014: `Unnamed`, `Unmarked Trail`, within 2.9 m |
| South 5.337–5.588 km | 252 m | unnamed `path`, way [`981873271`](https://www.openstreetmap.org/way/981873271/history) | Added from aerial imagery and Strava heatmap in [changeset 111044858](https://www.openstreetmap.org/changeset/111044858); no independent authoritative standing found |
| South 6.514–7.344 km | 830 m | unnamed `path`, way [`893180559`](https://www.openstreetmap.org/way/893180559/history) | NYS OPRHP 2014: three contiguous `Unnamed`, `Unmarked Trail` features within 5.1 m; an [independent route description](https://www.njhiking.com/best-hikes-pine-meadows-loop-harriman/) identifies the raised berms and pipes of an unfinished sewer system |
| South 8.130–8.535 km | 405 m | unnamed `path`, way [`153969020`](https://www.openstreetmap.org/way/153969020/history) | NYS OPRHP 2014: `Unnamed`, `Unmarked Trail`, within 16.6 m |
| West 12.969–13.386 km | 417 m | unnamed `path`, way [`982415773`](https://www.openstreetmap.org/way/982415773/history) | NYS OPRHP 2014 and current service: `Unnamed`, `Unmarked Trail`; current record `08020RT10414` says public, open, and foot-permitted |

South also uses unnamed connectors of 133 m at 14.617 km, 58 m at 14.939 km, and 36 m at
16.728 km. West makes a 68 m unnamed viewpoint excursion at 6.788 km and an unnamed 185 m final
connector at 22.619 km. None is a topological hole.

The 830 m South interval has unusually clear lineage. OSM originally recorded the corridor only as
an underground water pipeline in 2021. It became a `highway=path` on 2026-06-02 from NYS aerial
imagery and Strava heatmap evidence in [changeset 183564806](https://www.openstreetmap.org/changeset/183564806).
The 2014 state data already represented the same line as an unmarked trail. The current 2026 OPRHP
service no longer contains a coincident feature, so the stale state record is evidence of historical
standing, not a current access warranty.

## Source Findings

AllTrails is not the independent source of its ordinary dashed path network. Its [support
material](https://support.alltrails.com/hc/en-gb/articles/4410231246100-Verified-routes-vs-OSM-OpenStreetMap-segments)
says those segments come from OpenStreetMap, while AllTrails `Verified routes` are separately
hand-curated. Its [published derivation
methodology](https://support.alltrails.com/hc/en-us/articles/360019246411-OSM-Derived-Database-Derivation-Methodology)
selects OSM `path`, `track`, `footway`, `steps`, `bridleway`, and `cycleway` features. Consequently,
an AllTrails segment that now appears in OSM does not constitute a second corroborating provider.

The closest public AllTrails analogue for South is [Pine Meadow Lake, Tuxedo Rock, Breakneck, and
Panther Mountain](https://www.alltrails.com/trail/us/new-york/pine-meadow-lake-tuxedo-rock-breakneck-mountain-and-panther-mountain).
Its landmark order resembles South Lows and a 2025 review mentions an unmarked swamp section, but
the published route is shorter and the unauthenticated page exposes no geometry sufficient to prove
identity. No single public AllTrails page matched the custom West composition. Trailgen must not
treat inaccessible proprietary route geometry as a provider.

The strongest independent source is the official [NYS State Park Trails
dataset](https://data.ny.gov/Recreation/State-Park-Trails/7gkb-pzs9). Its 2014 shapefile distinguishes
`Marked Trail` and `Unmarked Trail`; the current [NY State Parks Trails FeatureServer](https://services.arcgis.com/1xFZPtKn1wKC6POA/ArcGIS/rest/services/NY_State_Parks_Trails/FeatureServer)
adds status, public-access, and permitted-use fields. The 2023 official [Harriman trail
map](https://parks.ny.gov/sites/default/files/HarrimanTrailMap.pdf) likewise distinguishes marked
trails, unmarked trails, woods roads, and pipelines. The [Trail Conference Harriman-Bear Mountain
map](https://store.nynjtc.org/products/harriman-bear-mountain-trails-map) explicitly maps
unmaintained trails and woods roads and is the strongest maintained cartographic reference, but is
a copyrighted retail product rather than an ingestible provider.

The current OPRHP service permits informational, non-commercial use with attribution. The older
data.ny.gov dataset declares no license. These sources may support this audit and manual comparison;
they cannot silently join Trailgen's redistributable default corpus without a compatible grant.

## Consequence

These loops do not justify a bushwhack router. Their geometry is routable today. They justify:

1. preserving the new separation of wayfinding (`marked`, `unmarked`, `unknown`) from physical
   standing, maintenance, and access as additional providers are admitted;
2. a public-corpus Harriman test whose ordered support points force the intended route and whose
   oracle verifies geometry, topology, wayfinding, and standing independently; and
3. treating the vanished current-state record for the South pipeline as temporal uncertainty, not
   silently converting a 2014 classification into a current access claim.

A bushwhack cost field and off-network router remain distinct future machinery for traces that
truly leave every known line.
