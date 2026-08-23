# Releasing Trailgen

`foundry.toml` is the complete support and evidence declaration. Eternalist
Foundry schedules and judges it; each product gate remains independently
runnable:

| Evidence | Command | Product claim |
| --- | --- | --- |
| Source | `scripts/check` | formatting, lints, tests, and rustdoc |
| Security | `scripts/audit` | no unadjudicated RustSec finding |
| Cargo graph | `scripts/package` | all five publishable crates package and resolve together |
| Runtime | `scripts/prove-runtime` | public install, identity, first presentation, and removal |
| X11 | `scripts/test-gui` | complete native user stories and host-GPU budgets |
| Wayland | `scripts/test-wayland` | isolated launch, witnessed presentation, and nonblack capture |
| macOS | `scripts/package-macos` | unsigned universal DMG on arm64 and x86_64 |
| Windows | `scripts/package-windows.ps1` | unsigned current-user x86_64 NSIS lifecycle |

X11 owns full story parity. Wayland claims presentation, not synthetic native
input. Hosted software graphics proves function, not production latency.
macOS and Windows prove the ordinary packaged lifecycle; unsigned artifacts
retain the documented Gatekeeper and SmartScreen friction. Uninstall preserves
projects and application state.

`scripts/release VERSION publish` publishes the five-crate Cargo graph in
dependency order. Foundry may publish installers, checksums, and the support
manifest only after that exact `trailgen` version is visible on crates.io.
Release commits and annotated tags use the Eternalist identity and the
YubiKey-backed signing key.
