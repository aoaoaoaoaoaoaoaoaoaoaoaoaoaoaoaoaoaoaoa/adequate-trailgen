use crate::{
    annotation,
    basemap::Source as BasemapSource,
    chrome,
    civic_area::{self, AddOutcome, CivicAreas, CivicKey, CivicRowState},
    commands::{self, Context as CommandContext, Edict},
    export::{ExportEvent, ExportForge, ExportJob, suggested_filename},
    gallery::{self, TrailSort},
    lexicon::{ExplainedText, Glosses},
    library::{Library, SavedTrail, SearchRecipe, TrailId, Trailhead, validate_trail_name},
    live_area::{self, RegionHandles, RegionScribe, ResizeEvent, ScribeEvent},
    map::{self, Atlas, SELECTED_TRAIL_COLOR, Viewport},
    portfolio::{self, CandidatePortfolio, CandidateWarmth},
    preferences::{BASE_PACE_SETTING, BasePace, Preferences},
    profile::ElevationProfile,
    project::{Project, SearchEvent, SearchForge, SearchHandle, SearchRequest},
    readout,
    relief::Relief,
    search_boundary::{self, BoundaryEvent, BoundaryScribe},
    slate::{ManualDraft, Slate},
    trail_data::{
        Event as TrailDataEvent, Mutation as TrailDataMutation, TrailData, progress_status,
    },
    vector_field::VectorField,
};
use anyhow::{Context as _, Result};
use brass_poolrooms::water::{Domain, Frame as WaterFrame, Surface, Wetness};
use crossbeam_channel::{Receiver, Sender, bounded};
use egui::{Color32, RichText, Stroke, vec2};
use eternalist_apps::{
    Inspector, LivingWait, NativeWake, ScribeOutcome, SettledScribe,
    command_guide::{CommandGuide, GuideSection},
    commands::{CommandDispatch, CommandStatus},
    configuration::ConfigurationLedger,
    panel_navigation::{PanelFrame, PanelNavigator},
    responsiveness::{Drain, DrainBudget, SupersedingSender, superseding_channel},
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
#[cfg(feature = "egui-test")]
use trailgen_contract::BoundaryPhase;
use trailgen_contract::Target;
use trailgen_core::{
    Coord, EdgeDisposition, EdgeEdicts, EdgeIndex, LoopConstraints, RouteMetrics, RouteShape,
    RoutingLaw, SearchParams, SearchProgress, SearchStage, SolverKind, SupportPoint, Trail,
    TrailRealization, TrailStanding, TrailgenError, WalkGraph, WalkRealmIndex, WalkRouter,
};
use trailgen_data::SurveyRegion;

const PROFILE_HEIGHT: f32 = 178.0;
const RESULTS_HEIGHT: f32 = 190.0;
const TOOLBAR_HEIGHT: f32 = 44.0;
const STATE_SETTLE: Duration = Duration::from_millis(400);
const SEARCH_SETTLE: Duration = Duration::from_millis(350);
const EVENT_DRAIN: DrainBudget = DrainBudget::new(64, Duration::from_millis(3));
const PROJECTION_EVENT_CAPACITY: usize = 16;
const CANDIDATE_COUNT: usize = 12;
const TRAILHEAD_SNAP_M: f64 = 500.0;
const UNDO_DEPTH: usize = 128;
const SHAPES: [(RouteShape, &str); 3] = [
    (RouteShape::Loop, "LOOP"),
    (RouteShape::OutAndBack, "OUT + BACK"),
    (RouteShape::Open, "POINT TO POINT"),
];

pub struct TrailApp {
    root: PathBuf,
    name: String,
    sinew: Option<Sinew>,
    editor_serial: u64,
    defaults: LoopConstraints,
    params: SearchParams,
    solver: SolverKind,
    library: Library,
    state_scribe: SettledScribe<DurableState>,
    pending_state: Option<(u64, DurableState)>,
    dirty_state: DirtyState,
    saved_projections: BTreeMap<TrailId, Option<SavedProjection>>,
    projection_forge: ProjectionForge,
    export_forge: ExportForge,
    last_exported: Option<TrailId>,
    hovered_saved: Option<TrailId>,
    latched_saved: Vec<TrailId>,
    rename: Option<RenameDraft>,
    candidates: Option<CandidatePortfolio>,
    results_open: bool,
    edicts: EdgeEdicts,
    edict_history: UndoLog<EdgeEdicts>,
    search_due: Option<Instant>,
    view: WorkbenchView,
    creator_mode: CreatorMode,
    delete_confirmation: Option<TrailId>,
    sort: TrailSort,
    trail_coloring: map::TrailColoring,
    viewport: Viewport,
    cartography: map::CartographicClock,
    scale_bar: map::ScaleBar,
    focus_frame: FocusFrame,
    fit: Fit,
    serial: u64,
    forge_phase: ForgePhase,
    placing_trailhead: bool,
    trailhead_drag: Option<TrailheadDrag>,
    vector: Option<VectorField>,
    relief: Relief,
    regions: Vec<SurveyRegion>,
    region_names: BTreeMap<String, String>,
    civic: CivicAreas,
    area_rename: Option<AreaRenameDraft>,
    corpus: Option<CorpusTask>,
    scribe: RegionScribe,
    area_handles: RegionHandles,
    guide: CommandGuide,
    panels: PanelNavigator,
    boundary_scribe: BoundaryScribe,
    offline: bool,
    shutters: BTreeMap<String, bool>,
    inspector_scroll: f32,
    observed_slate: Slate,
    base_pace: BasePace,
    pending_base_pace: Option<f64>,
    water: Surface,
    living_wait: LivingWait,
    status: String,
    trail_data_status: Option<String>,
    profile_cursor: ProfileCursor,
    map_rect: egui::Rect,
    map_regime: MapRegime,
    workspace_signal: Option<Action>,
    post_armament: Option<TrailDataMutation>,
}

#[derive(Clone)]
struct DurableState {
    library: Option<Library>,
    slate: Option<Slate>,
}

#[derive(Clone, Copy, Default)]
struct DirtyState {
    library: bool,
    slate: bool,
}

impl DurableState {
    fn save(self, root: &Path, slate_path: &Path) -> Result<()> {
        let mut faults = Vec::new();
        if let Some(library) = self.library
            && let Err(error) = library.save(root)
        {
            faults.push(format!("trail library: {error:#}"));
        }
        if let Some(slate) = self.slate
            && let Err(error) = slate.save(slate_path)
        {
            faults.push(format!("window state: {error:#}"));
        }
        if faults.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(faults.join("; ")))
        }
    }
}

fn raise_state_scribe(
    ctx: &egui::Context,
    root: &Path,
    slate_path: PathBuf,
) -> Result<SettledScribe<DurableState>> {
    let root = root.to_owned();
    SettledScribe::spawn(
        "trailgen-state-scribe",
        ctx,
        STATE_SETTLE,
        move |state: DurableState| state.save(&root, &slate_path),
    )
}

struct TrailEditor {
    name: String,
    name_draft: Option<EditorNameDraft>,
    origin: EditorOrigin,
    return_to: EditorReturn,
    shape: RouteShape,
    support_points: Vec<SupportPoint>,
    coordinate_callouts: Vec<bool>,
    realization: Option<TrailRealization>,
    realizing: Option<u64>,
    shape_guard: Option<(u64, RouteShape)>,
    profile: Option<ElevationProfile>,
    fault: Option<EditorFault>,
    notice: Option<String>,
    history: UndoLog<TrailSketch>,
    drag: Option<PinDrag>,
}

#[derive(Clone)]
struct EditorFault {
    message: String,
    support_slot: Option<usize>,
}

struct EditorNameDraft {
    text: String,
    seize_focus: bool,
}

enum WorkbenchView {
    Browse,
    Focus(Focus),
    Edit(Box<TrailEditor>),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CreatorMode {
    #[default]
    Neutral,
    Finder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MapRegime {
    Browse,
    Focus,
    Edit,
}

impl WorkbenchView {
    const fn map_regime(&self) -> MapRegime {
        match self {
            Self::Browse => MapRegime::Browse,
            Self::Focus(_) => MapRegime::Focus,
            Self::Edit(_) => MapRegime::Edit,
        }
    }

    const fn editor(&self) -> Option<&TrailEditor> {
        match self {
            Self::Edit(editor) => Some(editor),
            Self::Browse | Self::Focus(_) => None,
        }
    }

    const fn editor_mut(&mut self) -> Option<&mut TrailEditor> {
        match self {
            Self::Edit(editor) => Some(editor),
            Self::Browse | Self::Focus(_) => None,
        }
    }

    const fn focus(&self) -> Option<&Focus> {
        match self {
            Self::Focus(focus) => Some(focus),
            Self::Browse | Self::Edit(_) => None,
        }
    }

    const fn is_editing(&self) -> bool {
        matches!(self, Self::Edit(_))
    }
}

struct EditorReturn {
    focus: Option<Focus>,
    viewport: Viewport,
}

#[derive(Clone, PartialEq)]
struct TrailSketch {
    shape: RouteShape,
    support_points: Vec<SupportPoint>,
}

struct UndoLog<T> {
    past: VecDeque<T>,
    future: VecDeque<T>,
}

impl<T> Default for UndoLog<T> {
    fn default() -> Self {
        Self {
            past: VecDeque::new(),
            future: VecDeque::new(),
        }
    }
}

impl<T> UndoLog<T> {
    fn push(stack: &mut VecDeque<T>, state: T) {
        if stack.len() == UNDO_DEPTH {
            let _oldest = stack.pop_front();
        }
        stack.push_back(state);
    }

    fn commit(&mut self, before: T) {
        Self::push(&mut self.past, before);
        self.future.clear();
    }

    fn undo(&mut self, current: T) -> Option<T> {
        let target = self.past.pop_back()?;
        Self::push(&mut self.future, current);
        Some(target)
    }

    fn redo(&mut self, current: T) -> Option<T> {
        let target = self.future.pop_back()?;
        Self::push(&mut self.past, current);
        Some(target)
    }

    fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    #[cfg(feature = "egui-test")]
    fn redo_depth(&self) -> usize {
        self.future.len()
    }

    fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
    }
}

struct RenameDraft {
    trail: TrailId,
    text: String,
    seize_focus: bool,
}

struct AreaRenameDraft {
    region: String,
    text: String,
    seize_focus: bool,
}

enum AreaRenameAction {
    Begin(String),
    Commit,
    Cancel,
}

enum AreaRowAction {
    Remove(egui::Rect),
    Rename,
}

struct SavedProjection {
    miniature: gallery::SavedPreview,
    profile: Option<ElevationProfile>,
    overlay: map::RouteOverlay,
}

#[derive(Clone, Copy, Default)]
struct SearchEdit {
    changed: bool,
    submitted: bool,
}

#[derive(Clone, Copy)]
enum MeasureKind {
    Distance,
    MovingTime,
    Climb,
}

impl MeasureKind {
    const fn id(self) -> &'static str {
        match self {
            Self::Distance => "distance",
            Self::MovingTime => "moving-time",
            Self::Climb => "climb",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Distance => "DISTANCE · KM",
            Self::MovingTime => "MOVING TIME · H",
            Self::Climb => "CLIMB · M",
        }
    }

    const fn glosses(self) -> Glosses {
        match self {
            Self::MovingTime => Glosses::MOVING_TIME,
            Self::Distance | Self::Climb => Glosses::NONE,
        }
    }
}

impl SearchEdit {
    const fn merge(self, other: Self) -> Self {
        Self {
            changed: self.changed || other.changed,
            submitted: self.submitted || other.submitted,
        }
    }
}

