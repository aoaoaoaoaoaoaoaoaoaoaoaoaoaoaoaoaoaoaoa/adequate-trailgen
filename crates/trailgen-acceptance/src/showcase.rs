//! Trailgen's showcase: one presentation take discovering, refining, and
//! comparing long-day loops across Harriman and Bear Mountain State Parks.
//! It performs ordinary semantic acts against a real public specimen and is
//! never a verdict.

use std::{
    fmt::Write as _,
    fs::{self, File, TryLockError},
    path::{Path, PathBuf},
    time::Duration,
};

use egui_demo::{Showcase, specimen_cache};
use egui_tester::{
    Anchor, AppCommand, ArtifactStem, Button, Error, Graphics, Key, Motion, Network,
    ReactionBudget, Result, Silent, StoryObserver, Testbed, TestbedBuilder, Wheel, demand,
};
use num_traits::ToPrimitive as _;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use trailgen_contract::UI_FINGERPRINT;
use trailgen_core::GRAPH_FORMAT;
use trailgen_data::PROJECT_FORMATS;

use crate::{
    harness::{
        Target, TargetClass, TrailFrame, TrailStory, WITNESS, bind, first_anchor,
        functional_budget, map_pixel, verdict, world_from_coord,
    },
    interactions::{create_project, reveal_inspector_target},
    observation::{CorpusPhase, MapState, SearchPhase, View, Workspace, shows},
};

const PRODUCT: &str = "trailgen";
const FILM: &str = "trailgen-showcase";
/// Layout of a sealed specimen and the forge that seals it.
const SPECIMEN_FORMAT: u32 = 1;
const FORGING: &str = ".forging-";
/// Survey tiles stream from the network; framing waits this long per step.
const SURVEY_PATIENCE: Duration = Duration::from_mins(2);
const PROJECT_NAME: &str = "Harriman · Bear Mountain";
/// The deck's slug of [`PROJECT_NAME`], relative to the private root.
const PROJECT: &str = "harriman-bear-mountain";
/// Harriman and Bear Mountain State Parks as west, south, east, north.
const AREA: [f64; 4] = [-74.20, 41.16, -73.96, 41.34];
/// Lake Skannatati, where the Long Path and Arden–Surebridge trails meet.
const TRAILHEAD: [f64; 2] = [-74.1036, 41.2361];
const DISTANCE_MAX_KM: &str = "20";
/// A trail vertex on Black Rock Mountain's summit, 421 m.
const SUMMIT: [f64; 2] = [-74.133_942_1, 41.240_458_3];
/// Enlarged type, so the take still reads at half size.
const PREFERENCES: &str = "font_scale = \"large\"\n";
/// The operator's roaming tile cache, relative to the XDG cache home.
const ROAMING: &str = "trailgen/protomaps-v4";
const ROAMING_BOOTSTRAP: &str = "bootstrap-us.pmtiles";
const ROAMING_ZOOM: u32 = 15;
/// Wheel notches bounding the continental-to-parks survey zoom.
const FRAMING_NOTCHES: usize = 40;

/// Where a showcase develops its take.
pub enum Exposure {
    Stills(PathBuf),
    Film(PathBuf),
}

impl Exposure {
    fn develop(self) -> Result<Showcase> {
        match self {
            Self::Stills(directory) => Showcase::stills(directory),
            Self::Film(directory) => Showcase::film(directory, &ArtifactStem::new(FILM)?),
        }
    }
}

/// The sealed specimen project, forged on first use through Trailgen's own
/// map-area acquisition in a testbed of its own. Acquisition queries public
/// Overpass, USGS, NYS OPRHP, Protomaps, and AWS terrain services whose
/// latency and rate limits lie outside any take, so takes replay the sealed
/// project rather than re-querying them. The directory is named by a digest
/// of everything that forged it, so any change forges afresh.
pub fn specimen(bed: TestbedBuilder, binary: &Path) -> Result<PathBuf> {
    let home = specimen_cache(PRODUCT)?.join("specimens");
    let sealed = home.join(format!("{PROJECT}-{}", recipe_digest()));
    fs::create_dir_all(&home).map_err(|source| io("create specimen home", &home, source))?;
    let Some((staging, _claim)) = stage(&home, &sealed)? else {
        return Ok(sealed);
    };
    bed.run(|testbed| forge(testbed, binary, staging.path()))?;
    match fs::rename(staging.path(), &sealed) {
        // Sealing moves the staged tree; its guard then has nothing to remove.
        Ok(()) => Ok(sealed),
        // A concurrent forge sealed the same recipe first; this copy dies
        // with its guard.
        Err(_) if sealed.is_dir() => Ok(sealed),
        Err(source) => Err(io("seal specimen", &sealed, source)),
    }
}

