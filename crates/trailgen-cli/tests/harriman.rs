use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use trailgen_core::io::csv;

struct HarrimanCase {
    fixture: &'static str,
    name: &'static str,
    bbox: &'static str,
    start: &'static str,
    distance_km: (f64, f64),
    ascent_m: (f64, f64),
    max_lower_limb_load_km: f64,
}

const SOUTH_LOWS: HarrimanCase = HarrimanCase {
    fixture: "harriman-south-lows.csv",
    name: "Harriman South Lows",
    bbox: "-74.158,41.165,-74.065,41.217",
    start: "-74.12966,41.19856",
    distance_km: (21.5, 22.1),
    ascent_m: (500.0, 620.0),
    max_lower_limb_load_km: 700.0,
};

const WEST: HarrimanCase = HarrimanCase {
    fixture: "harriman-west.csv",
    name: "Harriman West",
    bbox: "-74.180,41.198,-74.113,41.271",
    start: "-74.15431,41.26478",
    distance_km: (22.6, 23.2),
    ascent_m: (950.0, 1_100.0),
    max_lower_limb_load_km: 1_200.0,
};

#[test]
fn solver_replays_harriman_south_lows_from_owned_trace() {
    replay_owned_trace(&SOUTH_LOWS);
}

#[test]
fn solver_replays_harriman_west_from_owned_trace() {
    replay_owned_trace(&WEST);
}

fn replay_owned_trace(case: &HarrimanCase) {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    let fixture = fixture(case.fixture);

    trailgen([
        "init",
        path(project),
        "--name",
        case.name,
        &format!("--bbox={}", case.bbox),
    ]);
    trailgen([
        "build",
        path(project),
        "--source",
        path(&fixture),
        "--snap-tolerance-m",
        "2",
    ]);
    trailgen([
        "generate",
        path(project),
        &format!("--start={}", case.start),
        "--min-km",
        &case.distance_km.0.to_string(),
        "--max-km",
        &case.distance_km.1.to_string(),
        "--min-ascent-m",
        &case.ascent_m.0.to_string(),
        "--max-ascent-m",
        &case.ascent_m.1.to_string(),
        "--min-descent-m",
        &case.ascent_m.0.to_string(),
        "--max-descent-m",
        &case.ascent_m.1.to_string(),
        "--max-lower-limb-load-km",
        &case.max_lower_limb_load_km.to_string(),
        "--max-low-confidence-fraction",
        "1",
        "--shape",
        "loop",
        "--solver",
        "exact",
        "--max-hops",
        "1100",
        "--max-frontier",
        "1000000",
        "--keep",
        "4",
        "--count",
        "1",
        "--seed",
        "0",
    ]);
    trailgen(["verify-generation", path(project)]);

    let routes: Value = serde_json::from_slice(
        &std::fs::read(project.join("routes/generated.routes.json")).unwrap(),
    )
    .unwrap();
    let route = &routes.as_array().unwrap()[0];
    assert_eq!(route["metrics"]["shape"], "loop");
    assert_eq!(route["verdict"]["satisfied"], true);
    assert_in(
        route["metrics"]["distance_m"].as_f64().unwrap() / 1_000.0,
        case.distance_km,
    );
    assert_in(
        route["metrics"]["ascent_m"].as_f64().unwrap(),
        case.ascent_m,
    );
    assert_in(
        route["metrics"]["descent_m"].as_f64().unwrap(),
        case.ascent_m,
    );

    let source = csv::route_line_from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    let generated = csv::route_line_from_str(
        &std::fs::read_to_string(project.join("routes/candidate-1.csv")).unwrap(),
    )
    .unwrap();
    let recovered = source
        .points
        .iter()
        .filter(|point| {
            generated
                .points
                .iter()
                .any(|candidate| point.haversine_m(*candidate) <= 2.0)
        })
        .count();
    assert!(
        recovered * 1_000 >= source.points.len() * 995,
        "only {recovered}/{} source points survived generation",
        source.points.len()
    );
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn assert_in(value: f64, (min, max): (f64, f64)) {
    assert!(
        (min..=max).contains(&value),
        "{value} outside {min}..={max}"
    );
}

fn trailgen<const N: usize>(args: [&str; N]) {
    let output = Command::new(env!("CARGO_BIN_EXE_trailgen"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "trailgen failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