impl SavedProjection {
    fn forge(trail: &SavedTrail) -> Self {
        Self {
            miniature: gallery::SavedPreview::forge(trail),
            profile: ElevationProfile::forge_saved(trail),
            overlay: map::RouteOverlay::saved(trail),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProfileOwner {
    Candidate(usize),
    Saved(TrailId),
    Editor,
}

#[derive(Default)]
struct ProfileCursor {
    owner: Option<ProfileOwner>,
    locked_m: Option<f64>,
    marker: Option<Coord>,
}

impl ProfileCursor {
    fn bind(&mut self, owner: Option<ProfileOwner>) {
        if self.owner != owner {
            self.owner = owner;
            self.locked_m = None;
        }
    }

    fn resolve(&mut self, hovered_m: Option<f64>, lock: bool, release: bool) -> Option<f64> {
        if release {
            self.locked_m = None;
        } else if lock && hovered_m.is_some() {
            self.locked_m = hovered_m;
        }
        self.locked_m.or(hovered_m)
    }
}

struct PinDrag {
    slot: usize,
    before: TrailSketch,
    grab: egui::Vec2,
}

struct TrailheadDrag {
    origin: Coord,
    preview: Coord,
    grab: egui::Vec2,
}

#[derive(Clone, Copy, Default)]
struct TrailheadGesture {
    captured: bool,
    stopped: bool,
}

#[derive(Clone, Copy)]
struct MapGesture {
    rect: egui::Rect,
    pointer: Option<egui::Pos2>,
    support_under_pointer: Option<usize>,
    trailhead: TrailheadGesture,
    click_modifiers: Option<egui::Modifiers>,
}

impl TrailEditor {
    fn forge(
        name: String,
        origin: EditorOrigin,
        return_to: EditorReturn,
        shape: RouteShape,
        support_points: Vec<SupportPoint>,
    ) -> Self {
        Self {
            name,
            name_draft: None,
            origin,
            return_to,
            shape,
            coordinate_callouts: vec![false; support_points.len()],
            support_points,
            realization: None,
            realizing: None,
            shape_guard: None,
            profile: None,
            fault: None,
            notice: None,
            history: UndoLog::default(),
            drag: None,
        }
    }

    const fn ready(&self) -> bool {
        self.realizing.is_none() && self.fault.is_none() && self.realization.is_some()
    }

    fn sketch(&self) -> TrailSketch {
        TrailSketch {
            shape: self.shape,
            support_points: self.support_points.clone(),
        }
    }

    fn durable_sketch(&self) -> TrailSketch {
        let mut sketch = self
            .drag
            .as_ref()
            .map_or_else(|| self.sketch(), |drag| drag.before.clone());
        if let Some((_, previous)) = self.shape_guard {
            sketch.shape = previous;
        }
        sketch
    }

    fn checkpoint(&mut self) {
        self.finish_drag();
        self.history.commit(self.sketch());
    }

    fn excise_support(&mut self, slot: usize) -> bool {
        if slot >= self.support_points.len() {
            return false;
        }
        self.checkpoint();
        self.support_points.remove(slot);
        self.coordinate_callouts.remove(slot);
        true
    }

    fn finish_drag(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        let changed = drag.before != self.sketch();
        if changed {
            self.history.commit(drag.before);
        }
        changed
    }

    fn undo(&mut self) -> bool {
        self.finish_drag();
        let current = self.sketch();
        let Some(target) = self.history.undo(current) else {
            return false;
        };
        self.restore(target);
        true
    }

    fn redo(&mut self) -> bool {
        self.finish_drag();
        let current = self.sketch();
        let Some(target) = self.history.redo(current) else {
            return false;
        };
        self.restore(target);
        true
    }

    fn restore(&mut self, target: TrailSketch) {
        self.shape = target.shape;
        self.replace_supports(target.support_points);
    }

    fn replace_supports(&mut self, support_points: Vec<SupportPoint>) {
        let mut callouts = vec![false; support_points.len()];
        let mut claimed = vec![false; self.support_points.len()];
        let mut unmatched_old = Vec::new();
        let mut unmatched_new = Vec::new();

        for (new_slot, support) in support_points.iter().enumerate() {
            if let Some(old_slot) = self
                .support_points
                .iter()
                .enumerate()
                .position(|(slot, old)| !claimed[slot] && old == support)
            {
                claimed[old_slot] = true;
                callouts[new_slot] = self.coordinate_callouts[old_slot];
            } else {
                unmatched_new.push(new_slot);
            }
        }
        unmatched_old.extend(
            claimed
                .iter()
                .enumerate()
                .filter_map(|(slot, claimed)| (!claimed).then_some(slot)),
        );
        if unmatched_old.len() == unmatched_new.len() {
            for (old_slot, new_slot) in unmatched_old.into_iter().zip(unmatched_new) {
                callouts[new_slot] = self.coordinate_callouts[old_slot];
            }
        }

        self.support_points = support_points;
        self.coordinate_callouts = callouts;
    }

    fn absorb_realization(&mut self, result: trailgen_core::Result<TrailRealization>) {
        self.realizing = None;
        match result {
            Ok(realization) => {
                self.profile = ElevationProfile::forge(realization.graph(), &realization.route);
                self.realization = Some(realization);
                self.fault = None;
                self.notice = None;
            }
            Err(err) => {
                self.fault = Some(editor_fault(&err, self.support_points.len()));
                self.notice = None;
            }
        }
    }

    fn reject_loop_closure(&mut self, error: &TrailgenError) -> String {
        let notice = editor_fault(error, self.support_points.len()).message;
        self.notice = Some(notice.clone());
        notice
    }
}

#[derive(Clone)]
enum EditorOrigin {
    New,
    Candidate,
    Saved(TrailId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Focus {
    Candidate { identity: usize },
    Saved(TrailId),
}

enum FocusAction {
    Close(egui::Rect),
    Step(isize, egui::Rect),
    Save(egui::Rect),
    Edit(egui::Rect),
    RequestDelete,
    ConfirmDelete,
    CancelDelete,
}

enum RenameAction {
    Begin(TrailId, egui::Rect),
    Commit,
    Cancel,
}

enum EditorNameAction {
    Begin(egui::Rect),
    Commit,
    Cancel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FocusFrame {
    return_to: Option<Viewport>,
}

impl FocusFrame {
    fn push(&mut self, viewport: Viewport) {
        let _ = self.return_to.get_or_insert(viewport);
    }

    const fn pop(&mut self) -> Option<Viewport> {
        self.return_to.take()
    }

    fn base(self, viewport: Viewport) -> Viewport {
        self.return_to.unwrap_or(viewport)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum Fit {
    #[default]
    Graph,
    Regions,
    Candidate {
        identity: usize,
    },
    Saved(TrailId),
    Civic(CivicKey),
    None,
}

enum CivicRowAction {
    Fit(CivicKey, egui::Rect),
    Retry(usize, egui::Rect),
    Remove(usize, egui::Rect),
}

#[derive(Debug, Default)]
enum ForgePhase {
    #[default]
    Idle,
    Striking {
        serial: u64,
        handle: SearchHandle,
        progress: SearchProgress,
        stopping: bool,
    },
}

impl ForgePhase {
    const fn active(&self) -> bool {
        matches!(self, Self::Striking { .. })
    }

    const fn serial(&self) -> Option<u64> {
        match self {
            Self::Idle => None,
            Self::Striking { serial, .. } => Some(*serial),
        }
    }

    const fn progress(&self) -> Option<SearchProgress> {
        match self {
            Self::Idle => None,
            Self::Striking { progress, .. } => Some(*progress),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Projects,
    Reload,
}

struct Sinew {
    graph: Arc<WalkGraph>,
    edge_index: Arc<EdgeIndex>,
    finder_index: Arc<WalkRealmIndex>,
    forge: SearchForge,
    editor_forge: EditorForge,
    atlas: Atlas,
}

enum CorpusTask {
    Acquiring(TrailData),
    Preparing(CorpusForge),
}

struct CorpusForge {
    event: Receiver<Result<CorpusArmament>>,
    _thread: thread::JoinHandle<()>,
}

struct EditorJob {
    serial: u64,
    name: String,
    shape: RouteShape,
    support_points: Vec<SupportPoint>,
    routing: RoutingLaw,
    constraints: LoopConstraints,
}

struct EditorEvent {
    serial: u64,
    result: trailgen_core::Result<TrailRealization>,
}

struct EditorForge {
    command: SupersedingSender<EditorJob>,
    events: Receiver<EditorEvent>,
    _thread: thread::JoinHandle<()>,
}

struct ProjectionForge {
    command: Sender<(TrailId, SavedTrail)>,
    events: Receiver<(TrailId, SavedProjection)>,
    _thread: thread::JoinHandle<()>,
}

impl EditorForge {
    fn spawn(ctx: &egui::Context, graph: Arc<WalkGraph>) -> Result<Self> {
        let (command, jobs) = superseding_channel::<EditorJob>();
        let (events_tx, events) = bounded(1);
        let wake = NativeWake::from_context(ctx);
        let worker = thread::Builder::new()
            .name("trail-realizer".to_owned())
            .spawn(move || {
                let index = EdgeIndex::forge(&graph);
                let router = WalkRouter::forge(&graph);
                while let Ok(job) = jobs.recv() {
                    let result = Trail::forge(job.shape, job.support_points, job.routing).and_then(
                        |trail| {
                            trail.realize_indexed(
                                job.name,
                                &graph,
                                &index,
                                &router,
                                &job.constraints,
                                TRAILHEAD_SNAP_M,
                            )
                        },
                    );
                    if events_tx
                        .send(EditorEvent {
                            serial: job.serial,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    let _woken = wake.request_foreground_repaint();
                }
            })
            .context("spawn trail realizer")?;
        Ok(Self {
            command,
            events,
            _thread: worker,
        })
    }

    fn strike(&self, job: EditorJob) -> Result<()> {
        self.command
            .offer(job)
            .map(|_superseded| ())
            .map_err(|_| anyhow::anyhow!("trail realizer stopped"))
    }
}

impl ProjectionForge {
    fn spawn(ctx: &egui::Context) -> Result<Self> {
        let (command, jobs) = bounded::<(TrailId, SavedTrail)>(PROJECTION_EVENT_CAPACITY);
        let (publish, events) = bounded(PROJECTION_EVENT_CAPACITY);
        let wake = NativeWake::from_context(ctx);
        let thread = thread::Builder::new()
            .name("saved-trail-projector".to_owned())
            .spawn(move || {
                while let Ok((id, trail)) = jobs.recv() {
                    if publish.send((id, SavedProjection::forge(&trail))).is_err() {
                        break;
                    }
                    let _woken = wake.request_foreground_repaint();
                }
            })
            .context("spawn saved-trail projector")?;
        Ok(Self {
            command,
            events,
            _thread: thread,
        })
    }

    fn strike(&self, id: TrailId, trail: SavedTrail) -> bool {
        self.command.try_send((id, trail)).is_ok()
    }
}

struct CorpusArmament {
    sinew: Sinew,
    source: BasemapSource,
    regions: Vec<SurveyRegion>,
    region_names: BTreeMap<String, String>,
    legacy_routes: Vec<SavedTrail>,
}

impl CorpusForge {
    fn spawn(ctx: &egui::Context, root: PathBuf, read_legacy_routes: bool) -> Result<Self> {
        let (event_tx, event) = bounded(1);
        let worker_ctx = ctx.clone();
        let wake = NativeWake::from_context(ctx);
        let thread = thread::Builder::new()
            .name("trail-corpus-forge".to_owned())
            .spawn(move || {
                let _forge = tracing::info_span!(
                    target: "eternalist::startup",
                    "corpus.forge",
                    root = %root.display()
                )
                .entered();
                let result = (|| {
                    #[cfg(feature = "egui-test")]
                    if let Some(delay) = std::env::var("TRAILGEN_STALL_ARMAMENT_MS")
                        .ok()
                        .and_then(|raw| raw.parse::<u64>().ok())
                    {
                        thread::sleep(Duration::from_millis(delay));
                    }
                    let graph = product_phase!("corpus.load", Project::load_graph(&root)?);
                    let legacy_routes = if read_legacy_routes {
                        product_phase!(
                            "corpus.legacy_routes",
                            Library::read_legacy_routes(&root, &graph)?
                        )
                    } else {
                        Vec::new()
                    };
                    let edge_index = Arc::new(product_phase!(
                        "corpus.edge_index",
                        EdgeIndex::forge(&graph)
                    ));
                    let finder_index = Arc::new(product_phase!(
                        "corpus.finder_index",
                        WalkRealmIndex::finder(&graph)
                    ));
                    let atlas = product_phase!("corpus.atlas", Atlas::forge(&graph));
                    let forge = product_phase!(
                        "corpus.search_worker",
                        SearchForge::spawn(
                            &worker_ctx,
                            Arc::clone(&graph),
                            Arc::clone(&finder_index),
                        )?
                    );
                    let editor_forge = product_phase!(
                        "corpus.editor_worker",
                        EditorForge::spawn(&worker_ctx, Arc::clone(&graph))?
                    );
                    let config =
                        product_phase!("corpus.data_config", trailgen_data::project_config(&root)?);
                    let bounds = config
                        .regions
                        .iter()
                        .map(|region| region.bounds)
                        .collect::<Vec<_>>();
                    let source = product_phase!(
                        "corpus.basemap_source",
                        BasemapSource::project(&root, &graph, &bounds)?
                    );
                    Ok(CorpusArmament {
                        sinew: Sinew {
                            graph,
                            edge_index,
                            finder_index,
                            forge,
                            editor_forge,
                            atlas,
                        },
                        source,
                        regions: config.regions,
                        region_names: config.region_names,
                        legacy_routes,
                    })
                })();
                let _sent = event_tx.send(result);
                let _woken = wake.request_foreground_repaint();
            })
            .context("spawn trail-corpus projection forge")?;
        Ok(Self {
            event,
            _thread: thread,
        })
    }
}

pub struct ReloadFrame {
    focus: Option<TrailId>,
    viewport: Viewport,
    browse_viewport: Option<Viewport>,
    latched_saved: Vec<TrailId>,
}

impl ReloadFrame {
    pub const fn browse(viewport: Viewport) -> Self {
        Self {
            focus: None,
            viewport,
            browse_viewport: None,
            latched_saved: Vec::new(),
        }
    }

    pub const fn viewport(&self) -> Viewport {
        self.viewport
    }
}

fn finder_counsel(library: &Library) -> String {
    if library.search().trailhead.is_some() {
        "Choose Find trails to search from this trailhead."
    } else {
        "Place a trailhead on the map, then find trails."
    }
    .to_owned()
}

fn raise_region_vector(
    ctx: &egui::Context,
    root: &Path,
    regions: &[SurveyRegion],
    offline: bool,
) -> Result<Option<VectorField>> {
    let bounds = regions
        .iter()
        .map(|region| region.bounds)
        .collect::<Vec<_>>();
    if bounds.is_empty() {
        return Ok(None);
    }
    let source = BasemapSource::regions(root, &bounds)?;
    product_phase!(
        "project.vector_field",
        VectorField::raise(ctx, source, offline, None).map(Some)
    )
}

fn resurrect_workbench(
    slate: &Slate,
    regions_empty: bool,
) -> (Viewport, WorkbenchView, Fit, String) {
    let browse_viewport = slate.viewport;
    if let Some(draft) = slate.manual_draft.clone() {
        let viewport = draft.viewport;
        let editor = TrailEditor::forge(
            draft.name,
            EditorOrigin::New,
            EditorReturn {
                focus: None,
                viewport: browse_viewport.unwrap_or(viewport),
            },
            draft.shape,
            draft.support_points,
        );
        return (
            viewport,
            WorkbenchView::Edit(Box::new(editor)),
            Fit::None,
            "Restored an unfinished manual trail; preparing its route…".to_owned(),
        );
    }
    let viewport = browse_viewport.unwrap_or(Viewport::WORLD);
    let fit = match (browse_viewport, regions_empty) {
        (Some(_), _) => Fit::None,
        (None, false) => Fit::Regions,
        (None, true) => Fit::Graph,
    };
    (
        viewport,
        WorkbenchView::Browse,
        fit,
        "Preparing trail network…".to_owned(),
    )
}

impl TrailApp {
    pub(crate) fn raise(
        ctx: &egui::Context,
        root: &Path,
        offline: bool,
        slate_path: PathBuf,
        trail_data: trailgen_data::TrailDataConfig,
        indexed: Option<&trailgen_data::Summary>,
    ) -> Result<Self> {
        let _open = tracing::info_span!(
            target: "eternalist::startup",
            "trailgen.raise",
            root = %root.display()
        )
        .entered();
        let project = product_phase!("project.open", Project::open(root)?);
        let slate = product_phase!("project.slate", Slate::load(&slate_path, &project.root));
        let refresh = !offline && !trail_data.regions.is_empty() && indexed.is_none();
        let vector = raise_region_vector(ctx, &project.root, &trail_data.regions, offline)?;
        let relief = product_phase!("project.relief", Relief::raise(ctx, &project.root)?);
        let civic = product_phase!(
            "project.civic_areas",
            CivicAreas::raise(ctx, &project.root, offline)?
        );
        let projection_forge = ProjectionForge::spawn(ctx)?;
        let export_forge = ExportForge::spawn(ctx)?;
        let legacy_routes_pending = project.library.legacy_routes_pending();
        let armament = CorpusForge::spawn(ctx, project.root.clone(), legacy_routes_pending)?;
        let Project {
            root,
            config,
            library,
        } = project;
        let (viewport, view, fit, status) =
            resurrect_workbench(&slate, trail_data.regions.is_empty());
        let cartography = map::CartographicClock::new(viewport);
        let state_scribe = raise_state_scribe(ctx, &root, slate_path)?;
        let mut app = Self {
            root,
            name: config.name,
            sinew: None,
            editor_serial: 0,
            defaults: config.constraints,
            params: config.search,
            solver: config.solver,
            library,
            state_scribe,
            pending_state: None,
            dirty_state: DirtyState::default(),
            saved_projections: BTreeMap::new(),
            projection_forge,
            export_forge,
            last_exported: None,
            hovered_saved: None,
            latched_saved: Vec::new(),
            rename: None,
            candidates: None,
            results_open: false,
            edicts: EdgeEdicts::default(),
            edict_history: UndoLog::default(),
            search_due: None,
            view,
            creator_mode: CreatorMode::Neutral,
            delete_confirmation: None,
            sort: slate.sort,
            trail_coloring: slate.trail_coloring,
            viewport,
            cartography,
            scale_bar: map::ScaleBar::default(),
            focus_frame: FocusFrame::default(),
            fit,
            serial: 0,
            forge_phase: ForgePhase::Idle,
            placing_trailhead: false,
            trailhead_drag: None,
            vector,
            relief,
            regions: trail_data.regions,
            region_names: trail_data.region_names,
            civic,
            area_rename: None,
            corpus: Some(CorpusTask::Preparing(armament)),
            scribe: RegionScribe::default(),
            area_handles: RegionHandles::default(),
            guide: CommandGuide::default(),
            panels: PanelNavigator::default(),
            boundary_scribe: BoundaryScribe::default(),
            offline,
            shutters: slate.shutters.clone(),
            inspector_scroll: slate.inspector_scroll,
            observed_slate: slate,
            base_pace: BasePace::default(),
            pending_base_pace: None,
            water: forge_water(),
            living_wait: LivingWait::default(),
            status,
            trail_data_status: Some("Preparing trail network…".to_owned()),
            profile_cursor: ProfileCursor::default(),
            map_rect: egui::Rect::ZERO,
            map_regime: MapRegime::Browse,
            workspace_signal: None,
            post_armament: refresh.then_some(TrailDataMutation::Refresh),
        };
        app.settle_raise();
        Ok(app)
    }

    fn settle_raise(&mut self) {
        self.reconcile_saved_projections();
        self.observed_slate = self.snapshot();
    }

    pub fn pulse(
        &mut self,
        ui: &mut egui::Ui,
        configuration: &mut ConfigurationLedger<Preferences>,
    ) -> Option<Action> {
        self.set_base_pace(configuration.live().base_pace());
        let mut drain = EVENT_DRAIN.arm();
        self.absorb_persistence();
        product_phase!(
            "pulse.corpus_events",
            self.absorb_corpus(ui.ctx(), &mut drain)
        );
        product_phase!(
            "pulse.editor_events",
            self.absorb_editor_events(ui.ctx(), &mut drain)
        );
        product_phase!(
            "pulse.exports",
            self.absorb_export_events(ui.ctx(), &mut drain)
        );
        product_phase!(
            "pulse.saved_projections",
            self.absorb_saved_projections(ui.ctx(), &mut drain)
        );
        product_phase!(
            "pulse.deferred_trail_refresh",
            self.tend_post_armament(ui.ctx())
        );
        if let Some(alarm) =
            product_phase!("pulse.civic_events", self.civic.pulse(ui.ctx(), &mut drain))
        {
            self.status = alarm;
        }
        product_phase!(
            "pulse.search_events",
            self.absorb_events(ui.ctx(), &mut drain)
        );
        product_phase!("pulse.input", {
            let guide_invoked = self.guide.take_shortcuts(ui.ctx());
            if !guide_invoked && !self.guide.is_open() {
                if let Some(dispatch) =
                    commands::canon().route(ui.ctx(), self.command_contexts(), |edict| {
                        self.edict_status(edict)
                    })
                {
                    self.apply_edict(dispatch);
                }
                self.take_keys(ui.ctx());
            }
        });
        let profile_owner = self.profile_owner();
        self.profile_cursor.bind(profile_owner);
        self.hovered_saved = None;
        self.profile_cursor.marker = None;
        let mut panels = std::mem::take(&mut self.panels);
        let inspector = product_phase!(
            "pulse.inspector",
            Inspector::new("trail-inspector")
                .scroll_id("trail-inspector-scroll")
                .scroll_offset(self.inspector_scroll)
                .show(ui, |ui| self.inspector(ui, &mut panels))
        );
        self.panels = panels;
        self.inspector_scroll = inspector.scroll_offset;
        if let Some(kmh) = self.pending_base_pace.take()
            && configuration
                .revise(|preferences| preferences.set_base_pace(kmh))
                .is_ok()
        {
            self.set_base_pace(configuration.live().base_pace());
        }
        // Moving-wall impulses starve presentation during concurrent search;
        // scroll displacement retains water response within the 40 ms cadence law.
        self.water.heave(ui.ctx(), inspector.scroll_offset);
        let _center = product_phase!(
            "pulse.arena",
            egui::CentralPanel::default().show(ui, |ui| self.arena(ui))
        );
        product_phase!("pulse.command_guide", self.command_guide(ui));
        self.observe_persistence();
        self.workspace_signal.take()
    }

    pub fn service_deadline(&self, _now: Instant) -> Option<Instant> {
        let search = (!self.forge_phase.active())
            .then_some(self.search_due)
            .flatten();
        self.state_scribe
            .deadline()
            .into_iter()
            .chain(self.civic.service_deadline())
            .chain(self.vector.as_ref().and_then(VectorField::service_deadline))
            .chain(search)
            .min()
    }

    pub fn service_deadline_reached(&mut self, now: Instant) -> bool {
        let mut changed = if self.search_due.is_some_and(|deadline| deadline <= now)
            && !self.forge_phase.active()
        {
            self.strike();
            true
        } else {
            false
        };
        if let Some(vector) = &mut self.vector {
            changed |= vector.service_deadline_reached(now);
        }
        changed |= self.civic.service_deadline_reached(now);
        if self
            .state_scribe
            .deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            let state = self.durable_state();
            let pending = state.clone();
            match self.state_scribe.tend(now, || state) {
                Ok(Some(sequence)) => self.accept_submission(sequence, pending),
                Ok(None) => {}
                Err(error) => {
                    self.status = format!("Could not submit persistent state: {error:#}");
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn set_base_pace(&mut self, base_pace: BasePace) {
        if self.base_pace != base_pace {
            self.base_pace = base_pace;
            self.schedule_revision();
        }
    }

    pub const fn water_mut(&mut self) -> &mut Surface {
        &mut self.water
    }

    pub(crate) fn help_activator(&mut self, ui: &mut egui::Ui) {
        let help = self.guide.activator(ui);
        crate::witness::response(ui, Target::Help, &help);
        self.water.monoglyph(&help);
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn window_title(&self) -> String {
        let trail = match &self.view {
            WorkbenchView::Focus(Focus::Candidate { identity }) => self
                .candidates
                .as_ref()
                .and_then(|run| run.slot(*identity).and_then(|slot| run.routes.get(slot)))
                .map(|route| route.name.as_str()),
            WorkbenchView::Focus(Focus::Saved(id)) => {
                self.library.trail(id).map(|trail| trail.name.as_str())
            }
            WorkbenchView::Edit(editor) => Some(editor.name.as_str()),
            WorkbenchView::Browse => None,
        };
        trail.map_or_else(
            || format!("{} · trailgen", self.name),
            |trail| format!("{trail} · {} · trailgen", self.name),
        )
    }

    pub fn reload_frame(&self) -> ReloadFrame {
        match self.view.focus() {
            Some(Focus::Saved(id)) => ReloadFrame {
                focus: Some(id.clone()),
                viewport: self.viewport,
                browse_viewport: self.focus_frame.return_to,
                latched_saved: self.latched_saved.clone(),
            },
            Some(Focus::Candidate { .. }) | None => ReloadFrame {
                focus: None,
                viewport: self.viewport,
                browse_viewport: None,
                latched_saved: self.latched_saved.clone(),
            },
        }
    }

    pub fn restore_reload_frame(&mut self, frame: ReloadFrame) {
        self.viewport = frame.viewport;
        self.fit = Fit::None;
        self.latched_saved = frame.latched_saved;
        self.reconcile_latched_saved();
        self.delete_confirmation = None;
        if let Some(id) = frame.focus.filter(|id| self.library.trail(id).is_some()) {
            self.view = WorkbenchView::Focus(Focus::Saved(id));
            self.focus_frame.return_to = frame.browse_viewport;
        } else {
            self.view = WorkbenchView::Browse;
            self.focus_frame = FocusFrame::default();
        }
    }

    #[cfg(feature = "egui-test")]
    pub(crate) fn witness_state(&self, text_edit_focused: bool) -> crate::witness::State {
        let view = match &self.view {
            WorkbenchView::Browse => trailgen_contract::View::Browse,
            WorkbenchView::Focus(Focus::Candidate { .. }) => {
                trailgen_contract::View::FocusCandidate
            }
            WorkbenchView::Focus(Focus::Saved(_)) => trailgen_contract::View::FocusSaved,
            WorkbenchView::Edit(_) => trailgen_contract::View::Edit,
        };
        crate::witness::State {
            contract: trailgen_contract::UI_FINGERPRINT,
            workspace: if self.sinew.is_some() {
                trailgen_contract::Workspace::Trail
            } else {
                trailgen_contract::Workspace::Preparing
            },
            view,
            rename_active: self.rename.is_some()
                || self.area_rename.is_some()
                || self
                    .view
                    .editor()
                    .is_some_and(|editor| editor.name_draft.is_some()),
            guide_open: self.guide.is_open(),
            text_edit_focused,
            saved_trails: self.library.trails().len(),
            visible_saved: self.latched_saved.len(),
            last_exported: self
                .last_exported
                .as_ref()
                .map(|identity| identity.as_str().to_owned()),
            candidates: self
                .candidates
                .as_ref()
                .map_or(0, |portfolio| portfolio.routes.len()),
            base_pace_kmh: Some(self.base_pace.kmh()),
            settings: crate::witness::Settings::default(),
            map: self.map_rect.is_positive().then(|| {
                crate::witness::MapState::forge(
                    self.map_rect,
                    self.viewport.center,
                    map::world_pixels(self.viewport),
                    self.trail_coloring,
                    self.vector
                        .as_ref()
                        .map_or(0, VectorField::presented_tile_count),
                )
            }),
            areas: Some(crate::witness::AreaState {
                regions: self.regions.len(),
                drawing: self.scribe.active(),
                resizing: self
                    .area_handles
                    .resizing()
                    .map(|(slot, corner)| crate::witness::AreaResizeState { slot, corner }),
            }),
            civic: Some(crate::witness::CivicState {
                active: self.civic.rows().len(),
                ready: self
                    .civic
                    .rows()
                    .iter()
                    .filter(|row| matches!(row.state, CivicRowState::Ready(_)))
                    .count(),
                preparing: self
                    .civic
                    .rows()
                    .iter()
                    .filter(|row| matches!(row.state, CivicRowState::Preparing))
                    .count(),
                suggestions: self.civic.suggestions().len(),
            }),
            editor: self.witness_editor(),
            search: Some(self.witness_search()),
            survey: None,
            profile: Some(self.witness_profile()),
        }
    }

    #[cfg(feature = "egui-test")]
    fn witness_editor(&self) -> Option<crate::witness::EditorState> {
        self.view
            .editor()
            .map(|editor| crate::witness::EditorState {
                origin: match &editor.origin {
                    EditorOrigin::New => trailgen_contract::EditorOrigin::New,
                    EditorOrigin::Candidate => trailgen_contract::EditorOrigin::Candidate,
                    EditorOrigin::Saved(_) => trailgen_contract::EditorOrigin::Saved,
                },
                shape: contract_route_shape(editor.shape),
                ready: editor.ready(),
                dragging_support: editor.drag.as_ref().map(|drag| drag.slot),
                support_points: editor
                    .support_points
                    .iter()
                    .map(|support| {
                        let coord = support.coord();
                        [coord.lon, coord.lat]
                    })
                    .collect(),
                coordinate_callouts: editor
                    .coordinate_callouts
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, visible)| visible.then_some(slot))
                    .collect(),
                fault_support: editor.fault.as_ref().and_then(|fault| fault.support_slot),
                route_signature: editor
                    .realization
                    .as_ref()
                    .map(TrailRealization::walk_fingerprint),
                redo_depth: editor.history.redo_depth(),
            })
    }

    #[cfg(feature = "egui-test")]
    fn witness_search(&self) -> crate::witness::SearchState {
        let recipe = self.library.search();
        crate::witness::SearchState {
            phase: if self.forge_phase.active() {
                trailgen_contract::SearchPhase::Running
            } else {
                trailgen_contract::SearchPhase::Idle
            },
            corpus: if self.corpus.is_some() {
                trailgen_contract::CorpusPhase::Updating
            } else {
                trailgen_contract::CorpusPhase::Idle
            },
            results: if self.results_open {
                trailgen_contract::ResultsPhase::Open
            } else {
                trailgen_contract::ResultsPhase::Dormant
            },
            trailhead: recipe.trailhead.is_some(),
            boundary: match (recipe.boundary.is_some(), self.boundary_scribe.active()) {
                (false, false) => BoundaryPhase::Unlimited,
                (false, true) => BoundaryPhase::Drawing,
                (true, false) => BoundaryPhase::Committed,
                (true, true) => BoundaryPhase::Redrawing,
            },
            required: self.edicts.required_count(),
            forbidden: self.edicts.forbidden_count(),
            revision_scheduled: self.search_due.is_some(),
        }
    }

    #[cfg(feature = "egui-test")]
    fn witness_profile(&self) -> crate::witness::ProfileState {
        crate::witness::ProfileState {
            visible: self
                .view
                .editor()
                .is_some_and(|editor| editor.profile.is_some())
                || (self.view.focus().is_some() && self.has_profile()),
            locked: self.profile_cursor.locked_m.is_some(),
            marker: self.profile_cursor.marker.is_some(),
        }
    }

    pub fn water_frame(
        &mut self,
        ctx: &egui::Context,
        pixels_per_point: f32,
        tooltip_rects: &[egui::Rect],
    ) -> WaterFrame {
        self.living_wait.compose(ctx, &mut self.water);
        self.water.frame(ctx, pixels_per_point, tooltip_rects, None)
    }

    const fn command_contexts(&self) -> &'static [CommandContext] {
        match (&self.view, self.creator_mode) {
            (WorkbenchView::Browse, CreatorMode::Neutral) => &[CommandContext::Creator],
            (WorkbenchView::Browse, CreatorMode::Finder) => {
                &[CommandContext::Finder, CommandContext::Creator]
            }
            (WorkbenchView::Focus(Focus::Candidate { .. }), CreatorMode::Neutral)
            | (WorkbenchView::Focus(Focus::Saved(_)), _) => {
                &[CommandContext::Focus, CommandContext::Creator]
            }
            (WorkbenchView::Focus(Focus::Candidate { .. }), CreatorMode::Finder) => &[
                CommandContext::Focus,
                CommandContext::Finder,
                CommandContext::Creator,
            ],
            (WorkbenchView::Edit(_), _) => &[CommandContext::Editor],
        }
    }

    const fn command_idioms(&self) -> &'static [GuideSection] {
        match (&self.view, self.creator_mode) {
            (WorkbenchView::Browse, CreatorMode::Neutral) => &commands::BROWSE_IDIOMS,
            (WorkbenchView::Browse, CreatorMode::Finder) => &commands::FINDER_IDIOMS,
            (WorkbenchView::Focus(Focus::Candidate { .. }), CreatorMode::Finder) => {
                &commands::CANDIDATE_IDIOMS
            }
            (WorkbenchView::Focus(Focus::Candidate { .. }), CreatorMode::Neutral) => {
                &commands::SAVED_IDIOMS
            }
            (WorkbenchView::Focus(Focus::Saved(_)), _) => &commands::SAVED_IDIOMS,
            (WorkbenchView::Edit(_), _) => &commands::EDITOR_IDIOMS,
        }
    }

    fn edict_status(&self, edict: Edict) -> CommandStatus<'static> {
        match edict {
            Edict::OpenProjects if self.view.is_editing() => {
                CommandStatus::Disabled("save or discard the trail edit first")
            }
            Edict::FindTrails if self.forge_phase.active() => CommandStatus::Hidden,
            Edict::FindTrails | Edict::BeginManual if self.sinew.is_none() => {
                CommandStatus::Disabled("the trail network is still preparing")
            }
            Edict::FindTrails if self.corpus.is_some() => {
                CommandStatus::Disabled("the trail network is being updated")
            }
            Edict::FindTrails if self.search_validation().is_some() => {
                CommandStatus::Disabled("complete a valid trail recipe first")
            }
            Edict::StopSearch
                if matches!(
                    self.forge_phase,
                    ForgePhase::Striking { stopping: true, .. }
                ) =>
            {
                CommandStatus::Disabled("the search is already stopping")
            }
            Edict::StopSearch if self.forge_phase.active() => CommandStatus::Enabled,
            Edict::BeginManual if self.forge_phase.active() || self.corpus.is_some() => {
                CommandStatus::Disabled("wait for the current operation to finish")
            }
            Edict::UndoSearchEdit if self.edict_history.can_undo() => CommandStatus::Enabled,
            Edict::UndoSearchEdit => CommandStatus::Disabled("there is no segment edit to undo"),
            Edict::RedoSearchEdit if self.edict_history.can_redo() => CommandStatus::Enabled,
            Edict::RedoSearchEdit => CommandStatus::Disabled("there is no segment edit to redo"),
            Edict::EditTrail if self.sinew.is_none() || self.corpus.is_some() => {
                CommandStatus::Disabled("wait for the trail network to finish updating")
            }
            Edict::EditTrail if self.focus_design().is_none() => {
                CommandStatus::Disabled("this legacy trail has no support-point design")
            }
            Edict::SaveCandidate if matches!(self.view.focus(), Some(Focus::Saved(_))) => {
                CommandStatus::Hidden
            }
            Edict::SaveCandidate if self.sinew.is_none() || self.corpus.is_some() => {
                CommandStatus::Disabled("wait for the trail network to finish updating")
            }
            Edict::RenameFocused if matches!(self.view.focus(), Some(Focus::Candidate { .. })) => {
                CommandStatus::Hidden
            }
            Edict::UndoTrailEdit
                if self
                    .view
                    .editor()
                    .is_some_and(|editor| editor.history.can_undo()) =>
            {
                CommandStatus::Enabled
            }
            Edict::UndoTrailEdit => CommandStatus::Disabled("there is no trail edit to undo"),
            Edict::RedoTrailEdit
                if self
                    .view
                    .editor()
                    .is_some_and(|editor| editor.history.can_redo()) =>
            {
                CommandStatus::Enabled
            }
            Edict::RedoTrailEdit => CommandStatus::Disabled("there is no trail edit to redo"),
            Edict::SaveTrail if self.view.editor().is_some_and(TrailEditor::ready) => {
                CommandStatus::Enabled
            }
            Edict::SaveTrail => CommandStatus::Disabled("the trail design is not ready to save"),
            Edict::CreateProject
            | Edict::OpenProject
            | Edict::DrawMapArea
            | Edict::RefreshMapAreas
            | Edict::StopSearch => CommandStatus::Hidden,
            Edict::OpenProjects
            | Edict::FindTrails
            | Edict::ToggleFinder
            | Edict::BeginManual
            | Edict::EditTrail
            | Edict::SaveCandidate
            | Edict::RenameFocused
            | Edict::DiscardTrailEdit
            | Edict::RenameEditor => CommandStatus::Enabled,
        }
    }

    fn apply_edict(&mut self, dispatch: CommandDispatch<'_, Edict>) {
        let edict = match dispatch {
            CommandDispatch::Invoke(edict) => edict,
            CommandDispatch::Refused { reason, .. } => {
                self.status = format!("Unavailable: {reason}.");
                return;
            }
        };
        match edict {
            Edict::OpenProjects => self.workspace_signal = Some(Action::Projects),
            Edict::FindTrails => {
                if let Some(fault) = self.search_validation() {
                    self.status = format!("Could not find trails: {fault}");
                } else {
                    self.strike();
                }
            }
            Edict::StopSearch => self.stop_search(),
            Edict::ToggleFinder => self.toggle_finder(),
            Edict::BeginManual => self.begin_editor(EditorOrigin::New, None),
            Edict::UndoSearchEdit => self.undo_edict(),
            Edict::RedoSearchEdit => self.redo_edict(),
            Edict::EditTrail => self.edit_focus(),
            Edict::SaveCandidate => self.save_focused_candidate(),
            Edict::RenameFocused => {
                if let Some(Focus::Saved(id)) = self.view.focus().cloned() {
                    self.begin_rename(id);
                }
            }
            Edict::DiscardTrailEdit => self.discard_editor(),
            Edict::UndoTrailEdit => self.undo_editor(),
            Edict::RedoTrailEdit => self.redo_editor(),
            Edict::SaveTrail => self.save_editor(),
            Edict::RenameEditor => self.begin_editor_rename(),
            Edict::CreateProject
            | Edict::OpenProject
            | Edict::DrawMapArea
            | Edict::RefreshMapAreas => {
                "That command belongs to another Trailgen workspace.".clone_into(&mut self.status);
            }
        }
    }

    fn search_validation(&self) -> Option<String> {
        self.sinew
            .as_ref()
            .context("trail network is still preparing")
            .and_then(|sinew| {
                self.search_request(self.serial.saturating_add(1))
                    .and_then(|request| request.validate(&sinew.graph))
            })
            .err()
            .map(|error| error.to_string())
    }

    fn command_guide(&mut self, ui: &egui::Ui) {
        let contexts = self.command_contexts();
        let idioms = self.command_idioms();
        let mut guide = std::mem::take(&mut self.guide);
        guide.show(
            ui.ctx(),
            commands::canon(),
            contexts,
            commands::scope_name,
            |edict| self.edict_status(edict),
            idioms,
        );
        if let Some(rect) = guide.rect() {
            crate::witness::rect(ui.ctx(), Target::CommandGuide, rect);
        }
        self.guide = guide;
    }

    fn inspector(&mut self, ui: &mut egui::Ui, navigator: &mut PanelNavigator) {
        let _name = ui.label(chrome::title(self.name.to_ascii_uppercase()));
        ui.add_space(3.0);
        let spec = commands::canon().spec(Edict::OpenProjects);
        let projects = ui
            .add_enabled(
                !self.view.is_editing(),
                chrome::command_spec_button(ui, spec, false)
                    .min_size(vec2(ui.available_width(), 27.0)),
            )
            .on_hover_text(format!(
                "{} · {}",
                spec.detail(),
                commands::canon().shortcuts(Edict::OpenProjects)[0].label(ui.ctx())
            ))
            .on_disabled_hover_text("Save or discard the trail edit first.");
        chrome::tension(ui, &projects);
        if chrome::exact_activation(ui, &projects) {
            self.apply_edict(CommandDispatch::Invoke(Edict::OpenProjects));
            self.water.click(projects.rect);
        }
        ui.add_space(3.0);
        let mut panels = navigator.frame(ui.ctx());
        self.section(
            &mut panels,
            ui,
            "library",
            "saved trails",
            true,
            Self::library_panel,
        );
        self.section(
            &mut panels,
            ui,
            "search",
            "trail creator",
            true,
            Self::search_panel,
        );
        self.section(
            &mut panels,
            ui,
            "calibration",
            "calibration",
            true,
            Self::calibration_panel,
        );
        self.section(
            &mut panels,
            ui,
            "areas",
            "map areas",
            true,
            Self::area_panel,
        );
        self.section(
            &mut panels,
            ui,
            "overlays",
            "overlays",
            true,
            Self::civic_panel,
        );
    }

    fn calibration_panel(&mut self, ui: &mut egui::Ui) {
        let label = ui.label(chrome::eyebrow(format!(
            "{} · KM/H",
            BASE_PACE_SETTING.name()
        )));
        let _label = Glosses::BASE_PACE.explain(label);
        let mut kmh = self.base_pace.kmh();
        let pace = ui.add(
            egui::DragValue::new(&mut kmh)
                .suffix(" KM/H")
                .range(0.5..=15.0)
                .speed(0.1)
                .max_decimals(1),
        );
        crate::witness::anchor(ui, Target::BasePace, pace.rect);
        let changed = pace.changed();
        let _pace = Glosses::BASE_PACE.explain(pace);
        if changed && BasePace::forge(kmh).is_some() {
            self.pending_base_pace = Some(kmh);
        }
    }

    fn civic_panel(&mut self, ui: &mut egui::Ui) {
        self.civic_completion(ui);
        ui.add_space(5.0);
        let _count = chrome::note(ui, format!("{} ACTIVE", self.civic.rows().len()));
        if let Some(action) = self.civic_rows(ui) {
            self.apply_civic_row_action(action);
        }
    }

    fn civic_completion(&mut self, ui: &mut egui::Ui) {
        let before = self.civic.query().to_owned();
        let entry = ui.add(
            egui::TextEdit::singleline(self.civic.query_mut())
                .font(egui::TextStyle::Monospace)
                .hint_text("city or borough…")
                .desired_width(ui.available_width()),
        );
        crate::witness::anchor(ui, Target::CivicSearch, entry.rect);
        if let Some(wake) = chrome::text_wake(ui, &entry, &before, self.civic.query()) {
            self.water.text(wake);
        }
        if self.civic.query() != before {
            let anchor = self.civic_project_anchor();
            self.civic.lookup(anchor);
        }
        let owns_keys = entry.has_focus() || !ui.ctx().text_edit_focused();
        let suggestions_open = !self.civic.suggestions().is_empty();
        if suggestions_open
            && owns_keys
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab))
        {
            self.civic.cycle_suggestion(true);
        } else if suggestions_open
            && owns_keys
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
        {
            self.civic.cycle_suggestion(false);
        }
        if suggestions_open
            && owns_keys
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.civic.dismiss_suggestions();
        }
        let accepted_by_key = suggestions_open
            && owns_keys
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let mut accepted = accepted_by_key
            .then(|| self.civic.selected_suggestion())
            .flatten();
        let suggestions = self.civic.suggestions().to_vec();
        let selected = self.civic.suggestion_pick();
        if !suggestions.is_empty() {
            ui.add_space(3.0);
            let _suggestions = ui.horizontal_wrapped(|ui| {
                for (slot, suggestion) in suggestions.iter().enumerate() {
                    let picked = slot == selected;
                    let cursor = if picked { "▸ " } else { "" };
                    let response = chrome::complete_chip(
                        ui,
                        RichText::new(format!("{cursor}{}", suggestion.caption())),
                        picked,
                    );
                    crate::witness::anchor(ui, Target::CivicSuggestion(slot), response.rect);
                    if response.hovered() {
                        self.civic.set_suggestion_pick(slot);
                    }
                    if response.clicked() {
                        accepted = Some(suggestion.clone());
                    }
                }
            });
        }
        if let Some(record) = accepted {
            if self.offline {
                "Reconnect to add a civic boundary.".clone_into(&mut self.status);
            } else {
                let name = record.name.clone();
                match self.civic.add(record) {
                    AddOutcome::Added(_) => {}
                    AddOutcome::Existing(_) => {
                        self.status = format!("{name} is already active.");
                    }
                }
            }
        }
    }

    fn civic_rows(&self, ui: &mut egui::Ui) -> Option<CivicRowAction> {
        let mut action = None;
        for (slot, row) in self.civic.rows().iter().enumerate() {
            let key = row.record.key.clone();
            let caption = row.record.caption();
            match &row.state {
                CivicRowState::Ready(_) => {
                    let _row = ui.horizontal(|ui| {
                        let width = (ui.available_width() - 61.0).max(72.0);
                        let area =
                            ui.add_sized([width, 27.0], chrome::command_button(caption, false));
                        crate::witness::anchor(ui, Target::CivicArea(slot), area.rect);
                        if area.clicked() {
                            action = Some(CivicRowAction::Fit(key, area.rect));
                        }
                        let remove = chrome::command(ui, "REMOVE", false);
                        crate::witness::anchor(ui, Target::CivicRemove(slot), remove.rect);
                        if remove.clicked() {
                            action = Some(CivicRowAction::Remove(slot, remove.rect));
                        }
                    });
                }
                CivicRowState::Preparing => {
                    let area = ui.add_enabled(
                        false,
                        chrome::command_button(caption, false)
                            .min_size(egui::vec2(ui.available_width(), 27.0)),
                    );
                    crate::witness::anchor(ui, Target::CivicArea(slot), area.rect);
                    let _state = chrome::note(ui, "PREPARING BOUNDARY…");
                }
                CivicRowState::Fault(fault) => {
                    let area = ui.add_enabled(
                        false,
                        chrome::command_button(caption, false)
                            .min_size(egui::vec2(ui.available_width(), 27.0)),
                    );
                    crate::witness::anchor(ui, Target::CivicArea(slot), area.rect);
                    let _fault = chrome::note(ui, fault.to_ascii_uppercase());
                    let _commands = ui.horizontal(|ui| {
                        let retry = chrome::command(ui, "RETRY", false);
                        crate::witness::anchor(ui, Target::CivicRetry(slot), retry.rect);
                        if retry.clicked() {
                            action = Some(CivicRowAction::Retry(slot, retry.rect));
                        }
                        let remove = chrome::command(ui, "REMOVE", false);
                        crate::witness::anchor(ui, Target::CivicRemove(slot), remove.rect);
                        if remove.clicked() {
                            action = Some(CivicRowAction::Remove(slot, remove.rect));
                        }
                    });
                }
            }
            ui.add_space(3.0);
        }
        action
    }

    fn apply_civic_row_action(&mut self, action: CivicRowAction) {
        match action {
            CivicRowAction::Fit(key, rect) => {
                self.fit = Fit::Civic(key);
                self.water.click(rect);
            }
            CivicRowAction::Retry(slot, rect) => {
                self.civic.retry(slot);
                self.water.click(rect);
            }
            CivicRowAction::Remove(slot, rect) => {
                if let Some(record) = self.civic.remove(slot) {
                    self.status = format!("Removed {} boundary.", record.name);
                }
                self.water.click(rect);
            }
        }
    }

    fn civic_project_anchor(&self) -> Coord {
        if self.regions.is_empty() {
            return map::world_to_coord(self.viewport.center);
        }
        let count = self.regions.len() as f64;
        let (lon, lat) = self.regions.iter().fold((0.0, 0.0), |(lon, lat), region| {
            (
                (region.bounds.west + region.bounds.east).mul_add(0.5, lon),
                (region.bounds.south + region.bounds.north).mul_add(0.5, lat),
            )
        });
        Coord::new(lon / count, lat / count)
    }

    fn section(
        &mut self,
        panels: &mut PanelFrame<'_>,
        ui: &mut egui::Ui,
        id: &'static str,
        title: &'static str,
        open: bool,
        body: fn(&mut Self, &mut egui::Ui),
    ) {
        let open = self.shutters.get(id).copied().unwrap_or(open);
        let section = panels.section(ui, id, title, open, |ui| body(self, ui));
        crate::witness::response(ui, Target::Panel(id), &section.header);
        if let Some(wake) = section.wake.as_ref() {
            let _prior = self
                .shutters
                .insert(id.to_owned(), matches!(wake.flux, chrome::FoldFlux::Open));
        }
        self.water.fold(section.wake);
    }

    fn search_panel(&mut self, ui: &mut egui::Ui) {
        if self.creator_tabs(ui) {
            return;
        }
        ui.add_space(6.0);
        if self.view.is_editing() {
            self.editor_panel(ui);
            return;
        }
        if self.creator_mode == CreatorMode::Neutral {
            let _counsel = chrome::note(ui, "SELECT MANUAL TO DRAW OR FINDER TO SEARCH");
            return;
        }
        let striking = self.forge_phase.active();
        let mut recipe = self.library.search().clone();
        let original = recipe.clone();

        self.trailhead_editor(ui, &mut recipe);
        self.search_boundary_editor(ui, &mut recipe);
        let recipe_edit = self.search_recipe_editor(ui, &mut recipe);
        let trailhead_missing = recipe.trailhead.is_none();

        if recipe_edit.changed || recipe != original {
            *self.library.search_mut() = recipe;
            self.mark_library_dirty();
            self.schedule_revision();
        }

        if self.shows_search_context() && self.candidates.is_some() {
            let _edicts = chrome::note(
                ui,
                format!(
                    "{} REQUIRED · {} EXCLUDED",
                    self.edicts.required_count(),
                    self.edicts.forbidden_count()
                ),
            );
            let _help = chrome::note(ui, "CLICK TRAIL TO REQUIRE · SHIFT+CLICK TO EXCLUDE");
        }

        ui.add_space(6.0);
        if let Some(progress) = self.forge_phase.progress() {
            search_progress(ui, progress);
            ui.add_space(4.0);
        }
        if self.armament_retry(ui) {
            return;
        }
        let validation = self.search_validation();
        let stopping = matches!(
            self.forge_phase,
            ForgePhase::Striking { stopping: true, .. }
        );
        let edict = if striking {
            Edict::StopSearch
        } else {
            Edict::FindTrails
        };
        let spec = commands::canon().spec(edict);
        let find = ui.add_enabled(
            if striking {
                !stopping
            } else {
                validation.is_none()
            },
            if stopping {
                chrome::command_button("STOPPING…", true)
            } else {
                chrome::command_spec_button(ui, spec, striking || validation.is_none())
            }
            .min_size(vec2(ui.available_width(), 36.0)),
        );
        let find = match (striking, validation.as_deref()) {
            (false, Some(fault)) => find.on_disabled_hover_text(fault),
            (true, _) => find.on_hover_text("Stop this search and keep its candidates · Esc"),
            (false, None) => find.on_hover_text("Find trails with this recipe · Enter"),
        };
        crate::witness::anchor(
            ui,
            if striking { Target::Stop } else { Target::Find },
            find.rect,
        );
        chrome::tension(ui, &find);
        if !striking && trailhead_missing {
            let _reason = chrome::note(ui, "PLACE A TRAILHEAD TO ENABLE SEARCH");
        }
        let activated = chrome::exact_activation(ui, &find);
        if activated && striking {
            self.stop_search();
            self.water.thwack(find.rect, 0.7);
        } else if !striking && (activated || recipe_edit.submitted) {
            self.activate_search(validation.as_deref(), find.rect);
        }
    }

    fn creator_tabs(&mut self, ui: &mut egui::Ui) -> bool {
        let editing = self.view.is_editing();
        let finder_selected = !editing && self.creator_mode == CreatorMode::Finder;
        let (finder, manual) = ui
            .horizontal(|ui| {
                let finder = ui.add_enabled(
                    !editing,
                    if editing {
                        chrome::command_button("FINDER", false)
                    } else {
                        chrome::command_spec_button(
                            ui,
                            commands::canon().spec(Edict::ToggleFinder),
                            finder_selected,
                        )
                    },
                );
                chrome::tension(ui, &finder);
                let finder = finder
                    .on_disabled_hover_text("Save or discard the trail edit first")
                    .on_hover_text(if finder_selected {
                        "Close the trail finder"
                    } else {
                        "Find trails from a trailhead"
                    });
                let manual = ui.add_enabled(
                    editing
                        || (self.sinew.is_some()
                            && !self.forge_phase.active()
                            && self.corpus.is_none()),
                    if editing {
                        chrome::command_button("MANUAL", true)
                    } else {
                        chrome::command_spec_button(
                            ui,
                            commands::canon().spec(Edict::BeginManual),
                            false,
                        )
                    },
                );
                chrome::tension(ui, &manual);
                let manual = manual
                    .on_disabled_hover_text("Wait for the current operation to finish")
                    .on_hover_text("Draw a trail with support points");
                (finder, manual)
            })
            .inner;
        crate::witness::anchor(ui, Target::Finder, finder.rect);
        crate::witness::anchor(ui, Target::Manual, manual.rect);
        if chrome::exact_activation(ui, &finder) && !editing {
            self.toggle_finder();
            self.water.select(finder.rect);
            return true;
        }
        if chrome::exact_activation(ui, &manual) && !editing {
            self.begin_editor(EditorOrigin::New, None);
            self.water.select(manual.rect);
            return true;
        }
        false
    }

    fn toggle_finder(&mut self) {
        if self.view.is_editing() {
            return;
        }
        self.creator_mode = match self.creator_mode {
            CreatorMode::Neutral => {
                self.status = finder_counsel(&self.library);
                CreatorMode::Finder
            }
            CreatorMode::Finder => {
                self.scribe.disarm();
                self.boundary_scribe.disarm();
                self.placing_trailhead = false;
                self.trailhead_drag = None;
                self.search_due = None;
                "Trail finder closed.".clone_into(&mut self.status);
                CreatorMode::Neutral
            }
        };
    }

    fn activate_search(&mut self, fault: Option<&str>, button: egui::Rect) {
        if let Some(fault) = fault {
            self.status = format!("Could not find trails: {fault}");
        } else {
            self.strike();
            self.water.thwack(button, 0.7);
        }
    }

    fn retry_armament(&mut self, ctx: &egui::Context) {
        match CorpusForge::spawn(ctx, self.root.clone(), self.library.legacy_routes_pending()) {
            Ok(forge) => {
                self.corpus = Some(CorpusTask::Preparing(forge));
                self.trail_data_status = Some("Preparing trail network…".to_owned());
                "Preparing trail network…".clone_into(&mut self.status);
            }
            Err(err) => self.status = format!("Could not prepare trail network: {err:#}"),
        }
    }

    fn armament_retry(&mut self, ui: &mut egui::Ui) -> bool {
        if self.sinew.is_some() || self.corpus.is_some() {
            return false;
        }
        let retry = ui
            .add(
                chrome::command_button("RETRY TRAIL NETWORK", true)
                    .min_size(vec2(ui.available_width(), 36.0)),
            )
            .on_hover_text("Prepare this project's trail network again");
        chrome::tension(ui, &retry);
        if retry.clicked() {
            self.retry_armament(ui.ctx());
            self.water.click(retry.rect);
        }
        true
    }

    fn trailhead_editor(&mut self, ui: &mut egui::Ui, recipe: &mut SearchRecipe) {
        let _trailhead = ui.label(chrome::eyebrow("TRAILHEAD"));
        let _trailhead_row = ui.horizontal(|ui| {
            let placing = self.placing_trailhead;
            let place = ui.add_enabled(
                self.sinew.is_some(),
                chrome::command_button(
                    if placing {
                        "CANCEL"
                    } else if recipe.trailhead.is_some() {
                        "MOVE ON MAP"
                    } else {
                        "PLACE ON MAP"
                    },
                    placing,
                )
                .min_size(vec2(
                    if recipe.trailhead.is_some() {
                        139.0
                    } else {
                        184.0
                    },
                    27.0,
                )),
            );
            let place = if self.sinew.is_none() {
                place.on_disabled_hover_text("Wait for the trail network to finish preparing")
            } else if placing {
                place.on_hover_text("Cancel trailhead placement · Esc")
            } else {
                place.on_hover_text("Place or move the trailhead on the map · Alt+Click")
            };
            crate::witness::anchor(ui, "search.trailhead", place.rect);
            chrome::tension(ui, &place);
            if place.clicked() {
                self.placing_trailhead = !placing;
                self.trailhead_drag = None;
                if self.placing_trailhead {
                    self.scribe.disarm();
                    self.boundary_scribe.disarm();
                    self.dissolve_focus();
                }
                self.water.click(place.rect);
            }
            if recipe.trailhead.is_some() {
                let clear =
                    ui.add(chrome::command_button("CLEAR", false).min_size(vec2(48.0, 27.0)));
                if clear.clicked() {
                    recipe.trailhead = None;
                    self.placing_trailhead = false;
                    self.trailhead_drag = None;
                    self.water.click(clear.rect);
                }
            }
        });
        let _set = chrome::note(
            ui,
            if recipe.trailhead.is_some() {
                "TRAILHEAD SET · DRAG PIN OR ALT+CLICK TO MOVE"
            } else {
                "ALT+CLICK MAP TO PLACE"
            },
        );
    }

    fn search_boundary_editor(&mut self, ui: &mut egui::Ui, recipe: &mut SearchRecipe) {
        ui.add_space(5.0);
        let _boundary = ui.label(chrome::eyebrow("SEARCH AREA"));
        let _boundary_row = ui.horizontal(|ui| {
            let drawing = self.boundary_scribe.active();
            let draw = ui.add(
                chrome::command_button(
                    if drawing {
                        "CANCEL DRAWING"
                    } else if recipe.boundary.is_some() {
                        "REDRAW ON MAP"
                    } else {
                        "DRAW ON MAP"
                    },
                    drawing,
                )
                .min_size(vec2(
                    if recipe.boundary.is_some() {
                        139.0
                    } else {
                        184.0
                    },
                    27.0,
                )),
            );
            let draw = if drawing {
                draw.on_hover_text("Cancel drawing this search boundary · Esc")
            } else {
                draw
            };
            crate::witness::anchor(ui, Target::Boundary, draw.rect);
            chrome::tension(ui, &draw);
            if draw.clicked() {
                if drawing {
                    self.boundary_scribe.disarm();
                } else {
                    self.boundary_scribe.arm();
                    self.scribe.disarm();
                    self.placing_trailhead = false;
                    self.trailhead_drag = None;
                    self.dissolve_focus();
                }
                self.water.click(draw.rect);
            }
            if recipe.boundary.is_some() {
                let clear =
                    ui.add(chrome::command_button("CLEAR", false).min_size(vec2(48.0, 27.0)));
                if clear.clicked() {
                    recipe.boundary = None;
                    self.boundary_scribe.disarm();
                    self.water.click(clear.rect);
                }
            }
        });
        let _state = chrome::note(
            ui,
            if recipe.boundary.is_some() {
                "ROUTES STAY INSIDE THE SEARCH BOUNDARY"
            } else {
                "NO SEARCH-AREA LIMIT"
            },
        );
    }

    fn search_recipe_editor(&mut self, ui: &mut egui::Ui, recipe: &mut SearchRecipe) -> SearchEdit {
        ui.add_space(5.0);
        let distance = distance_range(ui, &mut recipe.distance_m.min, &mut recipe.distance_m.max);
        let moving_time = moving_time_range(
            ui,
            &mut recipe.moving_time_s.min,
            &mut recipe.moving_time_s.max,
        );
        let climb = measure_range(
            ui,
            MeasureKind::Climb,
            &mut recipe.climb_m.min,
            &mut recipe.climb_m.max,
            10.0,
            [None, None],
        );
        let load_label = ui.label(chrome::eyebrow("LOWER-LIMB LOAD · FGJW KM"));
        let _load_label = Glosses::FGJW.explain(load_label);
        let load = ui.add(
            egui::DragValue::new(&mut recipe.lower_limb_load_km)
                .prefix("TARGET ")
                .range(0.0..=1_000.0)
                .speed(0.5)
                .max_decimals(1),
        );
        crate::witness::anchor(ui, Target::LowerLimbLoad, load.rect);
        let load_changed = load.changed();
        let load_submitted = response_submitted(ui, &load);
        let _load = Glosses::FGJW.explain(load);
        let load_edit = SearchEdit {
            changed: load_changed,
            submitted: load_submitted,
        };
        let _shape = ui.label(chrome::eyebrow("SHAPE"));
        let mut shape_changed = false;
        let _shapes = ui.horizontal_wrapped(|ui| {
            for (shape, label) in SHAPES {
                let response = chrome::command(ui, label, recipe.shape == shape);
                crate::witness::anchor(
                    ui,
                    format!("search.shape/{}", route_shape_name(shape)),
                    response.rect,
                );
                if response.clicked() && recipe.shape != shape {
                    recipe.shape = shape;
                    shape_changed = true;
                    self.water.select(response.rect);
                }
            }
        });
        distance
            .merge(moving_time)
            .merge(climb)
            .merge(load_edit)
            .merge(SearchEdit {
                changed: shape_changed,
                submitted: false,
            })
    }

    fn editor_panel(&mut self, ui: &mut egui::Ui) {
        let Some(editor) = self.view.editor() else {
            return;
        };
        let count = editor.support_points.len();
        let ready = editor.ready();
        let realizing = editor.realizing.is_some();
        let fault = editor.fault.clone();
        let notice = editor.notice.clone();
        let _mode = chrome::note(ui, format!("{count} SUPPORT POINT(S)"));
        ui.add_space(5.0);
        let _help = chrome::note(
            ui,
            if count == 0 {
                "CLICK A TRAIL TO PLACE THE TRAILHEAD"
            } else {
                "CLICK TO ADD · DRAG TO MOVE · SHIFT+CLICK TO DELETE · ALT+CLICK FOR COORDINATES"
            },
        );
        if realizing {
            let _progress = ui.label(chrome::eyebrow("UPDATING ROUTE…"));
        }
        if let Some(fault) = fault {
            let _fault = ui.colored_label(chrome::HOT, chrome::muted(fault.message));
        } else if let Some(notice) = notice {
            let _notice = ui.colored_label(chrome::HOT, chrome::muted(notice));
        }
        ui.add_space(5.0);
        self.editor_shape_controls(ui);
        ui.add_space(5.0);
        self.editor_command_controls(ui, ready);
    }

    fn editor_command_controls(&mut self, ui: &mut egui::Ui, ready: bool) {
        let can_undo = self
            .view
            .editor()
            .is_some_and(|editor| editor.history.can_undo());
        let can_redo = self
            .view
            .editor()
            .is_some_and(|editor| editor.history.can_redo());
        let shortcut = |edict| {
            commands::canon()
                .shortcuts(edict)
                .iter()
                .map(|shortcut| shortcut.label(ui.ctx()))
                .collect::<Vec<_>>()
                .join(" / ")
        };
        let discard_shortcut = shortcut(Edict::DiscardTrailEdit);
        let save_shortcut = shortcut(Edict::SaveTrail);
        let undo_shortcut = shortcut(Edict::UndoTrailEdit);
        let redo_shortcut = shortcut(Edict::RedoTrailEdit);
        let mechanism = chrome::MechanismSize::Medium;
        let (discard, save, undo, redo) = ui
            .horizontal(|ui| {
                let discard = chrome::Monoglyph::symbol(chrome::Symbol::Delete)
                    .size(mechanism)
                    .show(ui)
                    .on_hover_text(format!("Discard this trail edit · {discard_shortcut}"));
                let save = ui
                    .add_enabled_ui(ready, |ui| {
                        chrome::Monoglyph::symbol(chrome::Symbol::Confirm)
                            .size(mechanism)
                            .show(ui)
                    })
                    .inner
                    .on_hover_text(format!("Save this trail to the project · {save_shortcut}"));
                let undo = ui
                    .add_enabled_ui(can_undo, |ui| {
                        chrome::Monoglyph::symbol(chrome::Symbol::Undo)
                            .size(mechanism)
                            .show(ui)
                    })
                    .inner
                    .on_hover_text(format!("Undo trail edit · {undo_shortcut}"));
                let redo = ui
                    .add_enabled_ui(can_redo, |ui| {
                        chrome::Monoglyph::symbol(chrome::Symbol::Redo)
                            .size(mechanism)
                            .show(ui)
                    })
                    .inner
                    .on_hover_text(format!("Redo trail edit · {redo_shortcut}"));
                (discard, save, undo, redo)
            })
            .inner;
        crate::witness::anchor(ui, Target::EditorDiscard, discard.rect);
        crate::witness::anchor(ui, Target::EditorSave, save.rect);
        crate::witness::anchor(ui, Target::EditorUndo, undo.rect);
        crate::witness::anchor(ui, Target::EditorRedo, redo.rect);
        self.water.monoglyph(&discard);
        self.water.monoglyph(&save);
        self.water.monoglyph(&undo);
        self.water.monoglyph(&redo);
        if discard.clicked() {
            self.discard_editor();
            ui.ctx()
                .request_discard("editor discard changed the workbench structure");
        } else if save.clicked() {
            self.save_editor();
            ui.ctx()
                .request_discard("editor save changed the workbench structure");
        } else if undo.clicked() {
            self.undo_editor();
        } else if redo.clicked() {
            self.redo_editor();
        }
    }

    fn editor_shape_controls(&mut self, ui: &mut egui::Ui) {
        let Some(editor) = self.view.editor() else {
            return;
        };
        let mut looped = editor.shape == RouteShape::Loop;
        let closeable = matches!(editor.shape, RouteShape::Open | RouteShape::Loop);
        if closeable {
            let close_loop = chrome::Checkbox::new(&mut looped, "CLOSE LOOP").show(ui);
            crate::witness::anchor(ui, Target::CloseLoop, close_loop.rect);
            self.water.checkbox(&close_loop);
            if close_loop.changed() {
                if looped {
                    self.close_editor_loop();
                } else {
                    self.remember_editor();
                    self.view
                        .editor_mut()
                        .expect("editor shape controls require an editor")
                        .shape = RouteShape::Open;
                    let _serial = self.reforge_editor();
                    "Loop opened.".clone_into(&mut self.status);
                }
                looped = self
                    .view
                    .editor()
                    .is_some_and(|editor| editor.shape == RouteShape::Loop);
                ui.ctx().request_repaint();
            }
        }
        if !looped {
            return;
        }
        let ready = self.view.editor().is_some_and(TrailEditor::ready);
        let reverse = ui.add_enabled(
            ready,
            chrome::command_button("REVERSE DIRECTION", false)
                .min_size(vec2(ui.available_width(), 27.0)),
        );
        crate::witness::anchor(ui, Target::Reverse, reverse.rect);
        chrome::tension(ui, &reverse);
        if reverse.clicked() {
            self.reverse_editor();
            self.water.click(reverse.rect);
        }
    }

    fn library_panel(&mut self, ui: &mut egui::Ui) {
        let _count = chrome::note(
            ui,
            format!("{} SAVED TRAIL(S)", self.library.trails().len()),
        );
        if self.library.trails().is_empty() {
            let _empty = chrome::note(ui, "SAVE A SEARCH RESULT OR DRAW A TRAIL.");
            return;
        }

        let active = match self.view.focus() {
            Some(Focus::Saved(id)) => Some(id.clone()),
            _ => None,
        };
        let navigable = !self.view.is_editing();
        let pace = self.base_pace;
        let mut opened = None;
        let mut exported = None;
        let mut latch_changed = None;
        for (slot, trail) in self.library.trails().iter().enumerate() {
            let selected = active.as_ref() == Some(&trail.id);
            let mut latched = self.latched_saved.contains(&trail.id);
            let response = library_button(ui, trail, selected, navigable, &mut latched);
            #[cfg(feature = "egui-test")]
            crate::witness::anchor(
                ui,
                format!("library.trail/{}", trail.id.as_str()),
                response.open.rect,
            );
            crate::witness::anchor(ui, Target::SavedVisibility(slot), response.visibility.rect);
            crate::witness::anchor(ui, Target::SavedExport(slot), response.export.rect);
            self.water.monoglyph(&response.visibility);
            self.water.monoglyph(&response.export);
            if response.open.hovered()
                && let Some(projection) = self
                    .saved_projections
                    .get(&trail.id)
                    .and_then(Option::as_ref)
            {
                response.open.show_tooltip_ui(|ui| {
                    gallery::saved_preview(ui, trail, &projection.miniature, pace);
                });
            }
            let hovered = response.open.hovered() || response.visibility.hovered();
            if hovered {
                self.hovered_saved = Some(trail.id.clone());
                self.water
                    .hover(("saved-library", &trail.id), response.open.rect);
            }
            if response.open.clicked()
                || (response.open.has_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter)))
            {
                opened = Some((trail.id.clone(), response.open.rect));
            }
            if response.export.clicked() {
                exported = Some(trail.id.clone());
            }
            if response.visibility.changed() {
                latch_changed = Some((trail.id.clone(), latched));
            }
            ui.add_space(3.0);
        }

        if let Some((id, latched)) = latch_changed {
            self.set_saved_latch(id, latched);
            if latched {
                "Trail shown on the map."
            } else {
                "Trail hidden from the map."
            }
            .clone_into(&mut self.status);
        }
        if let Some((id, rect)) = opened {
            self.rename = None;
            self.enter_focus(Focus::Saved(id));
            self.water.click(rect);
        }
        if let Some(id) = exported {
            self.begin_export(&id);
        }
    }

    fn begin_rename(&mut self, id: TrailId) {
        if let Some(draft) = self.rename.as_mut().filter(|draft| draft.trail == id) {
            draft.seize_focus = true;
            return;
        }
        let Some(trail) = self.library.trail(&id) else {
            return;
        };
        self.rename = Some(RenameDraft {
            trail: id,
            text: trail.name.clone(),
            seize_focus: true,
        });
    }

    fn commit_rename(&mut self) {
        let Some(draft) = self.rename.take() else {
            return;
        };
        match self.library.rename_trail(&draft.trail, &draft.text) {
            Ok(true) => {
                self.inscribe_library();
                "Trail renamed.".clone_into(&mut self.status);
            }
            Ok(false) => {}
            Err(err) => self.status = format!("Could not rename that trail: {err:#}"),
        }
    }

    fn area_panel(&mut self, ui: &mut egui::Ui) {
        let _count = chrome::note(ui, format!("{} DOWNLOADED AREA(S)", self.regions.len()));
        let mutable = self.sinew.is_some()
            && !self.view.is_editing()
            && self.corpus.is_none()
            && !self.forge_phase.active();
        let renameable = !self.view.is_editing() && (self.sinew.is_none() || self.corpus.is_none());
        self.area_picker(ui, mutable);
        if let Some((id, rect)) = self.area_rows(ui, mutable, renameable) {
            if self
                .area_rename
                .as_ref()
                .is_some_and(|draft| draft.region == id)
            {
                self.area_rename = None;
            }
            match self.strike_corpus(ui.ctx(), TrailDataMutation::Remove(id)) {
                Ok(()) => self.water.click(rect),
                Err(err) => self.status = format!("Could not remove that map area: {err:#}"),
            }
        }
        self.area_refresher(ui, mutable);
    }

    fn area_picker(&mut self, ui: &mut egui::Ui, mutable: bool) {
        let selecting = self.scribe.active();
        let select = ui.add_enabled(
            !self.offline && mutable,
            chrome::command_button(
                if selecting {
                    "CANCEL DRAWING"
                } else {
                    "ADD MAP AREA"
                },
                selecting,
            )
            .min_size(vec2(ui.available_width(), 27.0)),
        );
        let select = if selecting {
            select.on_hover_text("Cancel drawing this map area · Esc")
        } else {
            select
        };
        crate::witness::anchor(ui, Target::AddMapArea, select.rect);
        chrome::tension(ui, &select);
        if select.clicked() {
            if selecting {
                self.scribe.disarm();
            } else {
                self.scribe.arm();
                self.boundary_scribe.disarm();
                self.placing_trailhead = false;
                self.trailhead_drag = None;
                self.dissolve_focus();
            }
            self.water.click(select.rect);
        }
    }

    fn area_rows(
        &mut self,
        ui: &mut egui::Ui,
        mutable: bool,
        renameable: bool,
    ) -> Option<(String, egui::Rect)> {
        let areas = self
            .regions
            .iter()
            .enumerate()
            .map(|(slot, region)| (slot, region.id.clone()))
            .collect::<Vec<_>>();
        let mut excision = None;
        let mut rename_action = None;
        for (slot, id) in areas {
            if self
                .area_rename
                .as_ref()
                .is_some_and(|draft| draft.region == id)
            {
                let draft = self.area_rename.as_mut().expect("area rename checked");
                let _row = ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut draft.text)
                            .font(egui::TextStyle::Monospace)
                            .char_limit(80)
                            .desired_width(116.0),
                    );
                    crate::witness::anchor(ui, Target::AreaRenameField(slot), edit.rect);
                    if draft.seize_focus {
                        edit.request_focus();
                        draft.seize_focus = false;
                    }
                    let save = chrome::command(ui, "SAVE", true)
                        .on_hover_text("Commit this map-area name · Enter");
                    let cancel = chrome::command(ui, "CANCEL", false)
                        .on_hover_text("Cancel this rename · Esc");
                    let (enter, escape) = rename_shortcuts(ui, &edit);
                    if enter || save.clicked() {
                        rename_action = Some(AreaRenameAction::Commit);
                    } else if escape || cancel.clicked() {
                        rename_action = Some(AreaRenameAction::Cancel);
                    }
                });
                continue;
            }
            let action = area_row(
                ui,
                &mut self.water,
                self.region_names.get(&id).map(String::as_str),
                slot,
                mutable,
                renameable,
            );
            match action {
                Some(AreaRowAction::Remove(rect)) => excision = Some((id, rect)),
                Some(AreaRowAction::Rename) => {
                    rename_action = Some(AreaRenameAction::Begin(id));
                }
                None => {}
            }
        }
        match rename_action {
            Some(AreaRenameAction::Begin(region)) => {
                let text = self.region_names.get(&region).cloned().unwrap_or_default();
                self.area_rename = Some(AreaRenameDraft {
                    region,
                    text,
                    seize_focus: true,
                });
            }
            Some(AreaRenameAction::Commit) => self.commit_area_rename(),
            Some(AreaRenameAction::Cancel) => self.area_rename = None,
            None => {}
        }
        excision
    }

    fn area_refresher(&mut self, ui: &mut egui::Ui, mutable: bool) {
        if !self.regions.is_empty() {
            let refresh = ui.add_enabled(
                !self.offline && mutable,
                chrome::command_button("REFRESH TRAILS", false)
                    .min_size(vec2(ui.available_width(), 24.0)),
            );
            crate::witness::anchor(ui, Target::RefreshTrails, refresh.rect);
            chrome::tension(ui, &refresh);
            if refresh.clicked() {
                match self.strike_corpus(ui.ctx(), TrailDataMutation::Refresh) {
                    Ok(()) => self.water.click(refresh.rect),
                    Err(err) => self.status = format!("Could not refresh trails: {err:#}"),
                }
            }
        }
    }

    fn commit_area_rename(&mut self) {
        let Some(draft) = self.area_rename.take() else {
            return;
        };
        match trailgen_data::name_region(&self.root, &draft.region, &draft.text) {
            Ok(config) => {
                self.region_names = config.region_names;
                "Map area named.".clone_into(&mut self.status);
            }
            Err(err) => self.status = format!("Could not name that map area: {err:#}"),
        }
    }

    fn arena(&mut self, ui: &mut egui::Ui) {
        let _toolbar = egui::Panel::top("trail-toolbar")
            .exact_size(TOOLBAR_HEIGHT)
            .show(ui, |ui| self.toolbar(ui));
        let _counsel = egui::Panel::bottom("trail-counsel")
            .exact_size(42.0)
            .show(ui, |ui| self.counsel(ui));
        if let Some(editor) = self.view.editor() {
            if editor.profile.is_some() {
                let _profile = egui::Panel::bottom("trail-profile")
                    .exact_size(PROFILE_HEIGHT)
                    .show(ui, |ui| self.profile(ui));
            }
        } else if self.view.focus().is_some() {
            if self.has_profile() {
                let _profile = egui::Panel::bottom("trail-profile")
                    .exact_size(PROFILE_HEIGHT)
                    .show(ui, |ui| self.profile(ui));
            }
        } else if self.results_open {
            let _results = egui::Panel::bottom("trail-results")
                .exact_size(RESULTS_HEIGHT)
                .show(ui, |ui| self.results_gallery(ui));
        }
        let _map = egui::CentralPanel::default().show(ui, |ui| self.map(ui));
    }

    fn counsel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        let _row = ui.horizontal(|ui| {
            let waiting = self.corpus.is_some();
            let message = if self.sinew.is_none() {
                &self.status
            } else if self.corpus.is_some() {
                self.trail_data_status
                    .as_deref()
                    .unwrap_or("Updating trails…")
            } else if self.scribe.active() {
                "Drag a rectangle across the map to download its trails. Esc cancels."
            } else if self.boundary_scribe.active() {
                "Draw a free-hand loop around the allowed search area. Release to finish; Esc cancels."
            } else if let Some(editor) = self.view.editor() {
                if editor.support_points.is_empty() {
                    "Click a trail to place the first support point. Alt+Delete discards."
                } else {
                    "Click to add support points; drag any bronze pin to reshape the trail."
                }
            } else if self.view.focus().is_some() {
                &self.status
            } else if self.creator_mode == CreatorMode::Neutral {
                "Select Manual to draw a trail or Finder to search."
            } else if self.placing_trailhead {
                "Click a trail to place the trailhead. Alt+click also works; Esc cancels."
            } else if self.active_trailhead().is_none() {
                "Place a trailhead, or Alt+click the map, then choose Find trails."
            } else if self.trailhead_drag.is_some() {
                "Drag the trailhead to a new starting point."
            } else {
                &self.status
            };
            let message = ui.add(
                egui::Label::new(RichText::new(message).monospace().color(chrome::TEXT)).wrap(),
            );
            if waiting {
                let rect = message.rect.expand(5.0);
                self.living_wait.claim(rect);
                crate::witness::anchor(ui, Target::TrailDataWait, rect);
            }
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        if self.view.is_editing() {
            self.editor_toolbar(ui);
        } else if self.view.focus().is_some() {
            self.focus_toolbar(ui);
        } else {
            self.results_toolbar(ui);
        }
    }

    fn results_toolbar(&mut self, ui: &mut egui::Ui) {
        let mut sort = None;
        let mut clear = None;
        let _row = ui.horizontal(|ui| {
            let _results = toolbar_text(ui, "RESULTS", chrome::TEXT);
            ui.separator();
            let _label = toolbar_text(ui, "SORT", chrome::MUTED);
            for candidate in TrailSort::ALL {
                let response = chrome::command(ui, candidate.label(), self.sort == candidate);
                if response.clicked() && self.sort != candidate {
                    sort = Some((candidate, response.rect));
                }
            }
            if self
                .candidates
                .as_ref()
                .is_some_and(|run| !run.routes.is_empty())
            {
                let response = chrome::command(ui, "CLEAR RESULTS", false);
                crate::witness::anchor(ui, "results.clear", response.rect);
                if response.clicked() {
                    clear = Some(response.rect);
                }
            }
        });
        if let Some((sort, rect)) = sort {
            self.sort = sort;
            self.water.select(rect);
        }
        if let Some(rect) = clear {
            if self.forge_phase.active() {
                self.stop_search();
            }
            self.candidates = None;
            self.results_open = false;
            self.edicts.clear();
            self.edict_history.clear();
            self.search_due = None;
            "Search results cleared. Saved trails are untouched.".clone_into(&mut self.status);
            self.water.click(rect);
        }
    }

    fn focus_toolbar(&mut self, ui: &mut egui::Ui) {
        let summary = self.focus_summary();
        let saved_id = match self.view.focus() {
            Some(Focus::Saved(id)) => Some(id.clone()),
            Some(Focus::Candidate { .. }) | None => None,
        };
        let mut action = None;
        let mut rename_action = None;
        let _row = ui.horizontal(|ui| {
            if let Some(command) = self.focus_command_controls(ui) {
                action = Some(command);
            }
            let back = chrome::command(ui, "← BACK", false)
                .on_hover_text("Return to the prior map viewport · Esc");
            crate::witness::anchor(ui, Target::FocusBack, back.rect);
            if back.clicked() {
                action = Some(FocusAction::Close(back.rect));
            }
            let previous = chrome::command_enabled(ui, self.focus_count() > 1, "◀", false)
                .on_hover_text("Previous trail · Left Arrow");
            if previous.clicked() {
                action = Some(FocusAction::Step(-1, previous.rect));
            }
            let next = chrome::command_enabled(ui, self.focus_count() > 1, "▶", false)
                .on_hover_text("Next trail · Right Arrow");
            if next.clicked() {
                action = Some(FocusAction::Step(1, next.rect));
            }
            if let Some((name, metrics)) = &summary {
                ui.separator();
                rename_action = self.focus_name_control(ui, saved_id.as_ref(), name);
                let _metrics = toolbar_text(
                    ui,
                    readout::metrics_summary(metrics, self.base_pace),
                    chrome::MUTED,
                );
                if let Some(standing) = self
                    .focus_standing()
                    .filter(|standing| *standing != TrailStanding::Established)
                {
                    let _standing = ui.colored_label(
                        toolbar_standing_color(standing),
                        RichText::new(format!(
                            "PATH STATUS · {}",
                            map::trail_standing_label(standing)
                        ))
                        .monospace()
                        .size(10.5),
                    );
                }
            }
        });
        let reconcile = rename_action.is_some() || action.is_some();
        self.enact_rename_action(rename_action);
        self.enact_focus_action(action.as_ref());
        if reconcile {
            ui.ctx()
                .request_discard("focus toolbar changed its structural state");
        }
    }

    fn focus_command_controls(&mut self, ui: &mut egui::Ui) -> Option<FocusAction> {
        let editing_ready = self.sinew.is_some() && self.corpus.is_none();
        let editable = editing_ready && self.focus_design().is_some();
        match self.view.focus().cloned() {
            Some(Focus::Candidate { .. }) => {
                let edit = ui
                    .add_enabled_ui(editable, |ui| {
                        commands::canon().button(Edict::EditTrail, ui)
                    })
                    .inner;
                let edit_clicked = edit.clicked();
                let edit = edit
                    .into_response()
                    .on_disabled_hover_text("This trail is not currently editable");
                chrome::tension(ui, &edit);
                crate::witness::anchor(ui, Target::FocusEdit, edit.rect);
                if edit_clicked {
                    return Some(FocusAction::Edit(edit.rect));
                }
                let save = ui.add_enabled(
                    editing_ready,
                    chrome::command_spec_button(
                        ui,
                        commands::canon().spec(Edict::SaveCandidate),
                        true,
                    ),
                );
                chrome::tension(ui, &save);
                crate::witness::anchor(ui, Target::FocusSave, save.rect);
                chrome::exact_activation(ui, &save).then_some(FocusAction::Save(save.rect))
            }
            Some(Focus::Saved(id)) if self.delete_confirmation.as_ref() == Some(&id) => {
                let _warning = ui.colored_label(chrome::HOT, chrome::eyebrow("DELETE TRAIL?"));
                let confirm = chrome::Monoglyph::symbol(chrome::Symbol::Delete)
                    .size(chrome::MechanismSize::Medium)
                    .show(ui)
                    .on_hover_text("Permanently delete this saved trail");
                let cancel = chrome::Monoglyph::symbol(chrome::Symbol::Remove)
                    .size(chrome::MechanismSize::Medium)
                    .show(ui)
                    .on_hover_text("Keep this saved trail");
                crate::witness::anchor(ui, Target::FocusDeleteConfirm, confirm.rect);
                crate::witness::anchor(ui, Target::FocusDeleteCancel, cancel.rect);
                self.water.monoglyph(&confirm);
                self.water.monoglyph(&cancel);
                if confirm.clicked() {
                    Some(FocusAction::ConfirmDelete)
                } else if cancel.clicked() {
                    Some(FocusAction::CancelDelete)
                } else {
                    None
                }
            }
            Some(Focus::Saved(_)) => {
                let delete = chrome::Monoglyph::symbol(chrome::Symbol::Delete)
                    .size(chrome::MechanismSize::Medium)
                    .show(ui)
                    .on_hover_text("Delete this saved trail");
                crate::witness::anchor(ui, Target::FocusDelete, delete.rect);
                self.water.monoglyph(&delete);
                if delete.clicked() {
                    return Some(FocusAction::RequestDelete);
                }
                let edit = ui
                    .add_enabled_ui(editable, |ui| {
                        commands::canon().button(Edict::EditTrail, ui)
                    })
                    .inner;
                let edit_clicked = edit.clicked();
                let edit = edit
                    .into_response()
                    .on_disabled_hover_text("This trail is not currently editable");
                chrome::tension(ui, &edit);
                crate::witness::anchor(ui, Target::FocusEdit, edit.rect);
                edit_clicked.then_some(FocusAction::Edit(edit.rect))
            }
            None => None,
        }
    }

    fn focus_name_control(
        &mut self,
        ui: &mut egui::Ui,
        saved_id: Option<&TrailId>,
        name: &str,
    ) -> Option<RenameAction> {
        let renaming =
            saved_id.is_some_and(|id| self.rename.as_ref().is_some_and(|draft| &draft.trail == id));
        if !renaming {
            let action = saved_id.and_then(|id| {
                let rename = chrome::Monoglyph::symbol(chrome::Symbol::Rename)
                    .size(chrome::MechanismSize::Medium)
                    .show(ui)
                    .on_hover_text(format!(
                        "Rename this saved trail · {}",
                        commands::canon().shortcuts(Edict::RenameFocused)[0].label(ui.ctx())
                    ));
                self.water.monoglyph(&rename);
                crate::witness::anchor(ui, Target::FocusRename, rename.rect);
                rename
                    .clicked()
                    .then(|| RenameAction::Begin(id.clone(), rename.rect))
            });
            let _name = toolbar_title(ui, name.to_ascii_uppercase());
            return action;
        }

        let draft = self.rename.as_mut().expect("rename draft checked");
        let edit = ui.add_sized(
            [190.0, 24.0],
            egui::TextEdit::singleline(&mut draft.text)
                .font(egui::TextStyle::Monospace)
                .text_color(chrome::TEXT)
                .char_limit(80),
        );
        crate::witness::anchor(ui, Target::RenameField, edit.rect);
        if draft.seize_focus {
            edit.request_focus();
            draft.seize_focus = false;
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        let valid = trail_name_is_valid(&draft.text);
        let (enter, escape) = rename_shortcuts(ui, &edit);
        let save = chrome::command_enabled(ui, valid, "SAVE", true)
            .on_hover_text("Commit this trail name · Enter");
        crate::witness::anchor(ui, "focus.rename.save", save.rect);
        let cancel = chrome::command(ui, "CANCEL", false).on_hover_text("Cancel this rename · Esc");
        if valid && (enter || save.clicked()) {
            Some(RenameAction::Commit)
        } else if escape || cancel.clicked() {
            Some(RenameAction::Cancel)
        } else {
            None
        }
    }

    fn enact_rename_action(&mut self, action: Option<RenameAction>) {
        match action {
            Some(RenameAction::Begin(id, rect)) => {
                self.begin_rename(id);
                self.water.click(rect);
            }
            Some(RenameAction::Commit) => self.commit_rename(),
            Some(RenameAction::Cancel) => self.rename = None,
            None => {}
        }
    }

    fn editor_toolbar(&mut self, ui: &mut egui::Ui) {
        let Some(editor) = self.view.editor() else {
            return;
        };
        let name = editor.name.to_ascii_uppercase();
        let summary = editor
            .ready()
            .then_some(editor)
            .and_then(|editor| editor.realization.as_ref())
            .map(|realization| {
                readout::metrics_summary(&realization.route.metrics, self.base_pace)
            });
        let mut action = None;
        let _row = ui.horizontal(|ui| {
            let renaming = self
                .view
                .editor()
                .is_some_and(|editor| editor.name_draft.is_some());
            if renaming {
                let draft = self
                    .view
                    .editor_mut()
                    .and_then(|editor| editor.name_draft.as_mut())
                    .expect("editor rename transaction checked");
                let edit = ui.add_sized(
                    [190.0, 24.0],
                    egui::TextEdit::singleline(&mut draft.text)
                        .font(egui::TextStyle::Monospace)
                        .text_color(chrome::TEXT)
                        .char_limit(80),
                );
                crate::witness::anchor(ui, Target::EditorRenameField, edit.rect);
                if draft.seize_focus {
                    edit.request_focus();
                    draft.seize_focus = false;
                }
                let valid = trail_name_is_valid(&draft.text);
                let (enter, escape) = rename_shortcuts(ui, &edit);
                let save = chrome::command_enabled(ui, valid, "SAVE", true)
                    .on_hover_text("Commit this trail name · Enter");
                let cancel =
                    chrome::command(ui, "CANCEL", false).on_hover_text("Cancel this rename · Esc");
                if valid && (enter || save.clicked()) {
                    action = Some(EditorNameAction::Commit);
                } else if escape || cancel.clicked() {
                    action = Some(EditorNameAction::Cancel);
                }
            } else {
                let rename = chrome::Monoglyph::symbol(chrome::Symbol::Rename)
                    .size(chrome::MechanismSize::Medium)
                    .show(ui)
                    .on_hover_text(format!(
                        "Rename this trail · {}",
                        commands::canon().shortcuts(Edict::RenameEditor)[0].label(ui.ctx())
                    ));
                self.water.monoglyph(&rename);
                crate::witness::anchor(ui, Target::EditorRename, rename.rect);
                if rename.clicked() {
                    action = Some(EditorNameAction::Begin(rename.rect));
                }
                let _name = toolbar_title(ui, name);
            }
            if let Some(summary) = summary {
                ui.separator();
                let _summary = toolbar_text(ui, summary, chrome::MUTED);
            }
        });
        self.enact_editor_name(action.as_ref());
    }

    fn begin_editor_rename(&mut self) {
        let Some(editor) = self.view.editor_mut() else {
            return;
        };
        if let Some(draft) = &mut editor.name_draft {
            draft.seize_focus = true;
        } else {
            editor.name_draft = Some(EditorNameDraft {
                text: editor.name.clone(),
                seize_focus: true,
            });
        }
    }

    fn enact_editor_name(&mut self, action: Option<&EditorNameAction>) {
        match action {
            Some(EditorNameAction::Begin(rect)) => {
                self.begin_editor_rename();
                self.water.click(*rect);
            }
            Some(EditorNameAction::Commit) => {
                let Some(editor) = self.view.editor_mut() else {
                    return;
                };
                let Some(draft) = editor.name_draft.take() else {
                    return;
                };
                let name = draft.text.trim();
                name.clone_into(&mut editor.name);
                if let Some(realization) = &mut editor.realization {
                    name.clone_into(&mut realization.route.name);
                }
                "Trail renamed.".clone_into(&mut self.status);
            }
            Some(EditorNameAction::Cancel) => {
                if let Some(editor) = self.view.editor_mut() {
                    editor.name_draft = None;
                }
            }
            None => {}
        }
    }

    fn enact_focus_action(&mut self, action: Option<&FocusAction>) {
        match action {
            Some(FocusAction::Close(rect)) => {
                self.leave_focus();
                self.water.click(*rect);
            }
            Some(FocusAction::Step(delta, rect)) => {
                self.step_focus(*delta);
                self.water
                    .lever(*rect, if delta.is_negative() { -1.0 } else { 1.0 });
            }
            Some(FocusAction::Save(rect)) => {
                self.save_focused_candidate();
                self.water.click(*rect);
            }
            Some(FocusAction::Edit(rect)) => {
                self.edit_focus();
                self.water.click(*rect);
            }
            Some(FocusAction::RequestDelete) => {
                self.delete_confirmation = match self.view.focus() {
                    Some(Focus::Saved(id)) => Some(id.clone()),
                    Some(Focus::Candidate { .. }) | None => None,
                };
            }
            Some(FocusAction::ConfirmDelete) => {
                self.delete_focused_trail();
            }
            Some(FocusAction::CancelDelete) => {
                self.delete_confirmation = None;
            }
            None => {}
        }
    }

    fn results_gallery(&mut self, ui: &mut egui::Ui) {
        let Some(run) = self.candidates.as_ref() else {
            let striking = matches!(self.forge_phase, ForgePhase::Striking { .. });
            let waiting = ui.available_rect_before_wrap();
            gallery_empty(
                ui,
                if striking {
                    "FINDING TRAILS…"
                } else {
                    "NO RESULTS YET"
                },
            );
            if striking {
                self.living_wait.claim(waiting);
                crate::witness::rect(ui.ctx(), Target::SearchWait, waiting);
            }
            return;
        };
        if run.routes.is_empty() {
            gallery_empty(ui, "NO TRAILS MATCHED THIS SEARCH");
            return;
        }
        let order = gallery::order_candidates(&run.routes, self.sort);
        let mut opened = None;
        let scroll = egui::ScrollArea::horizontal()
            .id_salt("trail-results-rack")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                let _rack = ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    for slot in order.iter().copied() {
                        let identity = run.identities[slot];
                        let active = self
                            .view
                            .focus()
                            .is_some_and(|focus| *focus == Focus::Candidate { identity });
                        let response = gallery::candidate_tile(
                            ui,
                            &run.routes[slot],
                            &run.previews[slot],
                            self.base_pace,
                            identity,
                            active,
                        );
                        crate::witness::anchor(
                            ui,
                            format!("results.candidate/{identity}"),
                            response.rect,
                        );
                        if response.hovered() {
                            self.water.hover(("candidate", slot), response.rect);
                        }
                        if response.clicked() {
                            opened = Some((identity, response.rect));
                        }
                    }
                    ui.add_space(6.0);
                });
            });
        self.water.heave(ui.ctx(), scroll.state.offset.x);
        if let Some((identity, rect)) = opened {
            self.enter_focus(Focus::Candidate { identity });
            self.water.click(rect);
        }
    }