/// Under the specimen home's lock, sweep forges whose owners have died, then
/// stage and claim a fresh forge unless the specimen is already sealed. Every
/// forge holds an exclusive lock on its staging directory until it ends.
fn stage(home: &Path, sealed: &Path) -> Result<Option<(TempDir, File)>> {
    let guard = File::open(home).map_err(|source| io("open specimen home", home, source))?;
    guard
        .lock()
        .map_err(|source| io("lock specimen home", home, source))?;
    let entries = fs::read_dir(home).map_err(|source| io("read specimen home", home, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io("read specimen home entry", home, source))?;
        let path = entry.path();
        if !entry.file_name().to_string_lossy().starts_with(FORGING) {
            continue;
        }
        let forge = File::open(&path).map_err(|source| io("open forge", &path, source))?;
        match forge.try_lock() {
            Ok(()) => fs::remove_dir_all(&path)
                .map_err(|source| io("remove abandoned forge", &path, source))?,
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(source)) => return Err(io("probe forge", &path, source)),
        }
    }
    if sealed.is_dir() {
        return Ok(None);
    }
    let staging = tempfile::Builder::new()
        .prefix(FORGING)
        .tempdir_in(home)
        .map_err(|source| io("stage specimen", home, source))?;
    let claim =
        File::open(staging.path()).map_err(|source| io("open forge", staging.path(), source))?;
    claim
        .lock()
        .map_err(|source| io("claim forge", staging.path(), source))?;
    Ok(Some((staging, claim)))
}

/// Digest of the specimen layout, its recipe, and every product format it
/// seals.
fn recipe_digest() -> String {
    let recipe = format!(
        "trailgen-specimen/{SPECIMEN_FORMAT}\n{PROJECT_NAME}\n{AREA:?}\n{UI_FINGERPRINT}\n{PROJECT_FORMATS:?}\n{GRAPH_FORMAT}\n{}\n",
        env!("CARGO_PKG_VERSION")
    );
    Sha256::digest(recipe)
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut digest, byte| {
            write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
            digest
        })
}

/// Perform the showcase against a private copy of the sealed specimen and
/// publish every artifact once the product has terminated.
pub fn run(
    testbed: &Testbed,
    binary: &Path,
    specimen: &Path,
    exposure: Exposure,
) -> Result<Vec<PathBuf>> {
    admit(testbed, specimen, Path::new(PROJECT))?;
    seed_roaming(testbed, false)?;
    let _preferences =
        testbed.write_private("xdg/config/trailgen/preferences.toml", PREFERENCES)?;
    let project = Testbed::guest_path(PROJECT);
    let app = testbed.launch(gui(binary).arg(&project))?;
    let mut story = bind(testbed, &app, functional_budget(), exposure.develop()?)?;
    perform(&mut story)?;
    let showcase = story.finish()?;
    app.terminate()?;
    showcase.publish()
}

