//! Trailgen's showcase: one presentation take discovering, refining, and
//! comparing long-day loops across Harriman and Bear Mountain State Parks.
//! It performs ordinary semantic acts against a real public specimen and is
//! never a verdict.

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use egui_demo::Showcase;
use egui_tester::{
    Anchor, AppCommand, Button, Error, Graphics, Key, Motion, Network, Result, Silent,
    StoryObserver, Testbed, TestbedBuilder, Wheel, demand,
};
use num_traits::ToPrimitive as _;

use crate::{
    harness::{
        Target, TargetClass, TrailFrame, TrailStory, WITNESS, bind, first_anchor,
        functional_budget, map_pixel, verdict, world_from_coord,
    },
    interactions::{create_project, reveal_inspector_target},
    observation::{CorpusPhase, SearchPhase, View, Workspace, shows},
};

const FILM: &str = "trailgen-showcase";
const PROJECT_NAME: &str = "Harriman + Bear Mountain";
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
            Self::Film(directory) => Showcase::film(directory, FILM),
        }
    }
}

/// The sealed specimen project, forged on first use through Trailgen's own
/// map-area acquisition in a testbed of its own. Acquisition queries public
/// Overpass, USGS, NYS OPRHP, Protomaps, and AWS terrain services whose
/// latency and rate limits lie outside any take, so takes replay the sealed
/// project rather than re-querying them. Delete the directory to reforge.
pub fn specimen(bed: TestbedBuilder, binary: &Path) -> Result<PathBuf> {
    let sealed = cache_home()?
        .join("trailgen-acceptance/specimens")
        .join(PROJECT);
    if sealed.is_dir() {
        return Ok(sealed);
    }
    let parent = sealed
        .parent()
        .expect("a specimen path is nested beneath the cache home");
    fs::create_dir_all(parent).map_err(|source| io("create specimen home", parent, source))?;
    let staging = tempfile::Builder::new()
        .prefix(".forging-")
        .tempdir_in(parent)
        .map_err(|source| io("stage specimen", parent, source))?;
    bed.run(|testbed| forge(testbed, binary, staging.path()))?;
    // Sealing moves the staged tree; the guard then has nothing left to remove.
    fs::rename(staging.path(), &sealed).map_err(|source| io("seal specimen", &sealed, source))?;
    Ok(sealed)
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
    let _zoomed = story.wheel(trailhead, -1, Wheel::default())?.next_frame()?;
    let _settled = settle(story)?;
    expose(story, "frontispiece")?;
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
    let _reopened = story
        .click_anchor(&kept)?
        .until(shows::view(View::FocusSaved) & shows::profile_visible())?;
    // Step back from the fitted loop so its north-east leg clears the legend
    // (jira bug/focus-fit-ignores-legend).
    let center = story.anchor(Target::Map)?.center();
    let _stepped = story.wheel(center, 1, Wheel::default())?.next_frame()?;
    let _settled = settle(story)?;
    expose(story, "kept")?;
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
        .filter(|anchor| anchor.name.starts_with(TargetClass::Candidate.prefix()))
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
fn expose<O: StoryObserver>(story: &mut TrailStory<'_, '_, O>, name: &str) -> Result<()> {
    let _left = story.leave()?.next_frame()?;
    let _painted = story.wait(shows::painted())?;
    story.still(name)
}

/// Create the specimen project, frame the parks, draw their map area, and
/// await the installed corpus; then harvest the project tree.
fn forge(testbed: &Testbed, binary: &Path, staging: &Path) -> Result<()> {
    seed_roaming(testbed, true)?;
    let app = testbed.launch(gui(binary).runtime(Duration::from_mins(40)))?;
    let mut story = bind(testbed, &app, functional_budget(), Silent)?;
    create_project(&mut story, PROJECT_NAME, "/test")?;
    frame_area(&mut story)?;
    let frame = story.frame()?;
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
        shows::workspace(Workspace::Trail) & shows::corpus(CorpusPhase::Idle) & shows::painted(),
    )?;
    // The map paints from roaming tiles while the area's own basemap archive
    // is still forging; the project is whole only once that archive is sealed.
    let cache = testbed.private_path(Path::new(PROJECT).join("cache"))?;
    app.wait_until(
        Duration::from_mins(10),
        "the area's basemap archive to be sealed",
        || {
            let names = fs::read_dir(&cache)
                .map_err(|source| io("read private project cache", &cache, source))?
                .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|source| io("read private project cache entry", &cache, source))?;
            Ok(names
                .iter()
                .any(|name| name.starts_with("basemap-") && name.ends_with(".pmtiles"))
                && !names.iter().any(|name| name.ends_with(".partial")))
        },
    )?;
    drop(story);
    app.terminate()?;
    harvest(testbed, Path::new(PROJECT), staging)
}

/// Pan the parks to the survey map's centre, then zoom until they fill most
/// of it.
fn frame_area(story: &mut TrailStory<'_, '_>) -> Result<()> {
    let frame = story.wait(shows::workspace(Workspace::Survey) & shows::painted())?;
    let center = story.anchor(Target::SurveyMap)?.center();
    let grip = map_pixel(
        &frame,
        [
            f64::midpoint(AREA[0], AREA[2]),
            f64::midpoint(AREA[1], AREA[3]),
        ],
    )?;
    let press = story
        .session()
        .button_down(grip.0, grip.1, Button::Primary)?;
    let _gripped = story.reaction(press).next_frame()?;
    let _panned = story.motion_to(center, Motion::default())?.next_frame()?;
    let release = story.session().button_up(Button::Primary)?;
    let _released = story.reaction(release).next_frame()?;
    for _ in 0..FRAMING_NOTCHES {
        if area_scale(&settle(story)?)? >= 0.55 {
            return Ok(());
        }
        let _zoomed = story.wheel(center, -1, Wheel::default())?.next_frame()?;
    }
    Err(verdict("the survey map never framed the parks"))
}

/// The fraction of the map's limiting axis the area spans.
fn area_scale(frame: &TrailFrame) -> Result<f64> {
    let map = frame
        .state
        .map
        .as_ref()
        .ok_or_else(|| verdict("survey omitted its map transform"))?;
    let [west, north] = world_from_coord([AREA[0], AREA[3]]);
    let [east, south] = world_from_coord([AREA[2], AREA[1]]);
    let [x0, y0, x1, y1] = map.rect.map(f64::from);
    Ok(((east - west) * map.world_points / (x1 - x0))
        .max((south - north) * map.world_points / (y1 - y0)))
}

fn settle<O: StoryObserver>(story: &mut TrailStory<'_, '_, O>) -> Result<TrailFrame> {
    story.wait_stable(
        Duration::from_secs(8),
        Duration::from_millis(160),
        "map viewport to settle",
        |frame| {
            frame.state.map.as_ref().map(|map| {
                [
                    map.center[0].to_bits(),
                    map.center[1].to_bits(),
                    map.world_points.to_bits(),
                ]
            })
        },
    )
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
    let source = cache_home()?.join(ROAMING);
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

fn cache_home() -> Result<PathBuf> {
    env::var_os("XDG_CACHE_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or_else(|| verdict("neither XDG_CACHE_HOME nor HOME names a cache home"))
}

fn io(operation: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        operation,
        path: path.into(),
        source,
    }
}
