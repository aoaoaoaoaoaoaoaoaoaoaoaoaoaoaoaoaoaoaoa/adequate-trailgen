mod fixture;
mod harness;
mod interactions;
mod observation;
mod performance;
mod stories;

use std::{env, path::PathBuf};

use egui_tester::{Result, TestbedBuilder};

fn main() -> Result<()> {
    let binary = env::var_os("TRAILGEN_ACCEPTANCE_BINARY")
        .map(PathBuf::from)
        .map_or_else(harness::sibling_binary, Ok)?;
    let artifacts = env::args_os()
        .nth(1)
        .or_else(|| env::var_os("TRAILGEN_ACCEPTANCE_ARTIFACTS"))
        .map(PathBuf::from);
    let mut builder = TestbedBuilder::default();
    if let Some(artifacts) = &artifacts {
        builder = builder.failure_artifacts(artifacts);
    }
    builder.run(|testbed| {
        let fixtures = fixture::FixtureWorld::raise(testbed)?;
        let harness = harness::Harness::new(testbed, &binary, &fixtures, artifacts.as_deref());
        stories::run(&harness)
    })
}
