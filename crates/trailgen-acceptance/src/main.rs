mod fixture;
mod harness;
mod interactions;
mod observation;
mod performance;
mod showcase;
mod stories;

use std::{
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use egui_tester::{Backend, Error, Result, TestbedBuilder, WaylandConfig, X11Config};

use crate::showcase::Exposure;

const CLIPBOARD_PROBE: &str = "--clipboard-probe";

fn main() -> Result<()> {
    if env::args_os().nth(1).as_deref() == Some(OsStr::new(CLIPBOARD_PROBE)) {
        return print_clipboard();
    }
    let cli = Cli::parse()?;
    let binary = env::var_os("TRAILGEN_ACCEPTANCE_BINARY")
        .map(PathBuf::from)
        .map_or_else(harness::sibling_binary, Ok)?;
    let artifacts = cli
        .artifacts
        .or_else(|| env::var_os("TRAILGEN_ACCEPTANCE_ARTIFACTS").map(PathBuf::from));
    let bed = || {
        let builder = TestbedBuilder::default().backend(cli.backend);
        match &artifacts {
            Some(artifacts) => builder.failure_artifacts(artifacts),
            None => builder,
        }
    };
    match cli.mode {
        Mode::Showcase(exposure) => {
            let specimen = showcase::specimen(bed(), &binary)?;
            for artifact in
                bed().run(|testbed| showcase::run(testbed, &binary, &specimen, exposure))?
            {
                println!("{}", artifact.display());
            }
            Ok(())
        }
        Mode::Smoke => rehearse(bed(), &binary, artifacts.as_deref(), |harness| {
            stories::smoke(harness, cli.backend)
        }),
        Mode::Stories(story) => rehearse(bed(), &binary, artifacts.as_deref(), |harness| {
            stories::run(harness, story.as_deref())
        }),
    }
}

/// Perform one acceptance act against the fixture world in a fresh testbed.
fn rehearse(
    bed: TestbedBuilder,
    binary: &Path,
    artifacts: Option<&Path>,
    act: impl FnOnce(&harness::Harness<'_>) -> Result<()>,
) -> Result<()> {
    bed.run(|testbed| {
        let fixtures = fixture::FixtureWorld::raise(testbed)?;
        act(&harness::Harness::new(
            testbed, binary, &fixtures, artifacts,
        ))
    })
}

fn print_clipboard() -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().map_err(|error| Error::Verdict {
        detail: format!("open native clipboard: {error}"),
    })?;
    let contents = clipboard.get_text().map_err(|error| Error::Verdict {
        detail: format!("read native clipboard: {error}"),
    })?;
    print!("{contents}");
    Ok(())
}

/// One acceptance-executable invocation: the launch smoke, standing stories,
/// or the showcase rail.
enum Mode {
    Smoke,
    Stories(Option<String>),
    Showcase(Exposure),
}

struct Cli {
    mode: Mode,
    artifacts: Option<PathBuf>,
    backend: Backend,
}

impl Cli {
    fn parse() -> Result<Self> {
        let mut args = env::args_os().skip(1);
        let mut mode = None;
        let mut artifacts = None;
        let mut backend = Backend::X11(X11Config::default());
        while let Some(argument) = args.next() {
            let chosen = match argument.to_str() {
                Some("--story") => Mode::Stories(Some(
                    required(&mut args, "--story")?
                        .to_string_lossy()
                        .into_owned(),
                )),
                Some("--stills") => Mode::Showcase(Exposure::Stills(PathBuf::from(required(
                    &mut args, "--stills",
                )?))),
                Some("--film") => Mode::Showcase(Exposure::Film(PathBuf::from(required(
                    &mut args, "--film",
                )?))),
                Some("--smoke") => Mode::Smoke,
                Some("--artifacts") => {
                    artifacts = Some(PathBuf::from(required(&mut args, "--artifacts")?));
                    continue;
                }
                Some("--backend") => {
                    let value = required(&mut args, "--backend")?;
                    backend = match value.to_str() {
                        Some("x11") => Backend::X11(X11Config::default()),
                        Some("wayland") => Backend::Wayland(WaylandConfig::default()),
                        Some(value) => {
                            return Err(Error::Verdict {
                                detail: format!(
                                    "unknown acceptance backend `{value}`; expected x11 or wayland"
                                ),
                            });
                        }
                        None => {
                            return Err(Error::Verdict {
                                detail: "acceptance backend must be valid Unicode".to_owned(),
                            });
                        }
                    };
                    continue;
                }
                Some(flag) => {
                    return Err(Error::Verdict {
                        detail: format!(
                            "unknown acceptance option `{flag}`; use --smoke, --story NAME, --stills DIR, --film DIR, --backend x11|wayland, or --artifacts PATH"
                        ),
                    });
                }
                None => {
                    return Err(Error::Verdict {
                        detail: "acceptance options must be valid Unicode".to_owned(),
                    });
                }
            };
            if mode.replace(chosen).is_some() {
                return Err(Error::Verdict {
                    detail: "--smoke, --story, --stills, and --film are mutually exclusive"
                        .to_owned(),
                });
            }
        }
        let mode = mode.unwrap_or(Mode::Stories(None));
        if matches!(backend, Backend::Wayland(_)) && !matches!(mode, Mode::Smoke) {
            return Err(Error::Verdict {
                detail: "Wayland admits launch-and-capture smoke only; native stories require X11"
                    .to_owned(),
            });
        }
        Ok(Self {
            mode,
            artifacts,
            backend,
        })
    }
}

fn required(args: &mut impl Iterator<Item = OsString>, option: &'static str) -> Result<OsString> {
    args.next().ok_or_else(|| Error::Verdict {
        detail: format!("{option} requires a value"),
    })
}
