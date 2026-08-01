# Shared Native Application Architecture

- **Status:** evidence-gated design with a green Trailgen incubation
- **Recorded:** 2026-07-30
- **Scope:** Trailgen, HRRR, Booru Viewer, Dwemer Poolrooms, and `egui-tester`

This document records the intended ownership boundaries and extraction order
for the native egui application fleet. It incorporates an independent,
read-only Fable inspection of all five repositories. It is not authorization
to manufacture the terminal crate graph before its seams have been proved.

## Decision

The fleet needs three distinct layers:

1. Dwemer Poolrooms owns visual and physical primitives.
2. A shared application shell may own native host mechanics that the
   applications have independently reproduced.
3. `egui-tester` owns external control, containment, timing, and judgment.

Product repositories retain their domain models, application-specific
presentation, fixtures, oracles, and immature mechanisms.

The first implementation increment is tester porcelain written in ordinary
Rust. Proc macros remain the expected eventual authoring surface, but their
natural language must be recovered from a proven runtime rather than invented
in advance.

The local [Native Responsiveness Doctrine](RESPONSIVENESS_DOCTRINE.md) governs
the current shell instrumentation experiment and its promotion gates.

`egui-tester` and the future shell remain separate repositories for now. Their
only required runtime seam is `egui-tester-witness`. Co-location may be
reconsidered if sustained co-evolution supplies evidence for it.

Applications may depend directly on Dwemer Poolrooms. The shell must not
interpose a counterfeit wrapper around every Poolrooms control or flourish.
Both the shell and an application may use Poolrooms directly, provided Cargo
resolves one compatible version.

## Names

**Shell** is the shared native host: event loop, rendering lifecycle, window,
presentation, and narrowly proven host policy. It is not the application.

**Command** is a stable user intention such as save, undo, or begin search. A
button, menu item, and shortcut may all invoke the same Command.

**Target** is a visible recipient of a gesture. `Support(3)`, a map canvas, and
a saved-trail row are Targets. A Target's statically admitted gesture classes
are distinct from its dynamic availability in the current application mode.

**Coordinate space** gives a destination meaning. Geographic coordinates,
profile distance, and window pixels are different coordinate spaces.

**Latency class** names a product obligation such as immediate reaction,
durable commit, or sustained pan cadence.

**Observation** is an acceptance-owned, deliberately partial interpretation of
one-way product telemetry. It releases waits; it does not decide correctness.

**Witness** is the versioned, launch-sealed frame envelope carrying timestamps,
targets, scale, and serialized product telemetry.

**Oracle** is external product evidence: pixels, durable files, process
behavior, platform accessibility, or a later cold start.

**Contract** is the shared product vocabulary of identity, Commands, Targets,
coordinate spaces, and latency classes. It does not contain domain behavior,
fixtures, oracles, or a mirror of application state.

`eternalist` is not yet an admissible framework name: `eternalist.moe` already
names another project in the same fleet namespace. The shell remains unnamed
until this collision is resolved.

## Dependency Topology

The intended product topology, with `A → B` meaning that `A` depends on `B`,
is:

```text
xyz-gui
  → xyz-contract
  → dwemer_poolrooms
  → trailgen-shell               private, provisional incubation

trailgen-shell
  → dwemer_poolrooms
  ── egui-test feature ─→ egui-tester-witness

xyz-acceptance
  → xyz-contract
  → egui-tester
```

`xyz-acceptance` is unpublished and lives in the product repository. It never
depends on `xyz-gui` or calls product internals. It launches the optimized
application binary and crosses the same native input and rendering boundaries
as a user.

`xyz-gui` never depends on the tester controller. Its optional test feature
adds one-way observation only.

The future `xyz-contract` may depend on a small shared contract-language crate.
That shared crate will contain handwritten traits and descriptors. A proc
macro companion may later compile declarations into this algebra, but the
algebra remains manually implementable and semantically authoritative.

No common contract-language crate should be published before two products
demonstrate the same vocabulary. A product-specific contract may incubate
locally before that extraction.

`trailgen-contract` is that first incubation. It depends only on Serde and owns
application identity, schema fingerprint, typed Target wire names, and the
small UI-state enums shared across the observation boundary. Tester gesture
methods accept any `Display` value, so the contract does not know the tester;
the GUI and acceptance executable nevertheless consume the same vocabulary.
Commands, coordinate descriptors, latency classes, derives, and a shared
contract-language crate remain deferred until repeated use gives them a
natural shape.

## Contract Boundary

The initial lawful contract surface is:

- application and contract identity;
- Commands and their labels, shortcuts, focus scopes, and named latency
  classes;
- Targets and their stable wire identities;
- typed coordinate spaces and their boundary transforms;
- a schema fingerprint checked before the first injected input.

The contract does not initially own:

- product witness-state structures;
- fixtures or external oracles;
- panel layout or responsive policy;
- domain entities merely because a Target refers to them;
- fleet capabilities, doctrine rules, or waiver machinery;
- background-work semantics beyond an identity that a proven cancellation
  interaction requires.

