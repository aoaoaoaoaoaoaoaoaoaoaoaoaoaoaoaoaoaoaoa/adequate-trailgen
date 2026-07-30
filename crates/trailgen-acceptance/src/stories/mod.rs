mod compare;
mod discover;
mod manual;
mod refine;

use egui_tester::Result;

use crate::harness::Harness;

pub fn run(harness: &Harness<'_>) -> Result<()> {
    discover::run(harness)?;
    refine::run(harness)?;
    compare::run(harness)?;
    manual::run(harness)?;
    println!("trailgen acceptance passed: 4 user stories");
    Ok(())
}