    fn profile(&mut self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        let _label = ui.label(chrome::eyebrow("ELEVATION · TERRAIN"));
        let profile_owner = self.profile_owner();
        self.profile_cursor.bind(profile_owner);
        let profile = if let Some(editor) = self.view.editor() {
            editor.profile.as_ref()
        } else {
            match self.view.focus() {
                Some(Focus::Candidate { identity }) => self
                    .candidates
                    .as_ref()
                    .and_then(|run| run.slot(*identity).and_then(|slot| run.profiles.get(slot)))
                    .and_then(Option::as_deref),
                Some(Focus::Saved(id)) => self
                    .saved_projections
                    .get(id)
                    .and_then(Option::as_ref)
                    .and_then(|projection| projection.profile.as_ref()),
                None => None,
            }
        };
        if let Some(profile) = profile {
            let probe = profile.show(
                ui,
                ui.available_height() - 3.0,
                self.profile_cursor.locked_m,
            );
            crate::witness::anchor(ui, Target::Profile, probe.response.rect);
            chrome::shallow_tension(ui, &probe.response);
            let active_m = self.profile_cursor.resolve(
                probe.hovered_m,
                probe.response.clicked(),
                probe.response.secondary_clicked(),
            );
            self.profile_cursor.marker = active_m.and_then(|distance| profile.coord_at(distance));
            if probe.response.hovered() {
                self.water.hover("trail-profile", probe.response.rect);
            }
            let _response = probe
                .response
                .on_hover_text("Left-click to lock this point · right-click to release");
        } else {
            self.profile_cursor.bind(None);
        }
    }