/// Discover loops from one trailhead, refine the best over a summit and keep
/// it, compare its rivals, then reopen the kept loop from the Library.
fn perform<O: StoryObserver>(story: &mut TrailStory<'_, '_, O>) -> Result<()> {
    story.chapter("Discover")?;
    let opened = story.wait_within(
        Duration::from_mins(2),
        shows::workspace(Workspace::Trail) & shows::corpus(CorpusPhase::Idle) & shows::painted(),
    )?;
    story.hold(Duration::from_secs(2))?;
    let trailhead = map_pixel(&opened, TRAILHEAD)?;
    reveal_inspector_target(story, Target::TrailheadPlacement)?;
    let _armed = story.click(Target::TrailheadPlacement)?.next_frame()?;
    let _placed = story
        .click_at(trailhead, Button::Primary)?
        .until(shows::trailhead())?;
    reveal_inspector_target(story, Target::DistanceMax)?;
    let _ranged = story
        .replace_text(Target::DistanceMax, DISTANCE_MAX_KM, shows::text_focused())?
        .next_frame()?;
    let _striking = story
        .key(Key::Return)?
        .until(shows::search(SearchPhase::Running))?;
    let found = story.wait_within(
        Duration::from_mins(3),
        shows::candidates_at_least(4) & shows::search(SearchPhase::Idle) & shows::painted(),
    )?;
    story.hold(Duration::from_secs(1))?;
    let trailhead = map_pixel(&found, TRAILHEAD)?;
    let scale = map_state(&found)?.world_points;
    let _zoomed = story
        .wheel(trailhead, -1, Wheel::default())?
        .until(shows::map_scale_at_least(scale * 1.2) & shows::painted())?;
    expose(story, &ArtifactStem::FRONTISPIECE)?;
    story.hold(Duration::from_secs(2))?;

    story.chapter("Refine")?;
    let cards = cards(&story.frame()?)?;
    focus_candidate(story, &cards[0])?;
    let editing = story
        .key(Key::Character('e'))?
        .until(shows::view(View::Edit) & shows::editor_ready())?
        .into_value();
    let _painted = story.wait(shows::painted())?;
    story.hold(Duration::from_secs(1))?;
    pull_support(story, &editing, SUMMIT)?;
    let _painted = story.wait(shows::painted())?;
    story.hold(Duration::from_secs(2))?;
    let _kept = story
        .click(Target::EditorSave)?
        .until(shows::view(View::FocusSaved) & shows::library(1))?;
    story.hold(Duration::from_secs(1))?;

    story.chapter("Compare")?;
    for card in &cards[1..4] {
        let _browsing = story.key(Key::Escape)?.until(shows::view(View::Browse))?;
        focus_candidate(story, card)?;
    }
    let browsing = story
        .key(Key::Escape)?
        .until(shows::view(View::Browse))?
        .into_value();
    let kept = first_anchor(
        &browsing,
        TargetClass::LibraryTrail,
        "the Library lost the kept loop",
    )?;
    let reopened = story
        .click_anchor(&kept)?
        .until(shows::view(View::FocusSaved) & shows::profile_visible() & shows::painted())?
        .into_value();
    // Step back from the fitted loop so its north-east leg clears the legend
    // (jira bug/focus-fit-ignores-legend).
    let center = story.anchor(Target::Map)?.center();
    let scale = map_state(&reopened)?.world_points;
    let _stepped = story
        .wheel(center, 1, Wheel::default())?
        .until(shows::map_scale_at_most(scale / 1.2) & shows::painted())?;
    expose(story, &ArtifactStem::new("kept")?)?;
    story.hold(Duration::from_secs(3))
}

/// Open one candidate card and dwell on its loop and elevation profile.
fn focus_candidate<O: StoryObserver>(
    story: &mut TrailStory<'_, '_, O>,
    card: &Anchor,
) -> Result<()> {
    let _focused = story
        .click_anchor(card)?
        .until(shows::view(View::FocusCandidate) & shows::profile_visible())?;
    let _painted = story.wait(shows::painted())?;
    story.hold(Duration::from_secs(2))
}

/// Candidate cards on the results shelf, left to right.
fn cards(frame: &TrailFrame) -> Result<Vec<Anchor>> {
    let mut cards = frame
        .anchors
        .iter()
        .filter(|anchor| {
            anchor
                .name
                .as_str()
                .starts_with(TargetClass::Candidate.prefix())
        })
        .cloned()
        .collect::<Vec<_>>();
    cards.sort_by(|left, right| left.rect[0].total_cmp(&right.rect[0]));
    demand(
        cards.len() >= 4,
        "search shelved fewer than four candidate cards",
    )?;
    Ok(cards)
}

/// Drag the editor support nearest `summit` onto it, sparing the trailhead,
/// and await the reforged route.
fn pull_support<O: StoryObserver>(
    story: &mut TrailStory<'_, '_, O>,
    editing: &TrailFrame,
    summit: [f64; 2],
) -> Result<()> {
    let editor = editing
        .state
        .editor
        .as_ref()
        .ok_or_else(|| verdict("editor witness omitted editor state"))?;
    let signature = editor
        .route_signature
        .ok_or_else(|| verdict("ready editor omitted its route signature"))?;
    let distance = |[lon, lat]: [f64; 2]| (lon - summit[0]).hypot(lat - summit[1]);
    let (slot, support) = editor
        .support_points
        .iter()
        .copied()
        .enumerate()
        .skip(1)
        .min_by(|(_, left), (_, right)| distance(*left).total_cmp(&distance(*right)))
        .ok_or_else(|| verdict("editor offered no movable support"))?;
    let grip = editing
        .anchor(&Target::Support(slot).to_string())
        .ok_or_else(|| verdict(format!("editor omitted draggable support {slot}")))?
        .center();
    let (from, to) = (map_pixel(editing, support)?, map_pixel(editing, summit)?);
    let destination = (
        to.0.saturating_add(grip.0.saturating_sub(from.0)),
        to.1.saturating_add(grip.1.saturating_sub(from.1)),
    );
    let press = story
        .session()
        .button_down(grip.0, grip.1, Button::Primary)?;
    let _seized = story
        .reaction(press)
        .until(shows::dragging_support(Some(slot)))?;
    let _carried = story
        .motion_to(
            destination,
            Motion {
                steps: 30,
                duration: Duration::from_millis(900),
            },
        )?
        .next_frame()?;
    let release = story.session().button_up(Button::Primary)?;
    let _reforged = story.reaction(release).until(
        shows::dragging_support(None) & shows::editor_ready() & shows::changed_signature(signature),
    )?;
    Ok(())
}

