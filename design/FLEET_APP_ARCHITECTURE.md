# Eternalist Fleet Architecture

- **Status:** lifecycle extracted; application-primitive promotion underway
- **Recorded:** 2026-08-05
- **Scope:** Trailgen, HRRR, Adequate Booru Viewer, Eternalist Apps, Dwemer
  Poolrooms, and `egui-tester`

## North Star

An Eternalist application should tend toward thin, explicit domain glue over
typed `eternalist-apps` primitives, with its real binary verified from outside
through `egui-tester`. This is a Platonic direction, not permission to invent a
framework before reuse exists. Raw egui, direct Poolrooms composition, and
product-local UI remain lawful whenever no shared law has been proved.

The system has four owners:

| Owner | Law |
| --- | --- |
| Dwemer Poolrooms | Low-level physical GUI embodiment and living water |
| Eternalist Apps | Native lifecycle and reusable high-level logical application primitives |
| product repository | Domain meaning, workers, persistence projections, unpromoted UI, fixtures, oracles, and stories |
| `egui-tester` | External containment, native input, synchronization, capture, timing, and failure evidence |

## Rectified Names

**Mechanism** is a low-level physical GUI element. Buttons, rollers, sliders,
tiles, frames, material, motion, intrinsic interaction, and displaced water are
Poolrooms mechanisms.

**Application primitive** is a reusable logical state machine or composition.
Inspectors, managers, menus, storage interactions, loading assemblies, and
similar application-scale laws belong to Eternalist Apps after promotion.

**Product glue** maps domain state into primitives and interprets their typed
actions. It is expected to be thin, but domain complexity is not moved into a
shared crate merely to shorten an application entry point.

**Witness** is one-way, launch-sealed, post-present semantic telemetry used to
release acceptance waits. It is not a correctness oracle.

**Oracle** is external evidence such as pixels, durable files, process state,
protocol traffic, or behavior after a cold restart.

Ownership follows the governing invariant rather than the everyday noun. A
menu actuator may be a Poolrooms mechanism while its command model, routing,
storage, and composition form an Eternalist primitive.

## Dependency Law

The intended product topology, with `A → B` meaning that `A` depends on `B`, is:

```text
xyz-gui
  → xyz-contract
  → eternalist-apps
  → dwemer_poolrooms

eternalist-apps
  → dwemer_poolrooms
  ── egui-test feature ─→ egui-tester-witness

xyz-acceptance
  → xyz-contract
  → egui-tester
```

Poolrooms never depends on Eternalist. It remains independently usable by
native and WebGPU applications with unrelated layouts and interaction grammar.
Applications may depend directly on Poolrooms and may freely combine its
mechanisms with Eternalist and product-local UI.

`xyz-acceptance` is unpublished and lives in the product repository. It never
depends on `xyz-gui` or invokes product internals. The GUI never depends on the
tester controller. Its optional `egui-test` feature adds observation only.

## Primitive Contract

An Eternalist application primitive:

- accepts explicit state and dependencies;
- composes Poolrooms mechanisms rather than counterfeiting their appearance;
- owns one coherent logical interaction and failure law;
- emits standard semantic anchors and witness state where useful;
- returns typed responses or actions for the product to interpret;
- may own persistence-neutral UI state;
- remains composable beside raw egui, Poolrooms, and product-local UI.

It does not discover services, invoke domain commands, dictate a product
persistence schema, require a global panel registry, or close the set of
application roles. The resulting DSL is library-shaped, not registry-shaped.

Modules inside `eternalist-apps` are the default unit of organization. A new
crate requires a materially different dependency universe, target claim, or
release authority. Reusable widgets do not each receive a package.

## Promotion Law

A logical primitive is promoted through either gate:

1. Two applications use it with the same behavioral and failure law, and a
   further independent reuse is plainly expected.
2. Three applications use it identically, whether or not further reuse was
   predicted.

The complete operation is:

```text
incubate locally
→ prove with executable evidence
→ satisfy a promotion gate
→ state the smallest common law
→ extract
→ migrate every adopter
→ delete every local rival
→ publish and advance the fleet cohort
```

Structural resemblance alone proves nothing. Promotion does not preserve every
local variation behind options. A real divergence remains product-local until
another consumer proves its own law.

## Verification Law

Every consequential acceptance step retains:

```text
native gesture
→ temporally eligible post-present witness
→ rendered or external oracle
```

Acceptance executables live with products. `egui-tester` owns generic control
and evidence, not Booru, HRRR, or Trailgen stories. Checked-in xdotool
choreography may direct demos, but it cannot substitute for acceptance.

Poolrooms mechanisms carry unit, interaction, native-gallery, and WebGPU
evidence. Eternalist primitives carry focused fixture stories. Products prove
their user obligations over the integrated optimized binary.

X11 is the sole current native acceptance vertical. Compilation elsewhere does
not create a platform claim. Poolrooms' renderer-independent chrome and WebGPU
gallery have their own evidence and do not inherit the Eternalist host's X11
boundary.

## Cohorts And Interlocks

Factory lockstep means every shared-layer change must do one of three things:

1. remain compatible with every registered consumer;
2. arrive with coordinated consumer migrations;
3. carry an explicit, expiring divergence record.

A process-only fleet rig should record stable and forge cohorts, repositories,
exact revisions, shared versions, affected joints, and required gates. Shared
head changes fan out through ephemeral downstream canaries. Product lockfiles
retain exact reproducibility; compatible manifest ranges admit automated,
grouped cohort updates.

Touching one side of a registered duplicated joint requires `propagate`,
`diverge`, or `extract`. Silence is not a disposition.

A single Cargo workspace is not presently required. Co-location is justified
only when the joint ledger shows that changes truly require atomic source or
resolution. Governance and canaries come first.

## Release Law

Shared packages publish only from a clean, tagged checkout after their source
gate, `cargo package --locked`, and downstream head canaries pass. The manifest
version, tag, packaged VCS metadata, and commit must identify the same source.
`--allow-dirty` is forbidden.

Release order follows the dependency graph:

```text
Poolrooms and egui-tester witness/controller
→ Eternalist Apps
→ grouped application lockfile updates
```

A tested cohort, not a common release date or version number, defines fleet
synchrony.

## Current Joint Register

| Joint | Disposition |
| --- | --- |
| Native lifecycle | Extracted into Eternalist; HRRR migration is in progress and ABV remains |
| Inspector and LivingWait | Eternalist application primitives |
| HRRR and ABV shelved collection | Promote as an Eternalist `Cabinet` after exact model/UI law is reconciled under stories |
| Trailgen and HRRR map pin | Promote physical die into Poolrooms; keep logical map manipulation above it |
| Trailgen and ABV text plate | Promote physical control into Poolrooms |
| Trailgen and HRRR cartography | Watched joint only; extract the smallest factual kernel when a live change forces reconciliation |
| ABV probe and external acceptance | Replace with standard witness; move product stories into ABV |
| Demo directors | Keep distinct from acceptance while reusing semantic targets and containment machinery where proved |

## Falsification Gates

Revise this architecture if Eternalist primitives require product-named
branches, if products must route around shared primitives to remain correct, if
Poolrooms becomes dependent on Eternalist application policy, if typed
composition cannot preserve direct egui escape hatches, or if coordinated
release work repeatedly requires atomic repository changes that the fleet rig
cannot stage safely.