    fn profile_owner(&self) -> Option<ProfileOwner> {
        match self.view.editor() {
            Some(_) => Some(ProfileOwner::Editor),
            None => match self.view.focus() {
                Some(Focus::Candidate { identity }) => Some(ProfileOwner::Candidate(*identity)),
                Some(Focus::Saved(id)) => Some(ProfileOwner::Saved(id.clone())),
                None => None,
            },
        }
    }

    fn map(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        crate::witness::anchor(ui, Target::Map, response.rect);
        let regime = self.view.map_regime();
        if regime == self.map_regime {
            self.viewport = self.viewport.preserve_visible_extent(self.map_rect, rect);
        }
        self.map_rect = rect;
        self.map_regime = regime;
        self.water.begin(Domain::shelf(rect));
        self.apply_fit(rect);
        let (legend_claims_pointer, legend_rect) = self.interact_trail_legend(ui, rect);
        let pointer = (!legend_claims_pointer)
            .then(|| response.interact_pointer_pos())
            .flatten();
        let support_under_pointer =
            pointer.and_then(|pointer| self.editor_support_at(pointer, rect));
        let (excising_supports, annotating_supports) =
            support_modifiers(self.view.is_editing(), ui);
        let area_handles_enabled = self.area_handles_enabled();
        let resize_event = if legend_claims_pointer {
            ResizeEvent::None
        } else {
            self.area_handles
                .interact(self.viewport, ui, rect, &self.regions, area_handles_enabled)
        };
        if !legend_claims_pointer
            && !excising_supports
            && !annotating_supports
            && ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary))
        {
            self.seize_editor_support(pointer, support_under_pointer, rect);
        }
        let trailhead_gesture = if legend_claims_pointer || self.area_handles.captured() {
            TrailheadGesture::default()
        } else {
            self.interact_trailhead(ui, rect)
        };
        let click_modifiers = (!legend_claims_pointer
            && response.clicked_by(egui::PointerButton::Primary))
        .then(|| primary_click_modifiers(ui, rect))
        .flatten();
        let editor_dragging = self
            .view
            .editor()
            .and_then(|editor| editor.drag.as_ref())
            .is_some();
        if editor_dragging {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if support_under_pointer.is_some() {
            ui.ctx()
                .set_cursor_icon(if excising_supports || annotating_supports {
                    egui::CursorIcon::PointingHand
                } else {
                    egui::CursorIcon::Grab
                });
        }
        let before = self.viewport;
        let map_gesture_captured = self.scribe.active()
            || self.boundary_scribe.active()
            || self.area_handles.captured()
            || editor_dragging
            || (support_under_pointer.is_some() && (excising_supports || annotating_supports))
            || trailhead_gesture.captured;
        let moved = map::navigate_with(
            &mut self.viewport,
            ui,
            &response,
            rect,
            !map_gesture_captured,
            !legend_claims_pointer,
        );
        if moved {
            self.fit = Fit::None;
            if response.dragged() {
                self.water
                    .drag(rect, ui.input(|input| input.pointer.delta().y));
            } else {
                self.water.bump(rect);
            }
        }
        let (scribe_event, boundary_event) = self.interact_scribes(ui, &response, rect);
        self.paint_map_scene(ui, rect, legend_rect);

        self.settle_map_gestures(
            ui,
            &response,
            MapGesture {
                rect,
                pointer,
                support_under_pointer,
                trailhead: trailhead_gesture,
                click_modifiers,
            },
        );
        if before != self.viewport {
            ui.ctx().request_repaint();
        }
        self.handle_scribe(ui.ctx(), &scribe_event);
        self.handle_area_resize(ui.ctx(), resize_event);
        self.handle_boundary(boundary_event);
    }

    fn paint_map_scene(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        legend_rect: Option<egui::Rect>,
    ) {
        let canvas = ui.painter_at(rect);
        let frame = map::MapFramePlan::forge(self.viewport, rect);
        let cartography = product_phase!(
            "map.cartographic_clock",
            self.cartography.observe(self.viewport, ui.ctx())
        );
        let _ground = canvas.rect_filled(rect, 0.0, map::MAP_GROUND);
        let annotations = product_phase!(
            "map.cartography",
            self.forge_cartography(&canvas, frame, cartography)
        );
        if let Some(sinew) = &mut self.sinew {
            product_phase!(
                "map.trail_context",
                sinew
                    .atlas
                    .paint_network(&canvas, frame, self.trail_coloring)
            );
        }
        product_phase!("map.live_areas", self.paint_live_area(&canvas, rect));
        let civic_labels = product_phase!(
            "map.civic_boundaries",
            civic_area::paint_boundaries(
                &canvas,
                frame,
                cartography.zoom.get(),
                self.civic
                    .ready()
                    .map(|(slot, area)| (slot, Arc::clone(area))),
            )
        );
        product_phase!("map.privileged_trails", self.paint_trails(&canvas, rect));
        if self.shows_search_context() {
            self.paint_edicts(&canvas, rect);
        }
        if let Some(annotations) = annotations {
            product_phase!("map.annotations", annotations.paint(&canvas));
        }
        if self.shows_search_context() {
            self.paint_search_boundary(&canvas, rect);
        }
        product_phase!(
            "map.civic_labels",
            civic_area::paint_labels(&canvas, &civic_labels)
        );
        self.paint_profile_marker(&canvas, rect);
        if self.view.is_editing() {
            self.paint_support_points(&canvas, rect, legend_rect);
        } else if let Some(trailhead) = self.active_trailhead() {
            let (coord, seized) = self
                .trailhead_drag
                .as_ref()
                .map_or_else(|| (trailhead.coord(), false), |drag| (drag.preview, true));
            map::paint_start(&canvas, coord, self.viewport, rect, seized);
            let anchor = map::screen_at(self.viewport, rect, map::world_from_coord(coord));
            crate::witness::rect(
                ui.ctx(),
                Target::TrailheadPin,
                egui::Rect::from_center_size(anchor, vec2(24.0, 24.0)),
            );
        }
        self.scale_bar.paint(&canvas, self.viewport, rect);
        let _edge = canvas.rect_stroke(
            rect.shrink(0.5),
            0.0,
            Stroke::new(1.0_f32, chrome::EDGE_STRONG),
            egui::StrokeKind::Inside,
        );
        self.paint_map_header(&canvas, rect);
        self.paint_map_footer(&canvas, rect);
    }

    fn interact_trail_legend(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
    ) -> (bool, Option<egui::Rect>) {
        let Some(sinew) = &mut self.sinew else {
            return (false, None);
        };
        let legend = sinew.atlas.show_legend(ui.ctx(), rect, self.trail_coloring);
        for (coloring, tab) in &legend.tabs {
            let target = coloring_target(*coloring);
            crate::witness::rect(ui.ctx(), target, *tab);
            if ui
                .input(|input| input.pointer.hover_pos())
                .is_some_and(|pointer| tab.contains(pointer))
            {
                self.water.hover(target, *tab);
            }
        }
        if let Some((coloring, tab)) = legend.clicked {
            self.trail_coloring = coloring;
            self.water.click(tab);
            ui.ctx().request_repaint();
        }
        let claims_pointer = ui.input(|input| {
            input
                .pointer
                .hover_pos()
                .zip(legend.rect)
                .is_some_and(|(pointer, legend)| legend.contains(pointer))
        });
        (claims_pointer, legend.rect)
    }

    fn paint_live_area(&self, painter: &egui::Painter, rect: egui::Rect) {
        if !self.regions.is_empty() || self.scribe.active() {
            live_area::paint(
                painter,
                live_area::Scene {
                    viewport: self.viewport,
                    canvas: rect,
                    regions: &self.regions,
                    names: &self.region_names,
                    preview: self.scribe.preview(self.viewport, rect),
                    adjustment: self.area_handles.preview(),
                    handles: self.area_handles_enabled(),
                },
            );
        }
    }

    const fn area_handles_enabled(&self) -> bool {
        self.sinew.is_some()
            && matches!(self.view, WorkbenchView::Browse)
            && self.corpus.is_none()
            && !self.forge_phase.active()
            && !self.scribe.active()
            && !self.boundary_scribe.active()
            && self.area_rename.is_none()
            && !self.placing_trailhead
            && self.trailhead_drag.is_none()
    }

    fn interact_scribes(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: egui::Rect,
    ) -> (ScribeEvent, BoundaryEvent) {
        (
            self.scribe.interact(self.viewport, ui, response, rect),
            self.boundary_scribe
                .interact(self.viewport, ui, response, rect),
        )
    }

    fn paint_search_boundary(&self, painter: &egui::Painter, rect: egui::Rect) {
        search_boundary::paint(
            painter,
            self.viewport,
            rect,
            self.library.search().boundary.as_ref(),
            self.boundary_scribe.preview(),
        );
    }

    fn paint_profile_marker(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(coord) = self.profile_cursor.marker else {
            return;
        };
        let center = map::screen_at(self.viewport, rect, map::world_from_coord(coord));
        let _shadow = painter.circle_stroke(
            center,
            7.5,
            Stroke::new(4.2_f32, Color32::from_black_alpha(185)),
        );
        let _ring = painter.circle_stroke(center, 7.5, Stroke::new(2.3_f32, chrome::HOT));
    }

    fn forge_cartography(
        &mut self,
        painter: &egui::Painter,
        frame: map::MapFramePlan,
        cartography: map::CartographicPlan,
    ) -> Option<Arc<annotation::Composition>> {
        let vector = self.vector.as_mut()?;
        product_phase!(
            "cartography.vector_fills",
            vector.paint_fills(painter, frame, cartography)
        );
        let relief = &self.relief;
        let annotations = product_phase!("cartography.compose_annotations", {
            vector.compose_annotations(painter, frame, cartography, relief.revision(), || {
                relief.annotations(frame, cartography)
            })
        });
        let gaps = annotations.contour_gaps();
        product_phase!(
            "cartography.relief",
            self.relief.paint(painter, frame, Arc::clone(&gaps))
        );
        product_phase!(
            "cartography.vector_strokes",
            vector.paint_strokes(painter, frame, gaps)
        );
        Some(annotations)
    }

    fn paint_trails(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        let frame = map::MapFramePlan::forge(self.viewport, rect);
        let focused_saved = match &self.view {
            WorkbenchView::Focus(Focus::Saved(id)) => Some(id.clone()),
            WorkbenchView::Browse
            | WorkbenchView::Focus(Focus::Candidate { .. })
            | WorkbenchView::Edit(_) => None,
        };
        let latched_painted = self.paint_latched_saved(painter, frame, focused_saved.as_ref());
        match &self.view {
            WorkbenchView::Edit(editor) => {
                if let Some(realization) = &editor.realization {
                    map::paint_route(
                        painter,
                        realization.graph(),
                        &realization.route,
                        self.viewport,
                        rect,
                        map::SELECTED_TRAIL_COLOR,
                        self.trail_coloring,
                    );
                }
            }
            WorkbenchView::Focus(Focus::Candidate { identity }) => {
                if let Some((graph, route)) = self.sinew.as_ref().map(|sinew| &sinew.graph).zip(
                    self.candidates
                        .as_ref()
                        .and_then(|run| run.slot(*identity).and_then(|slot| run.routes.get(slot))),
                ) {
                    map::paint_route(
                        painter,
                        graph,
                        route,
                        self.viewport,
                        rect,
                        SELECTED_TRAIL_COLOR,
                        self.trail_coloring,
                    );
                }
            }
            WorkbenchView::Focus(Focus::Saved(id)) => {
                if let Some(projection) =
                    self.saved_projections.get_mut(id).and_then(Option::as_mut)
                {
                    projection
                        .overlay
                        .paint(painter, frame, self.trail_coloring);
                }
            }
            WorkbenchView::Browse => {
                let hover_painted = self
                    .hovered_saved
                    .as_ref()
                    .filter(|id| !self.latched_saved.contains(id))
                    .and_then(|id| self.saved_projections.get_mut(id))
                    .and_then(Option::as_mut)
                    .is_some_and(|projection| {
                        projection.overlay.paint_hued(
                            painter,
                            frame,
                            map::candidate_color(self.latched_saved.len(), false),
                            1.0,
                        );
                        true
                    });
                if !latched_painted
                    && !hover_painted
                    && let Some(run) = &mut self.candidates
                {
                    run.overlay.paint(painter, frame, self.trail_coloring);
                }
            }
        }
    }

    fn paint_latched_saved(
        &mut self,
        painter: &egui::Painter,
        frame: map::MapFramePlan,
        skip: Option<&TrailId>,
    ) -> bool {
        let mut any_projection = false;
        for (ordinal, id) in self.latched_saved.iter().enumerate() {
            if skip == Some(id) {
                continue;
            }
            let Some(projection) = self.saved_projections.get_mut(id).and_then(Option::as_mut)
            else {
                continue;
            };
            projection.overlay.paint_hued(
                painter,
                frame,
                map::candidate_color(ordinal, false),
                if self.hovered_saved.as_ref() == Some(id) {
                    1.0
                } else {
                    0.5
                },
            );
            any_projection = true;
        }
        any_projection
    }

    fn paint_edicts(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(sinew) = &self.sinew else {
            return;
        };
        for edge in self.edicts.required() {
            map::paint_edict(
                painter,
                &sinew.graph,
                edge,
                EdgeDisposition::Required,
                self.viewport,
                rect,
            );
        }
        for edge in self.edicts.forbidden() {
            map::paint_edict(
                painter,
                &sinew.graph,
                edge,
                EdgeDisposition::Forbidden,
                self.viewport,
                rect,
            );
        }
    }

    fn shows_search_context(&self) -> bool {
        self.creator_mode == CreatorMode::Finder
            && matches!(
                self.view,
                WorkbenchView::Browse | WorkbenchView::Focus(Focus::Candidate { .. })
            )
    }

    fn settle_map_gestures(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        gesture: MapGesture,
    ) {
        let MapGesture {
            rect,
            pointer,
            support_under_pointer,
            trailhead,
            click_modifiers,
        } = gesture;
        if let Some(pointer) = pointer
            && let Some((slot, grab)) = self
                .view
                .editor()
                .and_then(|editor| editor.drag.as_ref())
                .map(|drag| (drag.slot, drag.grab))
        {
            self.preview_editor_support(map::coord_at(self.viewport, rect, pointer - grab), slot);
        }
        if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary)) {
            self.finish_editor_drag();
        }
        if trailhead.stopped {
            self.finish_trailhead_drag(rect);
        }
        let alt_click = click_modifiers.is_some_and(|modifiers| modifiers.alt)
            && self.trailhead_input_available()
            && !trailhead.captured;
        let annotation_click = self.view.is_editing()
            && click_modifiers.is_some_and(|modifiers| modifiers.alt && !modifiers.shift);
        let excision_click =
            self.view.is_editing() && click_modifiers.is_some_and(|modifiers| modifiers.shift);
        if annotation_click {
            if let Some(slot) = support_under_pointer {
                self.toggle_support_callout(slot);
            }
        } else if alt_click && let Some(pointer) = pointer {
            self.place_trailhead(map::coord_at(self.viewport, rect, pointer), pointer);
        } else if excision_click {
            if let Some(slot) = support_under_pointer {
                self.excise_editor_support(slot);
            }
        } else if response.clicked()
            && self.view.is_editing()
            && support_under_pointer.is_none()
            && let Some(pointer) = pointer
        {
            self.place_editor_support(map::coord_at(self.viewport, rect, pointer), None, true);
        } else if response.clicked()
            && self.placing_trailhead
            && let Some(pointer) = response.interact_pointer_pos()
        {
            self.place_trailhead(map::coord_at(self.viewport, rect, pointer), pointer);
        } else if response.clicked_by(egui::PointerButton::Primary)
            && !trailhead.captured
            && !self.view.is_editing()
            && self.candidates.is_some()
            && self.shows_search_context()
            && !self.scribe.active()
            && !self.boundary_scribe.active()
            && let Some(pointer) = pointer
        {
            self.edict_segment(
                map::coord_at(self.viewport, rect, pointer),
                click_modifiers.is_some_and(|modifiers| modifiers.shift),
            );
        }
    }

    fn trailhead_input_available(&self) -> bool {
        self.creator_mode == CreatorMode::Finder
            && self.sinew.is_some()
            && !self.view.is_editing()
            && self.view.focus().is_none()
            && self.corpus.is_none()
            && !self.scribe.active()
            && !self.boundary_scribe.active()
    }

    fn interact_trailhead(&mut self, ui: &egui::Ui, rect: egui::Rect) -> TrailheadGesture {
        if !self.trailhead_input_available() {
            return TrailheadGesture::default();
        }
        let Some(coord) = self
            .trailhead_drag
            .as_ref()
            .map(|drag| drag.preview)
            .or_else(|| self.active_trailhead().map(Trailhead::coord))
        else {
            return TrailheadGesture::default();
        };
        let anchor = map::screen_at(self.viewport, rect, map::world_from_coord(coord));
        let (pointer, pressed, released, down) = ui.input(|input| {
            (
                input.pointer.interact_pos(),
                input.pointer.button_pressed(egui::PointerButton::Primary),
                input.pointer.button_released(egui::PointerButton::Primary),
                input.pointer.button_down(egui::PointerButton::Primary),
            )
        });
        let hardware = chrome::ForgePin::new(anchor).size(chrome::MechanismSize::Medium);
        let hot = pointer.is_some_and(|pointer| hardware.grip().contains(pointer));
        if pressed
            && hot
            && let Some(pointer) = pointer
        {
            self.placing_trailhead = false;
            self.trailhead_drag = Some(TrailheadDrag {
                origin: coord,
                preview: coord,
                grab: pointer - anchor,
            });
        }
        if (down || released)
            && let (Some(drag), Some(pointer)) = (&mut self.trailhead_drag, pointer)
        {
            drag.preview = map::coord_at(self.viewport, rect, pointer - drag.grab);
        }
        let seized = self.trailhead_drag.is_some();
        if seized || hot {
            ui.ctx().set_cursor_icon(if seized {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }
        TrailheadGesture {
            captured: hot || seized,
            stopped: seized && released,
        }
    }

    fn finish_trailhead_drag(&mut self, rect: egui::Rect) {
        let Some(drag) = self.trailhead_drag.take() else {
            return;
        };
        if drag.preview == drag.origin {
            return;
        }
        let pointer = map::screen_at(self.viewport, rect, map::world_from_coord(drag.preview));
        self.place_trailhead(drag.preview, pointer);
    }

    fn seize_editor_support(
        &mut self,
        pointer: Option<egui::Pos2>,
        slot: Option<usize>,
        rect: egui::Rect,
    ) {
        let (Some(pointer), Some(slot), Some(editor)) = (pointer, slot, self.view.editor_mut())
        else {
            return;
        };
        let anchor = map::screen_at(
            self.viewport,
            rect,
            map::world_from_coord(editor.support_points[slot].coord()),
        );
        editor.drag = Some(PinDrag {
            slot,
            before: editor.sketch(),
            grab: pointer - anchor,
        });
    }

    fn paint_map_header(&self, painter: &egui::Painter, rect: egui::Rect) {
        let text = if self.corpus.is_some() {
            None
        } else if self.sinew.is_none() {
            Some(self.status.to_ascii_uppercase())
        } else if self.scribe.active() {
            Some("DRAW A MAP AREA".to_owned())
        } else if self.view.is_editing() {
            Some("TRAIL EDITOR".to_owned())
        } else if self.placing_trailhead {
            Some("CLICK A TRAIL TO PLACE THE TRAILHEAD".to_owned())
        } else if let Some((name, _)) = self.focus_summary() {
            Some(name.to_ascii_uppercase())
        } else if let Some(trail) = self
            .hovered_saved
            .as_ref()
            .and_then(|id| self.library.trail(id))
        {
            Some(trail.name.to_ascii_uppercase())
        } else if self.latched_saved.len() == 1 {
            self.library
                .trail(&self.latched_saved[0])
                .map(|trail| trail.name.to_ascii_uppercase())
        } else if !self.latched_saved.is_empty() {
            Some(format!("{} SAVED TRAILS", self.latched_saved.len()))
        } else if self.candidates.is_some() {
            Some("SEARCH RESULTS".to_owned())
        } else {
            None
        };
        let Some(text) = text else { return };
        let galley = painter.layout_no_wrap(text, egui::FontId::monospace(13.0), chrome::TEXT);
        let plate = egui::Rect::from_min_size(
            rect.left_top() + vec2(12.0, 12.0),
            galley.size() + vec2(14.0, 8.0),
        );
        let _fill = painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.92));
        let _stroke = painter.rect_stroke(
            plate,
            1.0,
            Stroke::new(1.0_f32, chrome::EDGE),
            egui::StrokeKind::Inside,
        );
        painter.galley(plate.min + vec2(7.0, 4.0), galley, chrome::TEXT);
    }

    fn paint_map_footer(&self, painter: &egui::Painter, rect: egui::Rect) {
        let attribution = if self
            .vector
            .as_ref()
            .is_some_and(VectorField::has_presented_tiles)
        {
            "PROTOMAPS · © OPENSTREETMAP · "
        } else {
            ""
        };
        let footer = painter.layout_no_wrap(
            format!("{attribution}Z {:.2}", self.viewport.zoom),
            egui::FontId::monospace(9.5),
            Color32::from_black_alpha(190),
        );
        let plate = egui::Rect::from_min_size(
            rect.right_bottom() - footer.size() - vec2(16.0, 13.0),
            footer.size() + vec2(10.0, 6.0),
        );
        let _ground = painter.rect_filled(plate, 1.0, Color32::from_white_alpha(150));
        painter.galley(
            plate.min + vec2(5.0, 3.0),
            footer,
            Color32::from_black_alpha(190),
        );
    }

    fn handle_scribe(&mut self, ctx: &egui::Context, event: &ScribeEvent) {
        match event {
            ScribeEvent::None => {}
            ScribeEvent::Fault(fault) => {
                self.status.clear();
                self.status.push_str(fault);
            }
            ScribeEvent::Committed(bounds) => {
                if let Err(err) = trailgen_data::validate_region(*bounds) {
                    self.status = format!("That map area cannot be used: {err:#}");
                    self.scribe.arm();
                } else {
                    let region = SurveyRegion::new(*bounds)
                        .expect("validated bounds must forge a survey region");
                    if self.regions.iter().any(|known| known.id == region.id) {
                        "That map area is already downloaded.".clone_into(&mut self.status);
                    } else if let Err(err) =
                        self.strike_corpus(ctx, TrailDataMutation::Add(*bounds))
                    {
                        self.status = format!("Could not add that map area: {err:#}");
                        self.scribe.arm();
                    } else {
                        self.regions.push(region);
                        "Map area selected. Downloading its trails…".clone_into(&mut self.status);
                    }
                }
            }
        }
    }

    fn handle_area_resize(&mut self, ctx: &egui::Context, event: ResizeEvent) {
        match event {
            ResizeEvent::None => {}
            ResizeEvent::Fault(fault) => fault.clone_into(&mut self.status),
            ResizeEvent::Committed { id, before, bounds } => {
                let Some(slot) = self.regions.iter().position(|region| region.id == id) else {
                    "That map area no longer exists.".clone_into(&mut self.status);
                    return;
                };
                debug_assert_eq!(self.regions[slot].bounds, before);
                let replacement = match SurveyRegion::new(bounds) {
                    Ok(replacement) => replacement,
                    Err(err) => {
                        self.status = format!("That map area cannot be resized: {err:#}");
                        return;
                    }
                };
                if self
                    .regions
                    .iter()
                    .enumerate()
                    .any(|(known, region)| known != slot && region.id == replacement.id)
                {
                    "That resize would duplicate another map area.".clone_into(&mut self.status);
                    return;
                }
                if let Err(err) = self.strike_corpus(
                    ctx,
                    TrailDataMutation::Replace {
                        id: id.clone(),
                        bounds,
                    },
                ) {
                    self.status = format!("Could not resize that map area: {err:#}");
                    return;
                }
                self.regions[slot] = replacement.clone();
                if let Some(name) = self.region_names.remove(&id) {
                    let _prior = self.region_names.insert(replacement.id, name);
                }
                "Map area resized. Updating its trails…".clone_into(&mut self.status);
            }
        }
    }

    fn handle_boundary(&mut self, event: BoundaryEvent) {
        match event {
            BoundaryEvent::None => {}
            BoundaryEvent::Fault(fault) => {
                self.status = fault;
                self.boundary_scribe.arm();
            }
            BoundaryEvent::Committed(boundary) => {
                self.library.search_mut().boundary = Some(boundary);
                self.mark_library_dirty();
                self.schedule_revision();
                "Search area set. Routes will remain inside the boundary."
                    .clone_into(&mut self.status);
            }
        }
    }

    fn place_trailhead(&mut self, requested: Coord, pointer: egui::Pos2) {
        let Some(sinew) = &self.sinew else {
            "Trail network is still preparing.".clone_into(&mut self.status);
            return;
        };
        let Some(projection) = sinew.finder_index.edges().project(&sinew.graph, requested) else {
            "No downloaded trail is near that point.".clone_into(&mut self.status);
            return;
        };
        let distance_m = projection.distance_m;
        if distance_m > TRAILHEAD_SNAP_M {
            "Move closer to a downloaded trail.".clone_into(&mut self.status);
            return;
        }
        let edge = &sinew.graph.edges[projection.edge.0];
        let vertex = if projection.progress_m <= edge.attr.length_m * 0.5 {
            edge.a
        } else {
            edge.b
        };
        let coord = sinew.graph.vertices[vertex.0].coord;
        let Some(trailhead) = Trailhead::forge(coord) else {
            "That trailhead cannot be used.".clone_into(&mut self.status);
            return;
        };
        self.library.search_mut().trailhead = Some(trailhead);
        self.placing_trailhead = false;
        self.trailhead_drag = None;
        self.inscribe_library();
        self.schedule_revision();
        self.status = if distance_m < 20.0 {
            "Trailhead set.".to_owned()
        } else {
            format!("Trailhead set; snapped {distance_m:.0} m to the trail.")
        };
        self.water.click(
            chrome::ForgePin::new(pointer)
                .size(chrome::MechanismSize::Medium)
                .grip(),
        );
    }

    fn editor_support_at(&self, pointer: egui::Pos2, rect: egui::Rect) -> Option<usize> {
        self.view
            .editor()?
            .support_points
            .iter()
            .enumerate()
            .rev()
            .find_map(|(slot, support)| {
                let anchor =
                    map::screen_at(self.viewport, rect, map::world_from_coord(support.coord()));
                chrome::ForgePin::new(anchor)
                    .size(chrome::MechanismSize::Medium)
                    .grip()
                    .contains(pointer)
                    .then_some(slot)
            })
    }

    fn paint_support_points(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        legend_rect: Option<egui::Rect>,
    ) {
        let Some(editor) = self.view.editor() else {
            return;
        };
        let (excising, hover_pointer) = painter
            .ctx()
            .input(|input| (input.modifiers.shift, input.pointer.hover_pos()));
        for (slot, support) in editor.support_points.iter().enumerate() {
            let anchor =
                map::screen_at(self.viewport, rect, map::world_from_coord(support.coord()));
            let hardware = chrome::ForgePin::new(anchor).size(chrome::MechanismSize::Medium);
            if editor.coordinate_callouts[slot] {
                let coord = support.coord();
                let galley = painter.layout_no_wrap(
                    format!("{:.6}, {:.6}", coord.lat, coord.lon),
                    egui::FontId::monospace(11.0),
                    chrome::TEXT,
                );
                let size = galley.size() + vec2(10.0, 6.0);
                let top = if anchor.y - size.y - 17.0 >= rect.top() + 4.0 {
                    anchor.y - size.y - 17.0
                } else {
                    anchor.y + 17.0
                };
                let left = size
                    .x
                    .mul_add(-0.5, anchor.x)
                    .clamp(rect.left() + 4.0, rect.right() - size.x - 4.0);
                let plate = egui::Rect::from_min_size(egui::pos2(left, top), size);
                let tether = if plate.center().y < anchor.y {
                    plate.center_bottom()
                } else {
                    plate.center_top()
                };
                painter.line_segment([tether, anchor], Stroke::new(1.0_f32, chrome::EDGE_STRONG));
                let _fill = painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.96));
                let _stroke = painter.rect_stroke(
                    plate,
                    1.0,
                    Stroke::new(1.0_f32, chrome::EDGE_STRONG),
                    egui::StrokeKind::Inside,
                );
                painter.galley(plate.min + vec2(5.0, 3.0), galley, chrome::TEXT);
                #[cfg(feature = "egui-test")]
                crate::witness::rect(painter.ctx(), Target::SupportCallout(slot), plate);
            }
            let hot = hover_pointer.is_some_and(|pointer| hardware.grip().contains(pointer));
            let hardware = hardware.inscription(if excising {
                chrome::Symbol::Remove.glyph().to_string()
            } else {
                slot.to_string()
            });
            let hardware = if excising {
                hardware.inscription_size(17.0)
            } else {
                hardware
            };
            hardware.paint(
                painter,
                editor.drag.as_ref().is_some_and(|drag| drag.slot == slot) || (excising && hot),
            );
            #[cfg(feature = "egui-test")]
            crate::witness::rect(painter.ctx(), Target::Support(slot), hardware.grip());
            if let Some(fault) = editor
                .fault
                .as_ref()
                .filter(|fault| fault.support_slot == Some(slot))
            {
                let plate = paint_support_fault(painter, rect, legend_rect, anchor, &fault.message);
                #[cfg(feature = "egui-test")]
                crate::witness::rect(painter.ctx(), Target::SupportFault(slot), plate);
                #[cfg(not(feature = "egui-test"))]
                let _ = plate;
            }
        }
    }

    fn place_editor_support(&mut self, requested: Coord, slot: Option<usize>, remember: bool) {
        let Some(sinew) = &self.sinew else {
            return;
        };
        let Some(projection) = sinew.edge_index.project(&sinew.graph, requested) else {
            return;
        };
        if projection.distance_m > TRAILHEAD_SNAP_M {
            if let Some(editor) = self.view.editor_mut() {
                editor.fault = Some(EditorFault {
                    message: "Move closer to a downloaded trail.".to_owned(),
                    support_slot: slot,
                });
            }
            return;
        }
        let support = SupportPoint::forge(projection.coord)
            .expect("edge projections contain valid coordinates");
        let insertion = slot.is_none().then(|| {
            self.view
                .editor()
                .and_then(|editor| editor.realization.as_ref())
                .and_then(|realization| realization.support_insertion(requested))
                .filter(|insertion| {
                    insertion.distance_m <= map::meters_per_point(self.viewport) * 11.0
                })
                .map(|insertion| insertion.slot)
        });
        let Some(editor) = self.view.editor() else {
            return;
        };
        if slot.map_or_else(
            || editor.support_points.contains(&support),
            |slot| editor.support_points.get(slot) == Some(&support),
        ) {
            return;
        }
        if remember {
            self.remember_editor();
        }
        let editor = self.view.editor_mut().expect("editor existence checked");
        if let Some(slot) = slot {
            editor.support_points[slot] = support;
        } else {
            let slot = insertion.flatten().unwrap_or(editor.support_points.len());
            editor.support_points.insert(slot, support);
            editor.coordinate_callouts.insert(slot, false);
        }
        let _serial = self.reforge_editor();
    }

    fn preview_editor_support(&mut self, requested: Coord, slot: usize) {
        let Some(sinew) = &self.sinew else {
            return;
        };
        let Some(projection) = sinew.edge_index.project(&sinew.graph, requested) else {
            return;
        };
        if projection.distance_m > TRAILHEAD_SNAP_M {
            return;
        }
        let support = SupportPoint::forge(projection.coord)
            .expect("edge projections contain valid coordinates");
        if let Some(current) = self
            .view
            .editor_mut()
            .and_then(|editor| editor.support_points.get_mut(slot))
            && *current != support
        {
            *current = support;
        }
    }

    fn remember_editor(&mut self) {
        if let Some(editor) = self.view.editor_mut() {
            editor.checkpoint();
        }
    }

    fn excise_editor_support(&mut self, slot: usize) {
        if !self
            .view
            .editor_mut()
            .is_some_and(|editor| editor.excise_support(slot))
        {
            return;
        }
        let _serial = self.reforge_editor();
        self.status = format!("Pin {slot} deleted.");
    }

    fn toggle_support_callout(&mut self, slot: usize) {
        let Some(visible) = self
            .view
            .editor_mut()
            .and_then(|editor| editor.coordinate_callouts.get_mut(slot))
        else {
            return;
        };
        *visible = !*visible;
        self.status = format!(
            "Pin {slot} coordinates {}.",
            if *visible { "shown" } else { "hidden" }
        );
    }

    fn finish_editor_drag(&mut self) {
        if self.view.editor_mut().is_some_and(TrailEditor::finish_drag) {
            let _serial = self.reforge_editor();
        }
    }

    fn undo_editor(&mut self) {
        if self.view.editor_mut().is_some_and(TrailEditor::undo) {
            let _serial = self.reforge_editor();
        }
    }

    fn redo_editor(&mut self) {
        if self.view.editor_mut().is_some_and(TrailEditor::redo) {
            let _serial = self.reforge_editor();
        }
    }

    fn reverse_editor(&mut self) {
        let Some(sinew) = &self.sinew else {
            return;
        };
        let reversed = self
            .view
            .editor()
            .and_then(|editor| editor.realization.as_ref())
            .context("reverse direction requires a realized loop")
            .and_then(|realization| realization.reverse_loop(&sinew.graph).map_err(Into::into));
        match reversed {
            Ok(reversal) => {
                self.remember_editor();
                let editor = self.view.editor_mut().expect("editor existence checked");
                editor.shape = reversal.trail.shape;
                editor.replace_supports(reversal.trail.support_points);
                let _serial = self.reforge_editor();
                let notice = match reversal.added_supports {
                    0 => "Loop direction reversed.".to_owned(),
                    1 => "Loop direction reversed; added 1 pin to preserve the exact route."
                        .to_owned(),
                    count => format!(
                        "Loop direction reversed; added {count} pins to preserve the exact route."
                    ),
                };
                if reversal.added_supports > 0 {
                    self.view
                        .editor_mut()
                        .expect("editor existence checked")
                        .notice = Some(notice.clone());
                }
                self.status = notice;
            }
            Err(err) => {
                let notice = format!("Could not reverse this loop: {err:#}");
                if let Some(editor) = self.view.editor_mut() {
                    editor.notice = Some(notice.clone());
                }
                self.status = notice;
            }
        }
    }

    fn close_editor_loop(&mut self) {
        let Some(editor) = self.view.editor() else {
            return;
        };
        if editor.support_points.len() < 2 {
            self.view
                .editor_mut()
                .expect("editor existence checked")
                .notice = Some("Add another pin before closing the loop.".to_owned());
            return;
        }
        let previous = editor.shape;
        self.remember_editor();
        self.view
            .editor_mut()
            .expect("editor existence checked")
            .shape = RouteShape::Loop;
        if let Some(serial) = self.reforge_editor() {
            self.view
                .editor_mut()
                .expect("editor existence checked")
                .shape_guard = Some((serial, previous));
            "Closing loop…".clone_into(&mut self.status);
        }
    }

    fn schedule_revision(&mut self) {
        if self.candidates.is_none() {
            return;
        }
        if matches!(self.view, WorkbenchView::Focus(Focus::Candidate { .. })) {
            self.leave_focus();
        }
        self.search_due = Some(Instant::now() + SEARCH_SETTLE);
        if self.forge_phase.active() {
            self.stop_search();
        }
    }

    fn edict_segment(&mut self, requested: Coord, forbidden: bool) {
        let Some(sinew) = &self.sinew else {
            return;
        };
        let Some(projection) = sinew.finder_index.edges().project(&sinew.graph, requested) else {
            return;
        };
        if projection.distance_m > map::meters_per_point(self.viewport) * 11.0 {
            "Click closer to a trail segment.".clone_into(&mut self.status);
            return;
        }
        let before = self.edicts.clone();
        if forbidden {
            self.edicts.toggle_forbidden(projection.edge);
        } else {
            self.edicts.toggle_required(projection.edge);
        }
        self.edict_history.commit(before);
        let disposition = self.edicts.disposition(projection.edge);
        self.dissolve_focus();
        match disposition {
            EdgeDisposition::Required => "Segment required. Revising trails…",
            EdgeDisposition::Forbidden => "Segment excluded. Revising trails…",
            EdgeDisposition::Free => "Segment rule removed. Revising trails…",
        }
        .clone_into(&mut self.status);
        self.schedule_revision();
    }

    fn undo_edict(&mut self) {
        let Some(edicts) = self.edict_history.undo(self.edicts.clone()) else {
            return;
        };
        self.edicts = edicts;
        "Segment rule undone. Revising trails…".clone_into(&mut self.status);
        self.schedule_revision();
    }

    fn redo_edict(&mut self) {
        let Some(edicts) = self.edict_history.redo(self.edicts.clone()) else {
            return;
        };
        self.edicts = edicts;
        "Segment rule restored. Revising trails…".clone_into(&mut self.status);
        self.schedule_revision();
    }

    fn strike(&mut self) {
        self.search_due = None;
        self.serial = self.serial.saturating_add(1);
        self.editor_serial = self.editor_serial.saturating_add(1);
        let launch = self.search_request(self.serial).and_then(|request| {
            let sinew = self
                .sinew
                .as_ref()
                .context("trail network is still preparing")?;
            let progress = if request.boundary.is_some() {
                SearchProgress {
                    stage: SearchStage::Preparing,
                    explored: 0,
                    limit: sinew.graph.edges.len(),
                    candidates: 0,
                }
            } else {
                SearchProgress {
                    stage: SearchStage::Exploring,
                    explored: 0,
                    limit: request.params.max_frontier,
                    candidates: 0,
                }
            };
            sinew.forge.strike(request).map(|handle| (handle, progress))
        });
        match launch {
            Ok((handle, progress)) => {
                self.results_open = true;
                self.forge_phase = ForgePhase::Striking {
                    serial: self.serial,
                    handle,
                    progress,
                    stopping: false,
                };
                self.status = search_progress_text(progress);
            }
            Err(err) => self.status = format!("Could not start this search: {err:#}"),
        }
    }

    fn stop_search(&mut self) {
        let ForgePhase::Striking {
            handle, stopping, ..
        } = &mut self.forge_phase
        else {
            return;
        };
        handle.stop();
        *stopping = true;
        "Stopping trail search…".clone_into(&mut self.status);
    }

    fn search_request(&self, serial: u64) -> Result<SearchRequest> {
        let sinew = self
            .sinew
            .as_ref()
            .context("trail network is still preparing")?;
        let recipe = self.library.search();
        let trailhead = recipe.trailhead.context("place a trailhead on the map")?;
        let start = sinew
            .finder_index
            .edges()
            .project(&sinew.graph, trailhead.coord())
            .map(|projection| {
                let edge = &sinew.graph.edges[projection.edge.0];
                if projection.progress_m <= edge.attr.length_m * 0.5 {
                    edge.a
                } else {
                    edge.b
                }
            })
            .context("no downloaded trail is near this trailhead")?;
        let mut params = self.params;
        params.keep = params.keep.max(CANDIDATE_COUNT);
        let warmth = self
            .candidates
            .as_ref()
            .map_or_else(CandidateWarmth::default, CandidatePortfolio::warmth);
        if !warmth.routes().is_empty() {
            params.seed = params
                .seed
                .wrapping_add(serial.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        }
        let mut constraints = recipe.constraints(&self.defaults)?;
        let pace = self.base_pace;
        constraints.min_moving_time_s = pace.population_time_s(constraints.min_moving_time_s);
        constraints.max_moving_time_s = pace.population_time_s(constraints.max_moving_time_s);
        Ok(SearchRequest {
            serial,
            start,
            boundary: recipe.boundary.clone(),
            constraints,
            params,
            solver: self.solver,
            count: CANDIDATE_COUNT,
            manual_defaults: self.defaults.clone(),
            edicts: self.edicts.clone(),
            warmth,
        })
    }

    fn absorb_events(&mut self, ctx: &egui::Context, drain: &mut Drain) {
        let events = self.take_search_events(ctx, drain);
        for event in events {
            match event {
                SearchEvent::Progress { serial, progress }
                    if self.forge_phase.serial() == Some(serial) =>
                {
                    if let ForgePhase::Striking {
                        progress: current,
                        stopping,
                        ..
                    } = &mut self.forge_phase
                    {
                        *current = progress;
                        if !*stopping {
                            self.status = search_progress_text(progress);
                        }
                    }
                }
                SearchEvent::Preview {
                    serial,
                    portfolio,
                    elapsed,
                } if self.forge_phase.serial() == Some(serial) => {
                    let count = portfolio.routes.len();
                    self.results_open = true;
                    self.candidates = Some(*portfolio);
                    self.reconcile_candidate_focus();
                    self.status = format!(
                        "Found {count} trail(s) in {}. Still searching…",
                        duration(elapsed)
                    );
                    ctx.request_repaint();
                }
                SearchEvent::Found {
                    serial,
                    portfolio,
                    elapsed,
                } if self.forge_phase.serial() == Some(serial) => {
                    self.forge_phase = ForgePhase::Idle;
                    let count = portfolio.routes.len();
                    self.results_open = true;
                    self.candidates = Some(*portfolio);
                    self.reconcile_candidate_focus();
                    self.status = if count == 0 {
                        format!("No trails matched in {}.", duration(elapsed))
                    } else {
                        format!("Found {count} trail(s) in {}.", duration(elapsed))
                    };
                    ctx.request_repaint();
                }
                SearchEvent::PreparingResults {
                    serial,
                    count,
                    elapsed,
                } if self.forge_phase.serial() == Some(serial) => {
                    if let ForgePhase::Striking {
                        progress, stopping, ..
                    } = &mut self.forge_phase
                    {
                        *progress = SearchProgress {
                            stage: SearchStage::Ranking,
                            explored: count,
                            limit: count,
                            candidates: count,
                        };
                        if !*stopping {
                            self.status = format!(
                                "Found {count} trail(s) in {}. Preparing display…",
                                duration(elapsed)
                            );
                        }
                    }
                    ctx.request_repaint();
                }
                SearchEvent::Stopped { serial, elapsed }
                    if self.forge_phase.serial() == Some(serial) =>
                {
                    self.forge_phase = ForgePhase::Idle;
                    self.status = format!(
                        "Search stopped after {}. Previous results are unchanged.",
                        duration(elapsed)
                    );
                    ctx.request_repaint();
                }
                SearchEvent::Progress { .. }
                | SearchEvent::Preview { .. }
                | SearchEvent::PreparingResults { .. }
                | SearchEvent::Found { .. }
                | SearchEvent::Stopped { .. } => {}
            }
        }
        if let Some(vector) = &mut self.vector {
            vector.absorb(ctx);
        }
        self.relief.absorb();
    }

    fn take_search_events(&self, ctx: &egui::Context, drain: &mut Drain) -> Vec<SearchEvent> {
        let Some(sinew) = &self.sinew else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Some(event) = drain.receive(&sinew.forge.events) {
            events.push(event);
        }
        if !sinew.forge.events.is_empty() {
            ctx.request_repaint();
        }
        events
    }

    fn absorb_editor_events(&mut self, ctx: &egui::Context, drain: &mut Drain) {
        let mut events = Vec::new();
        if let Some(sinew) = &self.sinew {
            while let Some(event) = drain.receive(&sinew.editor_forge.events) {
                events.push(event);
            }
            if !sinew.editor_forge.events.is_empty() {
                ctx.request_repaint();
            }
        }
        for event in events {
            let mut status = None;
            if let Some(editor) = self.view.editor_mut()
                && editor.realizing == Some(event.serial)
            {
                if let Some((serial, previous)) = editor.shape_guard
                    && serial == event.serial
                {
                    match event.result {
                        Ok(realization) => {
                            editor.shape_guard = None;
                            editor.absorb_realization(Ok(realization));
                            status = Some("Loop closed.".to_owned());
                        }
                        Err(error) => {
                            editor.shape = previous;
                            editor.shape_guard = None;
                            editor.realizing = None;
                            let notice = editor.reject_loop_closure(&error);
                            editor.fault = None;
                            status = Some(notice);
                        }
                    }
                } else {
                    editor.absorb_realization(event.result);
                }
            }
            if let Some(status) = status {
                self.status = status;
            }
        }
    }

    fn reconcile_candidate_focus(&mut self) {
        let alive = |identity| {
            self.candidates
                .as_ref()
                .is_some_and(|run| run.slot(identity).is_some())
        };
        let focused_missing = matches!(
            self.view.focus(),
            Some(Focus::Candidate { identity }) if !alive(*identity)
        );
        if focused_missing {
            self.leave_focus();
            return;
        }
        let returning_missing = self
            .view
            .editor()
            .and_then(|editor| editor.return_to.focus.as_ref())
            .is_some_and(
                |focus| matches!(focus, Focus::Candidate { identity } if !alive(*identity)),
            );
        if returning_missing {
            let viewport = self.focus_frame.pop();
            if let Some(editor) = self.view.editor_mut() {
                editor.return_to.focus = None;
                if let Some(viewport) = viewport {
                    editor.return_to.viewport = viewport;
                }
            }
        }
    }

    fn strike_corpus(&mut self, ctx: &egui::Context, mutation: TrailDataMutation) -> Result<()> {
        anyhow::ensure!(self.corpus.is_none(), "trail update already running");
        self.corpus = Some(CorpusTask::Acquiring(TrailData::spawn(
            ctx,
            self.root.clone(),
            mutation,
        )?));
        self.trail_data_status = Some("Updating trails…".to_owned());
        Ok(())
    }

    fn absorb_corpus(&mut self, ctx: &egui::Context, drain: &mut Drain) {
        let Some(task) = &self.corpus else {
            return;
        };
        if let CorpusTask::Preparing(forge) = task {
            let Some(result) = drain.receive(&forge.event) else {
                return;
            };
            self.corpus = None;
            let initial = self.sinew.is_none();
            match result.and_then(|armament| self.install_armament(ctx, armament)) {
                Ok(()) => {}
                Err(err) => {
                    if initial {
                        self.status = format!("Could not prepare trail network: {err:#}");
                        self.trail_data_status = Some("Trail preparation failed.".to_owned());
                    } else {
                        self.status = format!("Could not install updated trail data: {err:#}");
                        self.trail_data_status = Some("Trail update failed.".to_owned());
                    }
                }
            }
            return;
        }
        let CorpusTask::Acquiring(corpus) = task else {
            unreachable!("corpus task was exhaustively matched");
        };
        let mut events = Vec::new();
        while let Some(event) = drain.receive(&corpus.events) {
            events.push(event);
        }
        if !corpus.events.is_empty() {
            ctx.request_repaint();
        }
        for event in events {
            match event {
                TrailDataEvent::Progress(event) => {
                    self.trail_data_status = Some(progress_status(&event));
                }
                TrailDataEvent::Ready(Some(summary)) => {
                    self.regions = summary.regions;
                    self.trail_data_status = Some("Preparing the updated trail map…".to_owned());
                    match CorpusForge::spawn(ctx, self.root.clone(), false) {
                        Ok(forge) => self.corpus = Some(CorpusTask::Preparing(forge)),
                        Err(err) => {
                            self.corpus = None;
                            self.status = format!("Could not prepare updated trail data: {err:#}");
                            self.trail_data_status = Some("Trail update failed.".to_owned());
                        }
                    }
                    return;
                }
                TrailDataEvent::Ready(None) => {
                    self.regions.clear();
                    self.region_names.clear();
                    self.trail_data_status = Some("No map areas downloaded.".to_owned());
                    self.workspace_signal = Some(Action::Reload);
                    self.corpus = None;
                    self.inscribe_library();
                    return;
                }
                TrailDataEvent::Fault(fault) => {
                    self.status = format!("Trail update failed: {fault}");
                    self.trail_data_status = Some("Trail update failed.".to_owned());
                    if let Ok(config) = trailgen_data::project_config(&self.root) {
                        self.regions = config.regions;
                        self.region_names = config.region_names;
                    }
                    self.corpus = None;
                    return;
                }
            }
        }
    }

    fn install_armament(&mut self, ctx: &egui::Context, armament: CorpusArmament) -> Result<()> {
        let initial = self.sinew.is_none();
        let restoring_manual = initial
            && self
                .view
                .editor()
                .is_some_and(|editor| matches!(editor.origin, EditorOrigin::New));
        anyhow::ensure!(
            !self.view.is_editing() || restoring_manual,
            "finish the active trail edit before installing new trail data"
        );
        if !initial {
            self.relief.retarget(ctx, &self.root)?;
        }
        let vector_retired = if let Some(vector) = &mut self.vector {
            Some(if initial {
                vector.bind_trails(
                    ctx,
                    Arc::clone(&armament.sinew.graph),
                    Arc::clone(&armament.sinew.edge_index),
                )?
            } else {
                vector.retarget(
                    ctx,
                    armament.source,
                    self.offline,
                    Arc::clone(&armament.sinew.graph),
                    Arc::clone(&armament.sinew.edge_index),
                )?
            })
        } else {
            self.vector = Some(VectorField::raise(
                ctx,
                armament.source,
                self.offline,
                Some((
                    Arc::clone(&armament.sinew.graph),
                    Arc::clone(&armament.sinew.edge_index),
                )),
            )?);
            None
        };
        if !initial {
            if matches!(self.view.focus(), Some(Focus::Candidate { .. })) {
                self.dissolve_focus();
            }
            self.serial = self.serial.saturating_add(1);
            self.search_due = None;
            self.results_open = false;
            self.edicts.clear();
            self.edict_history.clear();
        }
        let retired = (
            self.sinew.replace(armament.sinew),
            std::mem::take(&mut self.candidates),
            std::mem::take(&mut self.forge_phase),
            vector_retired,
        );
        let _reaper = thread::Builder::new()
            .name("trail-corpus-reaper".to_owned())
            .spawn(move || drop(retired));
        if !initial {
            self.regions = armament.regions;
            self.region_names = armament.region_names;
        }
        if self.library.legacy_routes_pending()
            && self.library.absorb_legacy_routes(armament.legacy_routes)
        {
            self.reconcile_saved_projections();
        }
        self.profile_cursor.bind(self.profile_owner());
        self.trail_data_status = Some(format!(
            "Trail data ready in {} map area(s).",
            self.regions.len()
        ));
        if restoring_manual {
            let supports = self
                .view
                .editor()
                .map_or(0, |editor| editor.support_points.len());
            let _serial = self.reforge_editor();
            self.status = match supports {
                1 => "Restored an unfinished manual trail with 1 pin.".to_owned(),
                count => format!("Restored an unfinished manual trail with {count} pins."),
            };
        } else if initial {
            "Trail network ready. Select Manual to draw or Finder to search."
                .clone_into(&mut self.status);
        } else {
            "Updated trails are ready.".clone_into(&mut self.status);
        }
        self.inscribe_library();
        Ok(())
    }

    fn tend_post_armament(&mut self, ctx: &egui::Context) {
        if self.sinew.is_none() || self.corpus.is_some() || self.view.is_editing() {
            return;
        }
        let Some(mutation) = self.post_armament.take() else {
            return;
        };
        if let Err(err) = self.strike_corpus(ctx, mutation) {
            self.status = format!("Could not refresh trail data: {err:#}");
            self.trail_data_status = Some("Trail refresh failed.".to_owned());
        }
    }

    fn active_trailhead(&self) -> Option<Trailhead> {
        (self.creator_mode == CreatorMode::Finder && self.shows_search_context())
            .then_some(self.library.search().trailhead)
            .flatten()
    }

    fn save_focused_candidate(&mut self) {
        let Some(Focus::Candidate { identity }) = self.view.focus().cloned() else {
            return;
        };
        let Some(slot) = self.candidates.as_ref().and_then(|run| run.slot(identity)) else {
            return;
        };
        let Some(route) = self
            .candidates
            .as_ref()
            .and_then(|run| run.routes.get(slot))
            .cloned()
        else {
            return;
        };
        let design = self
            .candidates
            .as_ref()
            .and_then(|run| run.designs.get(slot))
            .map(|design| design.as_ref().clone());
        let Some(graph) = self.sinew.as_ref().map(|sinew| &sinew.graph) else {
            return;
        };
        let result = if let Some(design) = design {
            self.library.promote_design(graph, &route, &design)
        } else {
            self.library.promote(graph, &route)
        };
        match result {
            Ok(id) => {
                self.enter_focus(Focus::Saved(id));
                self.reconcile_saved_projections();
                self.inscribe_library();
                "Trail saved to the project.".clone_into(&mut self.status);
            }
            Err(err) => self.status = format!("Could not save this trail: {err:#}"),
        }
    }

    fn focus_design(&self) -> Option<Trail> {
        match self.view.focus() {
            Some(Focus::Candidate { identity }) => self
                .candidates
                .as_ref()
                .and_then(|run| run.slot(*identity).and_then(|slot| run.designs.get(slot)))
                .map(|design| design.as_ref().clone()),
            Some(Focus::Saved(id)) => self.library.trail(id).and_then(SavedTrail::design),
            None => None,
        }
    }

    fn edit_focus(&mut self) {
        let Some(trail) = self.focus_design() else {
            "This trail has no canonical support-point design.".clone_into(&mut self.status);
            return;
        };
        let Some((name, _)) = self.focus_summary() else {
            return;
        };
        let origin = match self.view.focus() {
            Some(Focus::Candidate { .. }) => EditorOrigin::Candidate,
            Some(Focus::Saved(id)) => EditorOrigin::Saved(id.clone()),
            None => return,
        };
        self.begin_editor(origin, Some((name, trail)));
    }

    fn begin_editor(&mut self, origin: EditorOrigin, seed: Option<(String, Trail)>) {
        let return_focus = match std::mem::replace(&mut self.view, WorkbenchView::Browse) {
            WorkbenchView::Browse => None,
            WorkbenchView::Focus(focus) => Some(focus),
            WorkbenchView::Edit(editor) => {
                self.view = WorkbenchView::Edit(editor);
                return;
            }
        };
        let (name, shape, support_points) = seed.map_or_else(
            || ("manual trail".to_owned(), RouteShape::Open, Vec::new()),
            |(name, trail)| (name, trail.shape, trail.support_points),
        );
        self.scribe.disarm();
        self.boundary_scribe.disarm();
        self.placing_trailhead = false;
        self.trailhead_drag = None;
        self.delete_confirmation = None;
        self.fit = Fit::None;
        self.view = WorkbenchView::Edit(Box::new(TrailEditor::forge(
            name,
            origin,
            EditorReturn {
                focus: return_focus,
                viewport: self.viewport,
            },
            shape,
            support_points,
        )));
        let _serial = self.reforge_editor();
        "Trail editor ready. Place support points on the map.".clone_into(&mut self.status);
    }

    fn manual_constraints(&self, shape: RouteShape) -> LoopConstraints {
        portfolio::manual_constraints(&self.defaults, shape)
    }

    fn reforge_editor(&mut self) -> Option<u64> {
        let editor = self.view.editor()?;
        self.editor_serial = self.editor_serial.saturating_add(1);
        let serial = self.editor_serial;
        let name = editor.name.clone();
        let shape = editor.shape;
        let support_points = editor.support_points.clone();
        if support_points.len() < 2 {
            if let Some(editor) = self.view.editor_mut() {
                editor.realization = None;
                editor.realizing = None;
                editor.shape_guard = None;
                editor.profile = None;
                editor.fault = None;
                editor.notice = None;
            }
            return None;
        }
        let job = EditorJob {
            serial,
            name,
            shape,
            support_points,
            routing: self.params.routing,
            constraints: self.manual_constraints(shape),
        };
        let launched = self
            .sinew
            .as_ref()
            .context("trail network is still preparing")
            .and_then(|sinew| sinew.editor_forge.strike(job));
        let accepted = launched.is_ok();
        if let Some(editor) = self.view.editor_mut() {
            editor.shape_guard = None;
            if accepted {
                editor.realizing = Some(serial);
                editor.fault = None;
                editor.notice = None;
            } else {
                editor.realizing = None;
                editor.fault = Some(EditorFault {
                    message: "Trailgen could not update this route. Try again.".to_owned(),
                    support_slot: None,
                });
            }
        }
        accepted.then_some(serial)
    }

    fn save_editor(&mut self) {
        if let Some(draft) = self
            .view
            .editor()
            .and_then(|editor| editor.name_draft.as_ref())
        {
            if !trail_name_is_valid(&draft.text) {
                "Give this trail a name before saving.".clone_into(&mut self.status);
                return;
            }
            self.enact_editor_name(Some(&EditorNameAction::Commit));
        }
        let Some(editor) = self.view.editor().filter(|editor| editor.ready()) else {
            return;
        };
        let Some(realization) = editor.realization.clone() else {
            return;
        };
        let origin = editor.origin.clone();
        let had_focus = editor.return_to.focus.is_some();
        let return_viewport = editor.return_to.viewport;
        let result = match &origin {
            EditorOrigin::New | EditorOrigin::Candidate => {
                self.library.promote_realization(&realization)
            }
            EditorOrigin::Saved(id) => self.library.replace_realization(id, &realization),
        };
        match result {
            Ok(id) => {
                if !had_focus {
                    self.focus_frame.push(return_viewport);
                }
                self.view = WorkbenchView::Focus(Focus::Saved(id.clone()));
                self.delete_confirmation = None;
                self.fit = Fit::Saved(id);
                self.reconcile_saved_projections();
                self.inscribe_library();
                "Trail saved.".clone_into(&mut self.status);
            }
            Err(err) => self.status = format!("Could not save this trail: {err:#}"),
        }
    }

    fn discard_editor(&mut self) {
        let editor = match std::mem::replace(&mut self.view, WorkbenchView::Browse) {
            WorkbenchView::Edit(editor) => editor,
            other => {
                self.view = other;
                return;
            }
        };
        self.viewport = editor.return_to.viewport;
        self.view = editor
            .return_to
            .focus
            .map_or(WorkbenchView::Browse, WorkbenchView::Focus);
        self.fit = Fit::None;
        "Trail edit discarded.".clone_into(&mut self.status);
    }

    fn delete_focused_trail(&mut self) {
        let Some(Focus::Saved(id)) = self.view.focus().cloned() else {
            return;
        };
        if self.library.remove_trail(&id) {
            self.saved_projections.remove(&id);
            self.latched_saved.retain(|latched| latched != &id);
            self.rename = None;
            self.delete_confirmation = None;
            self.leave_focus();
            self.inscribe_library();
            "Trail deleted from the project.".clone_into(&mut self.status);
        }
    }

    fn focus_summary(&self) -> Option<(String, RouteMetrics)> {
        match self.view.focus() {
            Some(Focus::Candidate { identity }) => self
                .candidates
                .as_ref()
                .and_then(|run| run.slot(*identity).and_then(|slot| run.routes.get(slot)))
                .map(|route| (route.name.clone(), route.metrics.clone())),
            Some(Focus::Saved(id)) => self
                .library
                .trail(id)
                .map(|trail| (trail.name.clone(), trail.metrics.clone())),
            None => None,
        }
    }

    fn focus_standing(&self) -> Option<TrailStanding> {
        match self.view.focus() {
            Some(Focus::Candidate { identity }) => self
                .candidates
                .as_ref()
                .and_then(|run| run.slot(*identity).and_then(|slot| run.routes.get(slot)))
                .and_then(|route| {
                    let graph = &self.sinew.as_ref()?.graph;
                    map::frailest_standing(
                        route
                            .edges
                            .iter()
                            .map(|edge| graph.edges[edge.0].attr.standing),
                    )
                }),
            Some(Focus::Saved(id)) => self.library.trail(id).and_then(|trail| {
                map::frailest_standing(trail.legs.iter().map(|leg| leg.standing))
            }),
            None => None,
        }
    }

    fn focus_count(&self) -> usize {
        match self.view.focus() {
            Some(Focus::Candidate { .. }) => {
                self.candidates.as_ref().map_or(0, |run| run.routes.len())
            }
            Some(Focus::Saved(_)) => self.library.trails().len(),
            None => 0,
        }
    }

    fn has_profile(&self) -> bool {
        match self.view.focus() {
            Some(Focus::Candidate { identity }) => self
                .candidates
                .as_ref()
                .and_then(|run| run.slot(*identity).and_then(|slot| run.profiles.get(slot)))
                .is_some_and(Option::is_some),
            Some(Focus::Saved(id)) => self
                .saved_projections
                .get(id)
                .and_then(Option::as_ref)
                .is_some_and(|projection| projection.profile.is_some()),
            None => false,
        }
    }

    fn step_focus(&mut self, delta: isize) {
        let next = match self.view.focus().cloned() {
            Some(Focus::Candidate { identity }) => {
                let Some(run) = &self.candidates else {
                    return;
                };
                let Some(slot) = run.slot(identity) else {
                    return;
                };
                let order = gallery::order_candidates(&run.routes, self.sort);
                let Some(next) = cyclic_step(&order, slot, delta) else {
                    return;
                };
                Focus::Candidate {
                    identity: run.identities[next],
                }
            }
            Some(Focus::Saved(id)) => {
                let trails = self.library.trails().iter().collect::<Vec<_>>();
                let order = gallery::order_saved(&trails, self.sort);
                let ids = order
                    .into_iter()
                    .map(|slot| trails[slot].id.clone())
                    .collect::<Vec<_>>();
                let Some(current) = ids.iter().position(|known| known == &id) else {
                    return;
                };
                let next = (current.cast_signed() + delta)
                    .rem_euclid(ids.len().cast_signed())
                    .cast_unsigned();
                Focus::Saved(ids[next].clone())
            }
            None => return,
        };
        self.fit = match &next {
            Focus::Candidate { identity } => Fit::Candidate {
                identity: *identity,
            },
            Focus::Saved(id) => Fit::Saved(id.clone()),
        };
        self.delete_confirmation = None;
        self.view = WorkbenchView::Focus(next);
    }

    fn enter_focus(&mut self, focus: Focus) {
        self.delete_confirmation = None;
        self.focus_frame.push(self.viewport);
        self.fit = match &focus {
            Focus::Candidate { identity } => Fit::Candidate {
                identity: *identity,
            },
            Focus::Saved(id) => Fit::Saved(id.clone()),
        };
        self.view = WorkbenchView::Focus(focus);
    }

    fn leave_focus(&mut self) {
        if !matches!(self.view, WorkbenchView::Focus(_)) {
            return;
        }
        self.view = WorkbenchView::Browse;
        self.delete_confirmation = None;
        if let Some(viewport) = self.focus_frame.pop() {
            self.viewport = viewport;
        }
        self.fit = Fit::None;
    }

    fn dissolve_focus(&mut self) {
        if matches!(self.view, WorkbenchView::Focus(_)) {
            self.view = WorkbenchView::Browse;
            self.delete_confirmation = None;
            self.focus_frame = FocusFrame::default();
            self.fit = Fit::None;
        }
    }

    fn apply_fit(&mut self, rect: egui::Rect) {
        let viewport = match &self.fit {
            Fit::Graph => self
                .sinew
                .as_ref()
                .map(|sinew| Viewport::fit_graph(&sinew.graph, rect)),
            Fit::Regions => Some(map::fit_coords(
                self.regions.iter().flat_map(|region| {
                    let bounds = region.bounds;
                    [
                        Coord::new(bounds.west, bounds.south),
                        Coord::new(bounds.east, bounds.north),
                    ]
                }),
                rect,
            )),
            Fit::Candidate { identity } => self
                .candidates
                .as_ref()
                .and_then(|run| run.slot(*identity).and_then(|slot| run.routes.get(slot)))
                .and_then(|route| {
                    self.sinew
                        .as_ref()
                        .map(|sinew| Viewport::fit_route(&sinew.graph, route, rect))
                }),
            Fit::Saved(id) => self
                .library
                .trail(id)
                .map(|trail| Viewport::fit_saved(trail, rect)),
            Fit::Civic(key) => self
                .civic
                .area(key)
                .map(|area| map::fit_coords(area.bounds.fit_points().into_iter(), rect)),
            Fit::None => None,
        };
        if let Some(viewport) = viewport {
            self.viewport = viewport;
            self.fit = Fit::None;
        }
    }

    fn take_keys(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() || ctx.memory(|memory| memory.top_modal_layer().is_some()) {
            return;
        }
        let widget_focused = ctx.memory(|memory| memory.focused().is_some());
        let find = !widget_focused
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        if find {
            let search_open = self.shutters.get("search").copied().unwrap_or(true);
            if search_open
                && self.creator_mode == CreatorMode::Finder
                && self.sinew.is_some()
                && !self.view.is_editing()
                && !self.forge_phase.active()
            {
                self.strike();
            }
            return;
        }
        let escape =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if escape && self.take_escape() {
            return;
        }
        if self.view.focus().is_none() {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)) {
            self.step_focus(-1);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)) {
            self.step_focus(1);
        }
    }

    fn take_escape(&mut self) -> bool {
        if self.delete_confirmation.take().is_some() {
            "Trail kept.".clone_into(&mut self.status);
        } else if self.forge_phase.active() {
            self.stop_search();
        } else if self.scribe.active() {
            self.scribe.disarm();
        } else if self.area_handles.captured() {
            self.area_handles.cancel();
        } else if self.boundary_scribe.active() {
            self.boundary_scribe.disarm();
        } else if self.trailhead_drag.is_some() {
            self.trailhead_drag = None;
        } else if self.placing_trailhead {
            self.placing_trailhead = false;
        } else if self.view.focus().is_some() {
            self.leave_focus();
        } else {
            return false;
        }
        true
    }

    fn mark_library_dirty(&mut self) {
        self.dirty_state.library = true;
        self.state_scribe.mark();
    }

    fn reconcile_saved_projections(&mut self) {
        self.reconcile_latched_saved();
        self.saved_projections
            .retain(|id, _| self.library.trail(id).is_some());
        let missing = self
            .library
            .trails()
            .iter()
            .filter(|trail| !self.saved_projections.contains_key(&trail.id))
            .cloned()
            .collect::<Vec<_>>();
        for trail in missing {
            let id = trail.id.clone();
            if self.projection_forge.strike(id.clone(), trail) {
                let prior = self.saved_projections.insert(id, None);
                debug_assert!(prior.is_none());
            }
        }
    }

    fn reconcile_latched_saved(&mut self) {
        self.latched_saved
            .retain(|id| self.library.trail(id).is_some());
    }

    fn set_saved_latch(&mut self, id: TrailId, latched: bool) {
        self.latched_saved.retain(|candidate| candidate != &id);
        if latched && self.library.trail(&id).is_some() {
            self.latched_saved.push(id);
        }
    }

    fn absorb_saved_projections(&mut self, ctx: &egui::Context, drain: &mut Drain) {
        while let Some((id, projection)) = drain.receive(&self.projection_forge.events) {
            if self.library.trail(&id).is_some() {
                let _pending = self.saved_projections.insert(id, Some(projection));
            }
        }
        if !self.projection_forge.events.is_empty() {
            ctx.request_repaint();
        }
    }

    fn begin_export(&mut self, id: &TrailId) {
        let Some(trail) = self.library.trail(id).cloned() else {
            return;
        };
        let suggested = suggested_filename(&trail.name);
        #[cfg(feature = "egui-test")]
        let destination = std::env::var_os("TRAILGEN_ACCEPTANCE_EXPORT_PATH")
            .map(PathBuf::from)
            .or_else(|| {
                rfd::FileDialog::new()
                    .set_title("Export saved trail")
                    .add_filter("GPS Exchange Format", &["gpx"])
                    .set_file_name(&suggested)
                    .save_file()
            });
        #[cfg(not(feature = "egui-test"))]
        let destination = rfd::FileDialog::new()
            .set_title("Export saved trail")
            .add_filter("GPS Exchange Format", &["gpx"])
            .set_file_name(&suggested)
            .save_file();
        let Some(destination) = destination else {
            return;
        };
        self.last_exported = None;
        self.status = format!("Exporting {}…", trail.name);
        if let Err(error) = self.export_forge.strike(ExportJob { trail, destination }) {
            self.status = format!("Could not export that trail: {error:#}");
        }
    }

    fn absorb_export_events(&mut self, ctx: &egui::Context, drain: &mut Drain) {
        while let Some(event) = drain.receive(self.export_forge.events()) {
            match event {
                ExportEvent::Written { id, destination } => {
                    self.last_exported = Some(id);
                    self.status = format!("Trail exported to {}.", destination.display());
                }
                ExportEvent::Fault(fault) => {
                    self.last_exported = None;
                    self.status = format!("Could not export that trail: {fault}");
                }
            }
        }
        if !self.export_forge.events().is_empty() {
            ctx.request_repaint();
        }
    }

    fn inscribe_library(&mut self) {
        self.dirty_state.library = true;
        let state = self.durable_state();
        match self.state_scribe.submit(state.clone()) {
            Ok(sequence) => self.accept_submission(sequence, state),
            Err(error) => {
                self.status = format!("Could not submit the trail library: {error:#}");
            }
        }
    }

    fn snapshot(&self) -> Slate {
        let viewport = match &self.view {
            WorkbenchView::Edit(editor) if editor.return_to.focus.is_none() => {
                editor.return_to.viewport
            }
            WorkbenchView::Browse | WorkbenchView::Focus(_) | WorkbenchView::Edit(_) => {
                self.focus_frame.base(self.viewport)
            }
        };
        let manual_draft = self.view.editor().and_then(|editor| {
            if !matches!(editor.origin, EditorOrigin::New) {
                return None;
            }
            let sketch = editor.durable_sketch();
            (!sketch.support_points.is_empty()).then(|| ManualDraft {
                name: editor.name.clone(),
                shape: sketch.shape,
                support_points: sketch.support_points,
                viewport: self.viewport,
            })
        });
        Slate {
            project: self.root.clone(),
            viewport: Some(viewport),
            manual_draft,
            shutters: self.shutters.clone(),
            inspector_scroll: self.inspector_scroll,
            sort: self.sort,
            trail_coloring: self.trail_coloring,
        }
    }

    fn observe_persistence(&mut self) {
        let current = self.snapshot();
        if current != self.observed_slate {
            self.observed_slate = current;
            self.dirty_state.slate = true;
            self.state_scribe.mark();
        }
    }

    fn durable_state(&self) -> DurableState {
        DurableState {
            library: self.dirty_state.library.then(|| self.library.clone()),
            slate: self.dirty_state.slate.then(|| self.observed_slate.clone()),
        }
    }

    fn accept_submission(&mut self, sequence: u64, state: DurableState) {
        if state.library.is_some() {
            self.dirty_state.library = false;
        }
        if state.slate.is_some() {
            self.dirty_state.slate = false;
        }
        self.pending_state = Some((sequence, state));
    }

    fn absorb_persistence(&mut self) {
        let Some(outcome) = self.state_scribe.take_outcome() else {
            return;
        };
        let sequence = match &outcome {
            ScribeOutcome::Saved { sequence } | ScribeOutcome::Fault { sequence, .. } => *sequence,
        };
        if self.pending_state.as_ref().map(|(pending, _)| *pending) != Some(sequence) {
            return;
        }
        let (_sequence, state) = self.pending_state.take().expect("pending sequence matched");
        match outcome {
            ScribeOutcome::Saved { .. } => {}
            ScribeOutcome::Fault { message, .. } => {
                self.dirty_state.library |= state.library.is_some();
                self.dirty_state.slate |= state.slate.is_some();
                self.status = format!("Could not save project state: {message}");
            }
        }
    }
}