/// Withdraw the pointer, await a wholly painted map, and name the still.
fn expose<O: StoryObserver>(story: &mut TrailStory<'_, '_, O>, name: &ArtifactStem) -> Result<()> {
    let _left = story.leave()?;
    let _painted = story.wait(shows::painted())?;
    story.still(name)
}

/// Create the specimen project, frame the parks, draw their map area, and
/// await the installed corpus and its sealed basemap archive; then harvest
/// the project tree.
fn forge(testbed: &Testbed, binary: &Path, staging: &Path) -> Result<()> {
    seed_roaming(testbed, true)?;
    let app = testbed.launch(gui(binary).runtime(Duration::from_mins(40)))?;
    let mut story = bind(testbed, &app, functional_budget(), Silent)?;
    create_project(&mut story, PROJECT_NAME, "/test")?;
    let frame = frame_area(&mut story)?;
    let northwest = map_pixel(&frame, [AREA[0], AREA[3]])?;
    let southeast = map_pixel(&frame, [AREA[2], AREA[1]])?;
    let _armed = story
        .click(Target::SurveyAddArea)?
        .until(shows::survey_drawing())?;
    let press = story
        .session()
        .button_down(northwest.0, northwest.1, Button::Primary)?;
    let _pressed = story.reaction(press).next_frame()?;
    let _previewed = story
        .motion_to(southeast, Motion::default())?
        .next_frame()?;
    let release = story.session().button_up(Button::Primary)?;
    let _acquiring = story.reaction(release).until(shows::survey_acquiring(1))?;
    let _installed = story.wait_within(
        Duration::from_mins(30),
        shows::workspace(Workspace::Trail)
            & shows::corpus(CorpusPhase::Idle)
            & shows::painted()
            & shows::archived(),
    )?;
    drop(story);
    app.terminate()?;
    harvest(testbed, Path::new(PROJECT), staging)
}

/// Pan the parks to the survey map's centre, then zoom until they fill most
/// of it, answering the painted frame that frames them.
fn frame_area(story: &mut TrailStory<'_, '_>) -> Result<TrailFrame> {
    let surveying = story.wait_within(
        SURVEY_PATIENCE,
        shows::workspace(Workspace::Survey) & shows::painted(),
    )?;
    let center = story.anchor(Target::SurveyMap)?.center();
    let grip = map_pixel(
        &surveying,
        [
            f64::midpoint(AREA[0], AREA[2]),
            f64::midpoint(AREA[1], AREA[3]),
        ],
    )?;
    let unpanned = map_state(&surveying)?.center.map(f64::to_bits);
    let press = story
        .session()
        .button_down(grip.0, grip.1, Button::Primary)?;
    let _gripped = story.reaction(press).next_frame()?;
    let _carried = story.motion_to(center, Motion::default())?.next_frame()?;
    let release = story.session().button_up(Button::Primary)?;
    let mut frame = story
        .reaction(release)
        .within(ReactionBudget::functional(SURVEY_PATIENCE))
        .until(
            shows::condition(
                "the survey camera to leave its first centre",
                move |state| {
                    state
                        .map
                        .as_ref()
                        .is_some_and(|map| map.center.map(f64::to_bits) != unpanned)
                },
            ) & shows::painted(),
        )?
        .into_value();
    for _ in 0..FRAMING_NOTCHES {
        if area_scale(&frame)? >= 0.55 {
            return Ok(frame);
        }
        let scale = map_state(&frame)?.world_points;
        frame = story
            .wheel(center, -1, Wheel::default())?
            .within(ReactionBudget::functional(SURVEY_PATIENCE))
            .until(shows::map_scale_at_least(scale * 1.2) & shows::painted())?
            .into_value();
    }
    Err(verdict("the survey map never framed the parks"))
}