The application serializes a minimal telemetry projection. Acceptance defines
its own partial `Deserialize` Observation containing only the fields that its
stories consume. This asymmetry is intentional: it keeps the consumer
authoritative and makes product telemetry less tempting as a correctness
oracle.

Static Target metadata describes gesture potential, not current permission.
For example, a support pin may admit dragging in principle while the current
mode, focus scope, or transaction makes it unavailable. Dynamic availability
belongs to the presented UI and its witness or accessibility tree.

## Tester Architecture

The `egui-tester` hard kernel survives:

- private display and filesystem;
- cgroup and process lifecycle;
- declared network authority;
- native input and action receipts;
- temporally eligible observation and surface-present fencing;
- screenshot and external-oracle machinery;
- reaction and sustained-cadence budgets;
- transcripts and failure artifacts.

The first porcelain layer uses ordinary Rust:

1. `Story<O>` binds the application, session, probe, and default reaction
   budget.
2. Acceptance-local typed Observations remove raw JSON navigation.
3. Target identity removes duplicated anchor strings.
4. Predicate combinators are values with structured failure diagnostics.
5. Gesture methods own receipt creation, fresh-frame waits, and budget
   adjudication.
6. File and pixel proof must be as cheap to express as witness predicates.
7. One launch-sealed observation journal preserves every semantic frame in
   order; `Probe` retains its own newest complete frame for targeting.

The handwritten porcelain materially contracts Trailgen's four stories
without weakening their gestures, oracles, restart checks, or performance
obligations. Exact line-count targets are rejected as architectural evidence:
the remaining ceremony must be compared against a second product before it can
define a language.

A proc-macro story language becomes justified only after this runtime has at
least two substantial consumers and repeated residual syntax remains. The
macro must be a compiler front end over the ordinary runtime, preserve source
spans, emit a flat execution plan, support explanation, and leave the raw API
usable. It must not own causal semantics or generate a tower of nested generic
types.

Every accepted story retains the law:

```text
native gesture
    → temporally eligible surface-submitted observation
    → rendered or external oracle
```

No checked-in xdotool choreography may substitute for a missing tester
capability.

## Platform Doctrine

X11 is the sole release-tested acceptance vertical. It must remain complete
across private display authority, real input, capture, presentation fencing,
reaction budgets, cadence evidence, and failure artifacts. The existing
headless Wayland capture smoke makes no parity claim. Generic Wayland input,
compositor policy, and platform-specific acceptance are deferred until the X11
architecture survives another product adoption; horizontal expansion may not
destabilize the proven vertical.

## Shell Boundary

Trailgen now contains the unpublished `trailgen-shell` incubation. The former
local `boiler.rs` has been deleted; all standing release-mode X11 stories and
the dense host-GPU cadence contract cross the shell seam. Its crate name and
repository remain provisional until HRRR supplies the second consumer.

The incubation includes only mechanics already reproduced across the
applications:

- winit lifecycle and repaint deadlines;
- window and egui/wgpu surface construction;
- resize, DPI, surface loss, and recovery;
- egui input, tessellation, texture updates, and rendering;
- Poolrooms water composition and final presentation;
- application GPU-resource registration;
- fatal-error propagation;
- optional post-surface-present observation enqueue;
- ordinary close disposition.

The shell should expose one small application trait or an equivalently bounded
set of explicit hooks. Likely variation points are application construction,
one UI pulse, GPU resource registration, water-frame composition, close
disposition, and optional diagnostics. A general plugin registry and a growing
constructor option bag are rejected.

Product-specific tray implementation, map rendering, domain workers, Booru
debug capture, and storage/cache policy remain local. A divergent application
may keep a local mechanism rather than purchasing one more permanent shell
option.

XDG root resolution, concurrent-instance locking, tray presence, and native
dialogs remain outside this first seam. They may enter only after a second
application proves one common law for them.

Semantic panel roles, responsive `ShellPlan` archetypes, a common Activity
state machine, first-run orchestration, migration, and corruption recovery
remain design candidates. Their laws may be documented and tested locally, but
their code is not shared until a second independent implementation proves the
same semantics.

## Poolrooms Boundary

Dwemer Poolrooms owns:

- fonts, palette, spacing, and visual materials;
- machined controls and their intrinsic interaction semantics;
- water simulation, forcing, and composition primitives;
- intrinsic accessibility information for each control.

Poolrooms does not own application Commands, Target identities, panel roles,
product lifecycle, persistence, or test witnesses.

Direct `xyz-gui → dwemer_poolrooms` dependency is expected. Applications must
be free to compose controls, water, and custom chrome without routing every
choice through the shell. The shell may also depend on Poolrooms for its own
host-level composition.

Poolrooms' present string-anchor instrumentation is transitional. It may be
deleted only after its current consumers have migrated and AccessKit or typed
target publication has demonstrated behavioral parity. Booru's demo machinery
currently consumes those anchors, so deletion cannot precede that migration.

## AccessKit

AccessKit is not currently active in any of the inspected applications.
Poolrooms already emits egui `WidgetInfo`, which provides a plausible
substrate, but stable tree identity, final-pass bounds, custom-canvas
representation, platform behavior, and frame cost remain unproved.