fn support_modifiers(editing: bool, ui: &egui::Ui) -> (bool, bool) {
    let modifiers = ui.input(|input| input.modifiers);
    (
        editing && modifiers.shift,
        editing && modifiers.alt && !modifiers.shift,
    )
}

fn primary_click_modifiers(ui: &egui::Ui, rect: egui::Rect) -> Option<egui::Modifiers> {
    ui.input(|input| {
        input.events.iter().rev().find_map(|event| match event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            } if rect.contains(*pos) => Some(*modifiers),
            _ => None,
        })
    })
}

fn toolbar_text(
    ui: &mut egui::Ui,
    text: impl Into<ExplainedText>,
    color: Color32,
) -> egui::Response {
    let text = text.into();
    let response = ui.label(
        RichText::new(text.text())
            .monospace()
            .size(12.0)
            .color(color),
    );
    text.explain(response)
}

fn toolbar_title(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.label(
        RichText::new(text.into())
            .monospace()
            .strong()
            .size(14.0)
            .color(chrome::TEXT),
    )
}

const fn toolbar_standing_color(standing: TrailStanding) -> Color32 {
    match standing {
        TrailStanding::Unknown => chrome::MUTED,
        known => map::trail_standing_color(known),
    }
}

fn trail_name_is_valid(name: &str) -> bool {
    validate_trail_name(name).is_ok()
}