/// The fraction of the map's limiting axis the area spans.
fn area_scale(frame: &TrailFrame) -> Result<f64> {
    let map = map_state(frame)?;
    let [west, north] = world_from_coord([AREA[0], AREA[3]]);
    let [east, south] = world_from_coord([AREA[2], AREA[1]]);
    let [x0, y0, x1, y1] = map.rect.map(f64::from);
    Ok(((east - west) * map.world_points / (x1 - x0))
        .max((south - north) * map.world_points / (y1 - y0)))
}

fn map_state(frame: &TrailFrame) -> Result<&MapState> {
    frame
        .state
        .map
        .as_ref()
        .ok_or_else(|| verdict("witness omitted the map transform"))
}

fn gui(binary: &Path) -> AppCommand {
    AppCommand::new(binary)
        .arg("gui")
        .graphics(Graphics::Host)
        .network(Network::Host)
        .witness(WITNESS)
        .runtime(Duration::from_mins(10))
}

/// Copy the operator's disposable roaming basemap tiles around the parks into
/// the private cache, with the continental bootstrap archive when surveying;
/// the product fetches whatever the operator's cache lacks.
fn seed_roaming(testbed: &Testbed, bootstrap: bool) -> Result<()> {
    // The specimen cache's XDG cache home also holds the operator's own.
    let source = specimen_cache(PRODUCT)?.with_file_name(ROAMING);
    let destination = Path::new("xdg/cache").join(ROAMING);
    if bootstrap && source.join(ROAMING_BOOTSTRAP).is_file() {
        let _bootstrap = testbed.copy_private(
            destination.join(ROAMING_BOOTSTRAP),
            source.join(ROAMING_BOOTSTRAP),
        )?;
    }
    let [west, south, east, north] = AREA;
    let (span_x, span_y) = (east - west, north - south);
    let [x0, y0] = world_from_coord([west - span_x, north + span_y]);
    let [x1, y1] = world_from_coord([east + span_x, south - span_y]);
    for zoom in 0..=ROAMING_ZOOM {
        let tiles = f64::from(1_u32 << zoom);
        let cell = |world: f64| {
            (world * tiles)
                .floor()
                .to_u32()
                .ok_or_else(|| verdict("roaming tile escaped the world"))
        };
        for x in cell(x0)?..=cell(x1)? {
            for y in cell(y0)?..=cell(y1)? {
                let tile = PathBuf::from(zoom.to_string())
                    .join(x.to_string())
                    .join(format!("{y}.mvt"));
                if source.join(&tile).is_file() {
                    let _tile =
                        testbed.copy_private(destination.join(&tile), source.join(&tile))?;
                }
            }
        }
    }
    Ok(())
}

/// Export the product's private project tree, refusing anything but
/// directories and regular files.
fn harvest(testbed: &Testbed, relative: &Path, destination: &Path) -> Result<()> {
    let host = testbed.private_path(relative)?;
    fs::create_dir_all(destination)
        .map_err(|source| io("create specimen directory", destination, source))?;
    let entries = fs::read_dir(&host)
        .map_err(|source| io("read private project directory", &host, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io("read private project entry", &host, source))?;
        let kind = entry
            .file_type()
            .map_err(|source| io("inspect private project entry", entry.path(), source))?;
        let name = entry.file_name();
        if kind.is_dir() {
            harvest(testbed, &relative.join(&name), &destination.join(&name))?;
        } else if kind.is_file() {
            testbed.export(relative.join(&name), destination.join(&name))?;
        } else {
            return Err(verdict(format!(
                "specimen project holds a non-regular entry {}",
                relative.join(&name).display()
            )));
        }
    }
    Ok(())
}

/// Copy the sealed specimen tree into the private root at `relative`.
fn admit(testbed: &Testbed, source: &Path, relative: &Path) -> Result<()> {
    let entries =
        fs::read_dir(source).map_err(|source_error| io("read specimen", source, source_error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io("read specimen entry", source, error))?;
        let path = entry.path();
        let relative = relative.join(entry.file_name());
        if path.is_dir() {
            admit(testbed, &path, &relative)?;
        } else {
            let _admitted = testbed.copy_private(relative, &path)?;
        }
    }
    Ok(())
}

fn io(operation: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        operation,
        path: path.into(),
        source,
    }
}
