# Native Application CI Doctrine

- **Scheduler and judge:** Eternalist Foundry
- **Release-tested coordinates:** Linux x86_64/X11/Vulkan,
  Linux x86_64/Wayland/Vulkan, macOS arm64/Metal, macOS x86_64/Metal, and
  Windows x86_64/DX12
- **Delivery:** Cargo on Linux, unsigned universal DMG on macOS, unsigned
  current-user NSIS installer on Windows

`foundry.toml` is the complete support and evidence declaration. The pinned
reusable workflow schedules that declaration, seals each proof receipt, judges
the complete graph, and publishes only judged release artifacts. Product law
remains in the commands below; workflow YAML contains no copied Cargo command
lists, fixtures, gestures, or release policy.

## Evidence Units

| Unit | Owner | Proves |
| --- | --- | --- |
| source | `scripts/check` | formatting, lints, unit and integration behavior, and documentation construction |
| security | `scripts/audit` | the locked dependency graph has no unadjudicated RustSec finding |
| source package | `scripts/package` | every publishable crate resolves and verifies together through Cargo's workspace packaging transaction |
| runtime | `scripts/prove-runtime` | host compilation, public Cargo install/uninstall, CLI identity, and first presentation on every declared coordinate |
| X11 acceptance | `scripts/test-gui` | full optimized user stories through private X11 input, capture, persistence, and external oracles |
| Wayland smoke | `scripts/test-wayland` | isolated compositor launch, witnessed surface presentation, and nonblack output capture |
| macOS artifact | `scripts/package-macos` | universal unsigned DMG structure, both architectures, public identity, first presentation, and mounted-artifact lifecycle |
| Windows artifact | `scripts/package-windows.ps1` | unsigned current-user NSIS install, identity, first presentation, uninstall, user-data preservation, and checksum |

X11 owns native input and full story parity. Wayland owns only launch,
surface-present synchronization, and output capture; the support claim does
not counterfeit compositor-independent input injection. macOS and Windows
prove the ordinary product and packaged lifecycle but do not claim the X11
story suite.

## Capability Law

`release-tested`, `excluded`, and unclaimed coordinates are disjoint. A new
operating system, architecture, window system, renderer, installer, or trust
mode enters the support manifest only with an evidence unit that can falsify
it. Empty jobs and waivers are forbidden.

Hosted software graphics proves bounded functional progress, not production
latency. The complete X11 suite remains a release gate on representative host
graphics where its performance budgets apply.

## Lifecycle Law

Cargo delivery is the Linux contract; no distribution package is claimed.
Installation evidence uses a disposable prefix, invokes only the public
entrypoint, removes it through Cargo, and proves the executable is gone.

The DMG is universal for Apple Silicon and Intel. The NSIS installer is x86_64
and current-user. Both artifacts are deliberately unsigned during incubation;
their public installation notes must state the resulting Gatekeeper or
SmartScreen friction. Uninstall preserves projects and application state.

## Release Law

Every release commit and annotated tag uses the Eternalist identity and
YubiKey-backed signing key. `scripts/release VERSION publish` publishes the
five-crate Cargo graph in dependency order. A version tag lets Foundry rejudge
the same support graph, but Foundry cannot publish the DMG, NSIS installer,
checksums, and support manifest until the exact `trailgen` version is visible
on crates.io. The public project ledger must name only those coordinates and
artifact URLs.