fn rename_shortcuts(ui: &egui::Ui, edit: &egui::Response) -> (bool, bool) {
    // Response focus queries lock egui's context; keep them outside input closures.
    let focused = edit.has_focus();
    let relinquished = edit.lost_focus();
    ui.input(|input| {
        (
            (focused || relinquished) && input.key_pressed(egui::Key::Enter),
            focused && input.key_pressed(egui::Key::Escape),
        )
    })
}

fn search_progress(ui: &mut egui::Ui, progress: SearchProgress) {
    let fraction = match progress.stage {
        SearchStage::Preparing | SearchStage::Exploring => {
            progress.explored as f32 / progress.limit.max(1) as f32
        }
        SearchStage::Ranking => 1.0,
    };
    let _label = ui.label(
        RichText::new(search_progress_text(progress).to_ascii_uppercase())
            .monospace()
            .size(10.0)
            .color(chrome::HOT),
    );
    let _progress = ui.add(
        egui::ProgressBar::new(fraction)
            .desired_width(ui.available_width())
            .desired_height(7.0)
            .fill(chrome::HOT)
            .animate(progress.stage != SearchStage::Ranking),
    );
}

fn search_progress_text(progress: SearchProgress) -> String {
    let percent = (progress.explored.saturating_mul(100) / progress.limit.max(1)).min(100);
    match progress.stage {
        SearchStage::Preparing if progress.limit > 0 => {
            format!("Preparing search area · {percent}%")
        }
        SearchStage::Preparing => "Preparing search area".to_owned(),
        SearchStage::Exploring => {
            format!(
                "Searching · {percent}% · {} candidates",
                progress.candidates
            )
        }
        SearchStage::Ranking => format!("Ranking · {} candidates", progress.candidates),
    }
}

