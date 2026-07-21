# Optimizer

## Objective

Trailgen separates physical effort from desirability:

- **Difficulty** is a target. Distance, climb, grade, substrate, technical pressure, navigation,
  confidence, and access evidence produce the scalar; ranking minimizes distance from the requested
  target inside the hard difficulty window.
- **Quality** is maximized. It is a length-normalized 0–100 score reduced by road exposure,
  uncertainty, and dubious access. Difficulty is deliberately absent, so a severe route can still
  be excellent.

Roads are not banned by default. `search.routing.road_aversion = 2` makes a fully road-exposed meter
cost three routing meters, while `constraints.max_road_fraction` remains available for an explicit
hard cap and defaults to `1`. Closed and private edges are never traversable. Pavement and road
terrain remain physically easier under the default difficulty multipliers; the optional
`difficulty.road_effort_penalty` is zero unless a project intentionally defines road walking as
extra effort.

## Support Designs

The canonical `Trail` is a shape, ordered geographic support points, and a routing law. Consecutive
support points are joined by the least-cost lawful graph path. Open trails end at the final point;
out-and-backs reverse the outward edge sequence exactly; loops add the least-cost return to the
trailhead. The GUI manual editor and editable library trails call this engine directly.

Search need not discover candidates in support-point space, but it must recover an exact design
before offering **Edit Trail**. Out-and-backs have a two-point form: trailhead and turnaround. For
loops and open routes, recursive arc contraction removes every control point whose neighboring walk
is already the least-cost lawful path. A candidate with an irreducible parallel-edge ambiguity is
saveable but is not falsely presented as editable. Figure-eight support topology remains undefined.

## Candidate Production

`RouteSolver` is the search seam. Loops use `LoopHunter`, a bounded edge-simple outward frontier
closed by routing-cost-ordered legal return paths. Small graphs may use `ExactLoopSolver`, which is
exhaustive only inside its hop, frontier, and distance envelope. `solver = "auto"` selects the exact
backend for graphs with at most 32 edges and the heuristic otherwise.

Out-and-backs bypass DFS prefix enumeration. One turn-aware, road-aware shortest-path frontier emits
the canonical shortest spine to each reachable turnaround and mirrors it. Dense chains therefore
produce one candidate per meaningful endpoint rather than combinatorial perturbations around the
same traversal.

The search envelope lives under `[search]`:

- `max_hops`: maximum outward edge count
- `max_frontier`: maximum expanded states
- `keep`: maximum retained portfolio
- `closure_paths`: return alternatives tried for a loop frontier
- `seed`: deterministic heuristic fanout jitter
- `routing.road_aversion`: finite road detour cost

The GUI defaults to 5,000 frontier states, one closure, 12 retained candidates, seed 2, and road
aversion 2. The CLI retains a larger diagnostic envelope.

## Ranking And Diversity

Every route is fully measured and judged before ranking. Hard constraint satisfaction is
lexicographic: no attractive violation outranks a lawful route. Within each tier, Pareto fronts use
constraint penalty, distance/ascent/descent window deviations, difficulty-target deviation, quality
loss, restricted-access fraction, and repeated-edge fraction. Scalar score then orders a front:

```text
score = constraint_penalty + 0.1 × (100 - quality)
```

Equivalent edge multisets collapse first. The diversity portfolio then measures length-weighted
edge overlap:

```text
distance(a, b) = 1 - shared_length / min(length(a), length(b))
```

It admits ranked routes at exclusion radii 0.35, 0.20, 0.08, then 0. This spends result slots on
different spines or lobes before allowing close variants, while still filling the gallery in a
sparse network. `--count` is consequently an upper bound, not a promise to fabricate duplicates.

## Reproduction

Both built-in solvers are deterministic for a fixed graph, effective config, start snap, seed, and
seed-route ledger. `trailgen generate` records those inputs, source fingerprints, graph summaries,
one-run overlays, exact edge sequences, measurements, verdicts, scores, ranks, and emitted artifact
fingerprints in `routes/generated.manifest.json`. `trailgen verify-generation` verifies the ledger,
replays metric and constraint computation, and reruns native generation before comparing candidates.

`trailgen formulate-milp` and `trailgen import-milp-solution` remain the exact external-solver seam
for connected simple loops. They share graph legality, measurements, reports, and artifact emission
with native generation rather than forming another product vertical.
