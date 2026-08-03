# Native Application CI Doctrine

- **Status:** Trailgen incubation; prepared for post-1.0 nucleation
- **Release coordinate:** Linux x86_64 with X11
- **Excluded coordinates:** Wayland, macOS, and Windows

CI schedules evidence. It does not own product law. Every consequential gate
must be an app-owned command that runs unchanged outside GitHub Actions,
produces one intelligible verdict, and can later move behind shared porcelain
without changing its contract.

## Evidence Units

Trailgen admits four units:

| Unit | Owner | Proves |
| --- | --- | --- |
| source | `scripts/check` | formatting, lints, unit and integration behavior, documentation construction |
| security | `scripts/audit` | the locked dependency graph has no admitted RustSec vulnerability, unmaintained crate, or yanked release |
| lifecycle | `scripts/verify-install` | the documented source installer works under a sterile non-default prefix, the installed GUI renders through X11, inert probes create no XDG state, and Cargo removes the tracked executable |
| native acceptance | `scripts/test-gui` | the optimized X11 product crosses real input, rendering, persistence, and external-oracle boundaries |

Each unit is complete at its own boundary. Workflow YAML may select an honest
subset of named GUI stories when hosted graphics cannot adjudicate production
budgets; it may not weaken, dilate, or reinterpret those budgets. The complete
`scripts/test-gui` remains a release gate on representative host graphics.

## Capability Law

An adopter first declares three disjoint platform sets:

1. **release-tested:** every advertised behavior is backed by the applicable
   source, lifecycle, and native acceptance evidence;
2. **supported:** deliberately carried behavior with explicit but possibly
   narrower evidence;
3. **unclaimed:** code may happen to compile or run, but the release promises
   nothing.

CI matrices are the projection of those sets, not an aspiration. Adding an OS,
window system, architecture, installer, package manager, network mode, tray,
or native dialog requires the evidence unit that can falsify that capability.
Absent capabilities create no empty jobs and no waivers.

The common baseline for a native Rust application is source verification,
dependency audit, and its declared installation lifecycle. GUI acceptance is
required only on release-tested GUI coordinates. Crates.io packaging, binary
archives, signing, update channels, and purge behavior acquire gates only when
the product actually claims them.

## Workflow Law

The workflow may:

- select a declared runner and install its host prerequisites;
- install the declared Rust toolchain and verification tools;
- cache rebuildable Cargo material;
- invoke one evidence unit;
- retain bounded failure artifacts.

It may not duplicate Cargo command lists, seed product fixtures, encode UI
gestures, invent timeouts, mutate release metadata, publish artifacts, or
silently broaden the support envelope. Third-party actions are pinned to
immutable commits and receive least privilege.

Hosted runners are suitable for deterministic functional stories under
software graphics. Production latency and cadence evidence requires a named,
representative host-GPU coordinate; a hosted software renderer cannot pass or
excuse those obligations.

## Lifecycle Law

Installation evidence uses a disposable prefix and sterile XDG roots. It
identifies the installed artifact through its public entrypoint, renders the
installed GUI through an uninstrumented native smoke, invokes only
side-effect-free shell probes, removes it through the documented
package-manager path, and proves the executable is gone. Uninstall preserves
projects and user state unless a separate, explicitly destructive purge
contract exists.

The lifecycle unit must evolve with the distribution claim. A future release
archive, desktop entry, icon, MIME registration, updater, or crates.io package
is not covered by the present Cargo source-install trial and must replace or
extend it with artifact-native evidence.

## Nucleation Gate

The native host has moved to `eternalist-apps`; CI machinery has not. HRRR and
Booru Viewer should adopt these semantic units with app-owned commands before
common CI code becomes admissible. The expected promotion targets remain:

- toolchain and Cargo-gate orchestration;
- sterile install-root and XDG containment;
- pinned-action workflow framing and failure-artifact retention;
- capability vocabulary for platform and GUI-test selection.

Product commands, package-manager choices, fixtures, user stories, supported
coordinates, and release policy remain app-owned inputs. A generated workflow
template is not the goal; a small agent-readable doctrine plus turnkey
porcelain earned from repeated implementations is.