const fn route_shape_name(shape: RouteShape) -> &'static str {
    match shape {
        RouteShape::Loop => "loop",
        RouteShape::OutAndBack => "out-and-back",
        RouteShape::FigureEight => "figure-eight",
        RouteShape::Open => "open",
    }
}

const fn coloring_target(coloring: map::TrailColoring) -> Target {
    match coloring {
        map::TrailColoring::Class => Target::LegendClass,
        map::TrailColoring::Formality => Target::LegendFormality,
        map::TrailColoring::Terrain => Target::LegendTerrain,
    }
}

#[cfg(feature = "egui-test")]
const fn contract_route_shape(shape: RouteShape) -> trailgen_contract::RouteShape {
    match shape {
        RouteShape::Loop => trailgen_contract::RouteShape::Loop,
        RouteShape::OutAndBack => trailgen_contract::RouteShape::OutAndBack,
        RouteShape::FigureEight => trailgen_contract::RouteShape::FigureEight,
        RouteShape::Open => trailgen_contract::RouteShape::Open,
    }
}

fn distance_range(ui: &mut egui::Ui, floor_m: &mut f64, ceiling_m: &mut f64) -> SearchEdit {
    let mut low = *floor_m / 1_000.0;
    let mut high = *ceiling_m / 1_000.0;
    let edit = measure_range(
        ui,
        MeasureKind::Distance,
        &mut low,
        &mut high,
        0.1,
        [None, Some(Target::DistanceMax)],
    );
    if edit.changed {
        *floor_m = low * 1_000.0;
        *ceiling_m = high * 1_000.0;
    }
    edit
}