Adoption therefore proceeds as a parity experiment:

1. Enable the egui-to-AccessKit path in one instrumented application.
2. Observe the tree without using accessibility actions as input.
3. Resolve ordinary controls from the tree, then inject native input at their
   bounds.
4. Compare identities and bounds against the standing typed-anchor stories.
5. Represent custom canvas entities accessibly where semantics are honest;
   retain typed custom targets where an accessibility node is unsuitable.
6. Delete an anchor path only after every consumer passes the same stories.

AccessKit is targeting and accessibility evidence, not proof of correct
pixels. Generic Wayland input also remains absent: the present Wayland backend
captures output but cannot inject compositor-authorized input.

## Evidence Ledger

The 2026-07-30 repository inspection established:

- Poolrooms instrumentation, `egui-tester-witness`, and Booru's probe form
  three competing anchor systems.
- Booru's acceptance executable lives in the tester repository rather than the
  product repository.
- HRRR has no acceptance suite or witness integration and remains several
  Poolrooms releases behind the other applications.
- Native dialogs, tray choreography, generic Wayland input, and deterministic
  time control remain outside the tester's evidence surface.
- Visual golden and perceptual comparison remain weaker than the map-heavy
  products require.

Trailgen closed two original defects during this incubation:

- provider fixtures now speak ordinary HTTP over a filesystem Unix socket
  inside the disposable test root while the product retains `Network::Deny`;
- `egui-tester-witness` now appends every semantic frame to one lossless
  observation journal, and reaction waits select the earliest temporally
  eligible match rather than the latest polled snapshot.

The tester-independent `trailgen-contract` also proves the first shared
vocabulary: the GUI and acceptance executable consume one Target enum plus
wire-state enums and verify `trailgen.ui/2` before the first input. These
closures are evidence for the present boundary, not permission to expand it.

## Promotion Law

A common mechanism crosses the shared boundary only through:

```text
incubate locally
    → prove with executable evidence
    → encounter a second independent consumer
    → state the common law
    → extract
    → migrate every adopter
    → delete every local copy
    → publish and pin coherently
```

A `utils` crate is forbidden. Structural similarity does not establish shared
semantics. An extraction that leaves local rivals or permanent compatibility
adapters has not completed.

The native host loop already has three independent implementations and has
earned an extraction attempt, but Trailgen's acceptance suite must guard the
first port. Cartography has two factual consumers but should wait for HRRR's
next real map requirement so that extraction answers a current need rather
than preserving the present duplication in amber.

## Implementation Order

1. **Completed:** add ordinary-Rust story context, typed observations, Target
   identity, and diagnostic predicate combinators to Trailgen.
2. **Completed:** deny IP networking, use a private Unix-socket HTTP fixture,
   and make semantic-frame delivery lossless and polling-independent.
3. **Completed:** extract the private `trailgen-shell` crate under all four
   release-mode X11 stories and host-GPU cadence evidence.
4. Write HRRR acceptance stories before changing its runtime; cover boot,
   first run, offline behavior, and one product-defining field interaction.
5. Bring HRRR to the current Poolrooms generation, then port it to the shell
   through a narrow tray-presence seam.
6. Move Booru's acceptance executable into Booru, port Booru to the shell, and
   retire its private probe only after its demo and acceptance consumers have
   migrated.
7. Extract the shared contract algebra after two products demonstrate the same
   Command, Target, coordinate, and latency vocabulary.
8. Run the AccessKit parity experiment and remove obsolete anchor machinery
   only after equivalent evidence is green.
9. Extract cartography when a real HRRR map change forces Trailgen and HRRR to
   reconcile their diverged implementations.
10. Add the story proc-macro front end if two product suites still exhibit
    repeated authoring ceremony over the proven runtime.

Fleet rule registries, capability-generated suites, codemods, and waiver
machinery remain deferred until fleet scale or repeated manual conformance
work supplies a payer.

## Falsification Gates

The design must be revised if:

- the ordinary-Rust porcelain cannot materially contract Trailgen without
  weakening evidence;
- the shell requires product-named branches or an expanding option inventory;
- HRRR or Booru must circumvent the shell's lifecycle to retain existing
  behavior;
- shared Command identity slows ordinary UI iteration enough that applications
  route around it;
- AccessKit cannot provide stable ordinary-widget identity and bounds;
- witness convenience causes stories to lose external or rendered oracles;
- version skew forces applications onto incompatible shell, Poolrooms, or
  witness generations.

## Open Decisions

- the shell and contract-language names;
- whether later co-evolution justifies one shell/tester workspace;
- whether the Unix-socket HTTP fixture should become tester porcelain after a
  second product needs it;
- the eventual platform path for native Wayland input, explicitly deferred
  until the X11 vertical survives another adoption;
- native dialog and tray-host testing;
- the visual-golden and perceptual-comparison contract;
- whether layout roles and Activity semantics eventually earn shared code;
- the residual syntax that will determine the proc-macro language.