fn moving_time_range(ui: &mut egui::Ui, floor_s: &mut f64, ceiling_s: &mut f64) -> SearchEdit {
    let mut low = *floor_s / 3_600.0;
    let mut high = *ceiling_s / 3_600.0;
    let edit = measure_range(
        ui,
        MeasureKind::MovingTime,
        &mut low,
        &mut high,
        0.25,
        [Some(Target::MovingTimeMin), Some(Target::MovingTimeMax)],
    );
    if edit.changed {
        *floor_s = low * 3_600.0;
        *ceiling_s = high * 3_600.0;
    }
    edit
}

fn measure_range(
    ui: &mut egui::Ui,
    kind: MeasureKind,
    minimum: &mut f64,
    maximum: &mut f64,
    speed: f64,
    targets: [Option<Target>; 2],
) -> SearchEdit {
    ui.vertical(|ui| {
        let label = ui.label(chrome::eyebrow(kind.label()));
        let _label = kind.glosses().explain(label);
        ui.horizontal(|ui| {
            let low = ui.add(
                egui::DragValue::new(minimum)
                    .prefix("MIN ")
                    .range(0.0..=1_000_000.0)
                    .speed(speed)
                    .max_decimals(1),
            );
            if let Some(target) = targets[0] {
                crate::witness::anchor(ui, target, low.rect);
            } else {
                crate::witness::anchor(ui, format!("search.{}.min", kind.id()), low.rect);
            }
            let high = ui.add(
                egui::DragValue::new(maximum)
                    .prefix("MAX ")
                    .range(0.0..=1_000_000.0)
                    .speed(speed)
                    .max_decimals(1),
            );
            if let Some(target) = targets[1] {
                crate::witness::anchor(ui, target, high.rect);
            } else {
                crate::witness::anchor(ui, format!("search.{}.max", kind.id()), high.rect);
            }
            let low_changed = low.changed();
            let high_changed = high.changed();
            let low_submitted = response_submitted(ui, &low);
            let high_submitted = response_submitted(ui, &high);
            reconcile_range(minimum, maximum, low_changed, high_changed);
            let _low = kind.glosses().explain(low);
            let _high = kind.glosses().explain(high);
            SearchEdit {
                changed: low_changed || high_changed,
                submitted: low_submitted || high_submitted,
            }
        })
        .inner
    })
    .inner
}

fn response_submitted(ui: &egui::Ui, response: &egui::Response) -> bool {
    let owns_submission = response.has_focus() || response.lost_focus();
    owns_submission && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

fn reconcile_range(minimum: &mut f64, maximum: &mut f64, low_changed: bool, high_changed: bool) {
    if low_changed && *minimum > *maximum {
        *minimum = *maximum;
    }
    if high_changed && *maximum < *minimum {
        *maximum = *minimum;
    }
}

struct LibraryResponses {
    open: egui::Response,
    visibility: chrome::MonoglyphResponse,
    export: chrome::MonoglyphResponse,
}

fn library_button(
    ui: &mut egui::Ui,
    trail: &SavedTrail,
    selected: bool,
    enabled: bool,
    latched: &mut bool,
) -> LibraryResponses {
    ui.horizontal(|ui| {
        let mechanism = chrome::MechanismSize::Medium;
        let mechanism_side = mechanism.side();
        let mechanism_span = ui.spacing().item_spacing.x + mechanism_side;
        let open_width = 2.0_f32
            .mul_add(-mechanism_span, ui.available_width())
            .max(1.0);
        let open = ui
            .add_enabled_ui(enabled, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(open_width, 38.0), egui::Sense::hover());
                ui.interact(
                    rect,
                    ui.id().with(("saved-trail", trail.id.as_str())),
                    egui::Sense::click(),
                )
            })
            .inner;
        let rect = open.rect;
        if ui.is_rect_visible(rect) {
            let fill = if selected {
                chrome::RAISED
            } else if open.hovered() {
                chrome::SURFACE.gamma_multiply(1.12)
            } else {
                chrome::SURFACE
            };
            let stroke = Stroke::new(
                if selected { 1.4_f32 } else { 1.0_f32 },
                if selected {
                    chrome::HOT
                } else {
                    chrome::EDGE_STRONG
                },
            );
            let _plate = ui
                .painter()
                .rect(rect, 1.0, fill, stroke, egui::StrokeKind::Inside);
            let ink = if enabled {
                if selected { chrome::HOT } else { chrome::TEXT }
            } else {
                chrome::MUTED
            };
            ui.painter().text(
                rect.left_top() + vec2(8.0, 5.0),
                egui::Align2::LEFT_TOP,
                trail.name.to_ascii_uppercase(),
                egui::FontId::monospace(12.5),
                ink,
            );
            ui.painter().text(
                rect.left_bottom() + vec2(8.0, -5.0),
                egui::Align2::LEFT_BOTTOM,
                readout::library_measurements(&trail.metrics),
                egui::FontId::monospace(10.5),
                chrome::MUTED,
            );
            let load = readout::library_load(&trail.metrics);
            ui.painter().text(
                rect.right_bottom() + vec2(-8.0, -5.0),
                egui::Align2::RIGHT_BOTTOM,
                load.text(),
                egui::FontId::monospace(10.5),
                chrome::MUTED,
            );
        }
        let visibility = chrome::Monoglyph::symbol(chrome::Symbol::Visibility)
            .size(mechanism)
            .show_latched(ui, latched)
            .on_hover_text(if *latched {
                "Hide from map"
            } else {
                "Show on map"
            });
        let export = ui
            .add_enabled_ui(enabled, |ui| {
                chrome::Monoglyph::symbol(chrome::Symbol::Export)
                    .size(mechanism)
                    .show(ui)
            })
            .inner
            .on_hover_text("Export GPX");
        LibraryResponses {
            open,
            visibility,
            export,
        }
    })
    .inner
}

fn area_row(
    ui: &mut egui::Ui,
    water: &mut Surface,
    name: Option<&str>,
    slot: usize,
    mutable: bool,
    renameable: bool,
) -> Option<AreaRowAction> {
    let mut action = None;
    let name = name.map_or_else(|| format!("AREA {slot:02}"), str::to_ascii_uppercase);
    let _row = ui.horizontal(|ui| {
        let remove = ui
            .add_enabled_ui(mutable, |ui| {
                chrome::Monoglyph::symbol(chrome::Symbol::Delete)
                    .size(chrome::MechanismSize::Medium)
                    .show(ui)
            })
            .inner
            .on_hover_text("Remove this downloaded area and update trails.");
        water.monoglyph(&remove);
        let rename = ui
            .add_enabled_ui(renameable, |ui| {
                chrome::Monoglyph::symbol(chrome::Symbol::Rename)
                    .size(chrome::MechanismSize::Medium)
                    .show(ui)
            })
            .inner
            .on_hover_text("Rename this map area");
        water.monoglyph(&rename);
        crate::witness::anchor(ui, Target::AreaRename(slot), rename.rect);
        let _label = ui.add(egui::Label::new(chrome::muted(name)).truncate());
        if remove.clicked() {
            action = Some(AreaRowAction::Remove(remove.rect));
        } else if rename.clicked() {
            action = Some(AreaRowAction::Rename);
        }
    });
    action
}

fn gallery_empty(ui: &egui::Ui, message: &str) {
    let _empty = ui.painter().text(
        ui.available_rect_before_wrap().center(),
        egui::Align2::CENTER_CENTER,
        message,
        egui::FontId::monospace(13.0),
        chrome::MUTED,
    );
}

fn paint_support_fault(
    painter: &egui::Painter,
    map_rect: egui::Rect,
    legend_rect: Option<egui::Rect>,
    anchor: egui::Pos2,
    message: &str,
) -> egui::Rect {
    let wrap_width = (map_rect.width() - 24.0).clamp(96.0, 220.0);
    let galley = painter.layout(
        message.to_owned(),
        egui::FontId::monospace(11.0),
        chrome::HOT,
        wrap_width,
    );
    let size = galley.size() + vec2(12.0, 8.0);
    let top = if anchor.y - size.y - 19.0 >= map_rect.top() + 4.0 {
        anchor.y - size.y - 19.0
    } else {
        anchor.y + 19.0
    };
    let mut left = size
        .x
        .mul_add(-0.5, anchor.x)
        .clamp(map_rect.left() + 4.0, map_rect.right() - size.x - 4.0);
    let mut plate = egui::Rect::from_min_size(egui::pos2(left, top), size);
    if let Some(legend) = legend_rect.filter(|legend| legend.expand(4.0).intersects(plate)) {
        left = (legend.left() - size.x - 6.0).max(map_rect.left() + 4.0);
        plate = egui::Rect::from_min_size(egui::pos2(left, top), size);
    }
    let tether = if plate.center().y < anchor.y {
        plate.center_bottom()
    } else {
        plate.center_top()
    };
    painter.line_segment([tether, anchor], Stroke::new(1.5, chrome::HOT));
    let _fill = painter.rect_filled(plate, 1.0, chrome::SURFACE.gamma_multiply(0.98));
    let _stroke = painter.rect_stroke(
        plate,
        1.0,
        Stroke::new(1.5, chrome::HOT),
        egui::StrokeKind::Inside,
    );
    painter.galley(plate.min + vec2(6.0, 4.0), galley, chrome::HOT);
    plate
}

fn editor_fault(error: &TrailgenError, support_count: usize) -> EditorFault {
    let last = support_count.checked_sub(1);
    match error {
        TrailgenError::UnreachableSupport { from, to } => EditorFault {
            message: format!(
                "Pin {to} cannot reach pin {from} on the downloaded trails. Move pin {to} or add an intermediate pin."
            ),
            support_slot: Some(*to),
        },
        TrailgenError::SupportOffNetwork { slot, .. } => EditorFault {
            message: format!(
                "Pin {slot} is too far from a downloaded trail. Move it onto a trail."
            ),
            support_slot: Some(*slot),
        },
        TrailgenError::ShapeMismatch { actual, expected } => EditorFault {
            message: format!(
                "This design forms {actual:?}, not {expected:?}. Move a support point."
            ),
            support_slot: last,
        },
        TrailgenError::InvalidData(_) | TrailgenError::InvalidGeometry(_) => EditorFault {
            message: last.map_or_else(
                || "Trailgen could not connect these support points.".to_owned(),
                |slot| {
                    format!(
                        "Trailgen could not connect pin {slot} to the route. Move it or add an intermediate pin."
                    )
                },
            ),
            support_slot: last,
        },
        _ => EditorFault {
            message: error.to_string(),
            support_slot: last,
        },
    }
}

fn cyclic_step(order: &[usize], current: usize, delta: isize) -> Option<usize> {
    let slot = order.iter().position(|slot| *slot == current)?;
    let next = (slot.cast_signed() + delta)
        .rem_euclid(order.len().cast_signed())
        .cast_unsigned();
    Some(order[next])
}

fn duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2} s", duration.as_secs_f64())
    } else {
        format!("{} ms", duration.as_millis())
    }
}

impl Drop for TrailApp {
    fn drop(&mut self) {
        let state = DurableState {
            library: Some(self.library.clone()),
            slate: Some(self.snapshot()),
        };
        if let Err(error) = self.state_scribe.flush(state) {
            eprintln!("could not save trailgen project state: {error:#}");
        }
    }
}

pub fn forge_water() -> Surface {
    let mut water = Surface::new(Wetness::Wet);
    let (chemistry, agitation) = water.laboratory_mut();
    chemistry.refract_px = 0.22;
    chemistry.meniscus_px = 0.42;
    chemistry.ior_spread = 0.09;
    chemistry.bulge_px = 1.8;
    chemistry.source_gain = 12.0;
    agitation.enter_impulse = 0.14;
    agitation.exit_impulse = 0.08;
    agitation.click_impulse = 0.34;
    agitation.scroll_coupling = 0.0035;
    agitation.pond_impulse = 0.24;
    water
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_shortcuts_do_not_reenter_egui_input_lock() {
        let context = egui::Context::default();
        let mut name = "Harriman South Lows".to_owned();
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let edit = ui.text_edit_singleline(&mut name);
                edit.request_focus();
                assert_eq!(rename_shortcuts(ui, &edit), (false, false));
            })
            .drop_without_applying_deltas();

        let mut enter = egui::RawInput::default();
        enter.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        context
            .run_ui(enter, |ui| {
                let edit = ui.text_edit_singleline(&mut name);
                assert_eq!(rename_shortcuts(ui, &edit), (true, false));
            })
            .drop_without_applying_deltas();
    }

    #[test]
    fn editor_undo_restores_whole_gestures() {
        let first = SupportPoint::forge(Coord::new(-74.0, 41.0)).expect("valid support");
        let second = SupportPoint::forge(Coord::new(-73.99, 41.01)).expect("valid support");
        let mut editor = TrailEditor {
            name: "test".to_owned(),
            name_draft: None,
            origin: EditorOrigin::New,
            return_to: EditorReturn {
                focus: None,
                viewport: Viewport {
                    center: [0.5, 0.5],
                    zoom: 2.0,
                },
            },
            shape: RouteShape::OutAndBack,
            support_points: vec![first],
            coordinate_callouts: vec![false],
            realization: None,
            realizing: None,
            shape_guard: None,
            profile: None,
            fault: None,
            notice: None,
            history: UndoLog::default(),
            drag: None,
        };

        editor.checkpoint();
        editor.shape = RouteShape::Loop;
        assert!(editor.undo());
        assert_eq!(editor.shape, RouteShape::OutAndBack);
        assert!(editor.redo());
        assert_eq!(editor.shape, RouteShape::Loop);
        assert!(editor.undo());

        editor.checkpoint();
        editor.support_points.push(second);
        assert!(editor.undo());
        assert_eq!(editor.support_points, vec![first]);
        assert!(editor.redo());
        assert_eq!(editor.support_points, vec![first, second]);
        assert!(editor.undo());

        editor.drag = Some(PinDrag {
            slot: 0,
            before: editor.sketch(),
            grab: egui::Vec2::ZERO,
        });
        editor.support_points[0] = second;
        editor.finish_drag();
        assert!(editor.undo());
        assert_eq!(editor.support_points, vec![first]);
        assert!(editor.redo());
        assert_eq!(editor.support_points, vec![second]);
    }

    #[test]
    fn manual_autosave_rejects_uncommitted_drag_and_shape_previews() {
        let first = SupportPoint::forge(Coord::new(-74.0, 41.0)).expect("valid support");
        let dragged = SupportPoint::forge(Coord::new(-73.99, 41.01)).expect("valid support");
        let mut editor = TrailEditor::forge(
            "test".to_owned(),
            EditorOrigin::New,
            EditorReturn {
                focus: None,
                viewport: Viewport::WORLD,
            },
            RouteShape::Open,
            vec![first],
        );

        editor.drag = Some(PinDrag {
            slot: 0,
            before: editor.sketch(),
            grab: egui::Vec2::ZERO,
        });
        editor.support_points[0] = dragged;
        assert_eq!(editor.durable_sketch().support_points, vec![first]);

        editor.drag = None;
        editor.shape = RouteShape::Loop;
        editor.shape_guard = Some((7, RouteShape::Open));
        assert_eq!(editor.durable_sketch().shape, RouteShape::Open);
    }

    #[test]
    fn invalid_editor_draft_retains_last_valid_route_and_profile() -> anyhow::Result<()> {
        let graph = trailgen_core::GraphBuilder::default().build(
            &trailgen_core::io::geojson::network_from_str(include_str!(
                "../../trailgen-core/tests/fixtures/mini_network.geojson"
            ))?,
        )?;
        let constraints = LoopConstraints {
            min_distance_m: 0.0,
            max_distance_m: 20_000.0,
            ..LoopConstraints::default()
        };
        let route = SolverKind::Exact
            .solve(
                SearchParams::default(),
                &graph,
                trailgen_core::VertexId(0),
                &constraints,
                1,
            )
            .into_iter()
            .next()
            .expect("fixture has a loop");
        let trail = Trail::infer(&graph, &route, trailgen_core::RoutingLaw::default())
            .expect("fixture route has an exact design");
        let realization = trail.realize("candidate", &graph, &constraints, 1.0)?;
        let profile = ElevationProfile::forge(&graph, &route).expect("fixture has elevation");
        let expected_edges = realization.route.edges.clone();
        let mut editor = TrailEditor {
            name: "candidate".to_owned(),
            name_draft: None,
            origin: EditorOrigin::Candidate,
            return_to: EditorReturn {
                focus: Some(Focus::Candidate { identity: 0 }),
                viewport: Viewport {
                    center: [0.5, 0.5],
                    zoom: 14.0,
                },
            },
            shape: RouteShape::Loop,
            coordinate_callouts: vec![false; trail.support_points.len()],
            support_points: trail.support_points,
            realization: Some(realization),
            realizing: None,
            shape_guard: None,
            profile: Some(profile),
            fault: None,
            notice: None,
            history: UndoLog::default(),
            drag: None,
        };

        let accepted = editor.sketch();
        let notice = editor.reject_loop_closure(&TrailgenError::ShapeMismatch {
            actual: RouteShape::OutAndBack,
            expected: RouteShape::Loop,
        });
        assert!(
            editor.ready(),
            "a rejected toggle preserves the valid draft"
        );
        assert!(editor.sketch() == accepted);
        assert_eq!(
            notice,
            "This design forms OutAndBack, not Loop. Move a support point."
        );
        editor.notice = None;

        editor.absorb_realization(Err(trailgen_core::TrailgenError::InvalidData(
            "transient unroutable draft".to_owned(),
        )));

        assert!(!editor.ready(), "an invalid draft cannot be saved");
        assert!(editor.profile.is_some(), "the last valid profile remains");
        assert_eq!(
            editor
                .realization
                .as_ref()
                .expect("last valid route remains")
                .route
                .edges,
            expected_edges
        );
        Ok(())
    }
}
