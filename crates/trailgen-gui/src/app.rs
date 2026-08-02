use crate::{
    annotation,
    basemap::Source as BasemapSource,
    chrome,
    civic_area::{self, AddOutcome, CivicAreas, CivicKey, CivicRowState},
    gallery::{self, TrailSort},
    library::{Library, SavedTrail, SearchRecipe, TrailId, Trailhead, validate_trail_name},
    live_area::{self, RegionHandles, RegionScribe, ResizeEvent, ScribeEvent},
    map::{self, Atlas, SELECTED_TRAIL_COLOR, Viewport},
    portfolio::{self, CandidatePortfolio, CandidateWarmth},
    profile::ElevationProfile,
    project::{Project, SearchEvent, SearchForge, SearchHandle, SearchRequest},
    relief::Relief,
    search_boundary::{self, BoundaryEvent, BoundaryScribe},
    shortcut_help::{Mode as ShortcutMode, ShortcutHelp},
    slate::{ManualDraft, Slate},
    trail_data::{
        Event as TrailDataEvent, Mutation as TrailDataMutation, TrailData, progress_status,
    },
    vector_field::VectorField,
};
use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use dwemer_poolrooms::water::{Domain, Frame as WaterFrame, Surface, Wetness};
use egui::{Color32, RichText, Stroke, vec2};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
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
    committed_library: Library,
    library_dirty: Option<Instant>,
    saved_projections: BTreeMap<TrailId, Option<SavedProjection>>,
    projection_forge: ProjectionForge,
    hovered_saved: Option<TrailId>,
    rename: Option<RenameDraft>,
    candidates: Option<CandidatePortfolio>,
    results_open: bool,
    edicts: EdgeEdicts,
    edict_history: UndoLog<EdgeEdicts>,
    search_due: Option<Instant>,
    view: WorkbenchView,
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
    shortcuts: ShortcutHelp,
    boundary_scribe: BoundaryScribe,
    offline: bool,
    shutters: BTreeMap<String, bool>,
    inspector_scroll: f32,
    slate_path: PathBuf,
    committed_slate: Slate,
    observed_slate: Slate,
    slate_dirty: Option<Instant>,
    water: Surface,
    status: String,
    trail_data_status: Option<String>,
    profile_cursor: ProfileCursor,
    map_rect: egui::Rect,
    map_regime: MapRegime,
    workspace_signal: Option<Action>,
    post_armament: Option<TrailDataMutation>,
}

struct TrailEditor {
    name: String,
    name_draft: Option<EditorNameDraft>,
    origin: EditorOrigin,
    return_to: EditorReturn,
    shape: RouteShape,
    support_points: Vec<SupportPoint>,
    realization: Option<TrailRealization>,
    realizing: Option<u64>,
    shape_guard: Option<(u64, RouteShape)>,
    profile: Option<ElevationProfile>,
    fault: Option<String>,
    notice: Option<String>,
    history: UndoLog<TrailSketch>,
    drag: Option<PinDrag>,
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
        self.support_points = target.support_points;
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
                self.fault = Some(editor_fault(&err));
                self.notice = None;
            }
        }
    }

    fn reject_loop_closure(&mut self, error: &TrailgenError) -> String {
        let notice = editor_fault(error);
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
    Delete(egui::Rect),
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
    command: Sender<EditorJob>,
    pending: Receiver<EditorJob>,
    events: Receiver<EditorEvent>,
    _thread: thread::JoinHandle<()>,
}

struct ProjectionForge {
    command: Sender<(TrailId, SavedTrail)>,
    events: Receiver<(TrailId, SavedProjection)>,
    _thread: thread::JoinHandle<()>,
}

impl EditorForge {
    fn spawn(ctx: egui::Context, graph: Arc<WalkGraph>) -> Result<Self> {
        let (command, jobs) = bounded::<EditorJob>(1);
        let pending = jobs.clone();
        let (events_tx, events) = unbounded();
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
                    ctx.request_repaint();
                }
            })
            .context("spawn trail realizer")?;
        Ok(Self {
            command,
            pending,
            events,
            _thread: worker,
        })
    }

    fn strike(&self, job: EditorJob) -> Result<()> {
        match self.command.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(job)) => {
                let _superseded = self.pending.try_recv();
                self.command
                    .try_send(job)
                    .map_err(|_| anyhow::anyhow!("trail realizer stopped"))
            }
            Err(TrySendError::Disconnected(_)) => Err(anyhow::anyhow!("trail realizer stopped")),
        }
    }
}

impl ProjectionForge {
    fn spawn(ctx: egui::Context) -> Result<Self> {
        let (command, jobs) = unbounded::<(TrailId, SavedTrail)>();
        let (publish, events) = unbounded();
        let thread = thread::Builder::new()
            .name("saved-trail-projector".to_owned())
            .spawn(move || {
                while let Ok((id, trail)) = jobs.recv() {
                    if publish.send((id, SavedProjection::forge(&trail))).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .context("spawn saved-trail projector")?;
        Ok(Self {
            command,
            events,
            _thread: thread,
        })
    }

    fn strike(&self, id: TrailId, trail: SavedTrail) {
        let _sent = self.command.send((id, trail));
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
                            worker_ctx.clone(),
                            Arc::clone(&graph),
                            Arc::clone(&finder_index),
                        )?
                    );
                    let editor_forge = product_phase!(
                        "corpus.editor_worker",
                        EditorForge::spawn(worker_ctx.clone(), Arc::clone(&graph))?
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
                worker_ctx.request_repaint();
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
}

impl ReloadFrame {
    pub const fn browse(viewport: Viewport) -> Self {
        Self {
            focus: None,
            viewport,
            browse_viewport: None,
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
        let projection_forge = ProjectionForge::spawn(ctx.clone())?;
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
        let mut app = Self {
            root,
            name: config.name,
            sinew: None,
            editor_serial: 0,
            defaults: config.constraints,
            params: config.search,
            solver: config.solver,
            committed_library: library.clone(),
            library,
            library_dirty: None,
            saved_projections: BTreeMap::new(),
            projection_forge,
            hovered_saved: None,
            rename: None,
            candidates: None,
            results_open: false,
            edicts: EdgeEdicts::default(),
            edict_history: UndoLog::default(),
            search_due: None,
            view,
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
            shortcuts: ShortcutHelp::default(),
            boundary_scribe: BoundaryScribe::default(),
            offline,
            shutters: slate.shutters.clone(),
            inspector_scroll: slate.inspector_scroll,
            slate_path,
            committed_slate: slate.clone(),
            observed_slate: slate,
            slate_dirty: None,
            water: forge_water(),
            status,
            trail_data_status: Some("Preparing trail network…".to_owned()),
            profile_cursor: ProfileCursor::default(),
            map_rect: egui::Rect::ZERO,
            map_regime: MapRegime::Browse,
            workspace_signal: None,
            post_armament: refresh.then_some(TrailDataMutation::Refresh),
        };
        app.reconcile_saved_projections();
        app.observed_slate = app.snapshot();
        Ok(app)
    }

    pub fn pulse(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        product_phase!("pulse.search_events", self.absorb_events(ui.ctx()));
        product_phase!("pulse.editor_events", self.absorb_editor_events());
        product_phase!("pulse.saved_projections", self.absorb_saved_projections());
        product_phase!("pulse.corpus_events", self.absorb_corpus(ui.ctx()));
        product_phase!(
            "pulse.deferred_trail_refresh",
            self.tend_post_armament(ui.ctx())
        );
        if let Some(alarm) = product_phase!("pulse.civic_events", self.civic.pulse()) {
            self.status = alarm;
        }
        product_phase!("pulse.input", {
            if !self.shortcuts.capture(ui.ctx()) {
                self.take_keys(ui.ctx());
            }
        });
        let profile_owner = self.profile_owner();
        self.profile_cursor.bind(profile_owner);
        self.hovered_saved = None;
        self.profile_cursor.marker = None;
        let _left = product_phase!(
            "pulse.inspector",
            egui::Panel::left("trail-inspector")
                .resizable(false)
                .exact_size(chrome::INSPECTOR_WIDTH)
                .show_inside(ui, |ui| {
                    let scroll = egui::ScrollArea::vertical()
                        .id_salt("trail-inspector-scroll")
                        .vertical_scroll_offset(self.inspector_scroll)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add_space(ui.spacing().item_spacing.x);
                            self.inspector(ui);
                        });
                    self.inspector_scroll = scroll.state.offset.y.max(0.0);
                    self.water.heave(ui.ctx(), scroll.state.offset.y);
                })
        );
        let _center = product_phase!(
            "pulse.arena",
            egui::CentralPanel::default().show_inside(ui, |ui| self.arena(ui))
        );
        let shortcut_mode = match self.view {
            WorkbenchView::Browse => ShortcutMode::Browse,
            WorkbenchView::Focus(_) => ShortcutMode::Focus,
            WorkbenchView::Edit(_) => ShortcutMode::Editor,
        };
        product_phase!(
            "pulse.shortcuts",
            self.shortcuts.show(ui.ctx(), shortcut_mode)
        );
        product_phase!("pulse.search_commit", self.tend_search(ui.ctx()));
        product_phase!("pulse.library_commit", self.tend_library(ui.ctx()));
        product_phase!("pulse.slate_commit", self.tend_slate(ui.ctx()));
        self.workspace_signal.take()
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
            },
            Some(Focus::Candidate { .. }) | None => ReloadFrame {
                focus: None,
                viewport: self.viewport,
                browse_viewport: None,
            },
        }
    }

    pub fn restore_reload_frame(&mut self, frame: ReloadFrame) {
        self.viewport = frame.viewport;
        self.fit = Fit::None;
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
            shortcut_help: self.shortcuts.open(),
            text_edit_focused,
            saved_trails: self.library.trails().len(),
            candidates: self
                .candidates
                .as_ref()
                .map_or(0, |portfolio| portfolio.routes.len()),
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
            boundary: recipe.boundary.is_some(),
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
        self.water.frame(ctx, pixels_per_point, tooltip_rects, None)
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let _name = ui.label(chrome::title(self.name.to_ascii_uppercase()));
        ui.add_space(3.0);
        let projects = ui
            .horizontal(|ui| {
                let width = (ui.available_width() - 31.0).max(24.0);
                let projects = ui
                    .add_enabled(
                        !self.view.is_editing(),
                        chrome::command_button("PROJECTS", false).min_size(vec2(width, 27.0)),
                    )
                    .on_hover_text("Open the project deck · Ctrl+O")
                    .on_disabled_hover_text("Finish or cancel the trail edit first.");
                self.shortcuts.button(ui);
                projects
            })
            .inner;
        chrome::tension(ui, &projects);
        if projects.clicked() {
            self.workspace_signal = Some(Action::Projects);
            self.water.click(projects.rect);
        }
        ui.add_space(3.0);
        self.section(ui, "library", "saved trails", true, Self::library_panel);
        self.section(ui, "search", "trail creator", true, Self::search_panel);
        self.section(ui, "areas", "map areas", true, Self::area_panel);
        self.section(ui, "overlays", "overlays", true, Self::civic_panel);
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
                    AddOutcome::Added(_) => {
                        self.status = format!("Preparing {name} boundary…");
                    }
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
        ui: &mut egui::Ui,
        id: &'static str,
        title: &'static str,
        open: bool,
        body: fn(&mut Self, &mut egui::Ui),
    ) {
        let open = self.shutters.get(id).copied().unwrap_or(open);
        let wake = chrome::section(ui, id, title, open, |ui| body(self, ui));
        if let Some(wake) = wake.as_ref() {
            let _prior = self
                .shutters
                .insert(id.to_owned(), matches!(wake.flux, chrome::FoldFlux::Open));
        }
        self.water.fold(wake);
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
        let validation = self
            .sinew
            .as_ref()
            .context("trail network is still preparing")
            .and_then(|sinew| {
                self.search_request(self.serial.saturating_add(1))
                    .and_then(|request| request.validate(&sinew.graph))
            })
            .err()
            .map(|err| err.to_string());
        let stopping = matches!(
            self.forge_phase,
            ForgePhase::Striking { stopping: true, .. }
        );
        let find = ui.add_enabled(
            if striking {
                !stopping
            } else {
                validation.is_none()
            },
            chrome::command_button(
                if striking {
                    if stopping {
                        "STOPPING…"
                    } else {
                        "STOP SEARCH"
                    }
                } else {
                    "FIND TRAILS"
                },
                striking || validation.is_none(),
            )
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
        if find.clicked() && striking {
            self.stop_search();
            self.water.thwack(find.rect, 0.7);
        } else if !striking && (find.clicked() || recipe_edit.submitted) {
            self.activate_search(validation.as_deref(), find.rect);
        }
    }

    fn creator_tabs(&mut self, ui: &mut egui::Ui) -> bool {
        let editing = self.view.is_editing();
        let (finder, manual) = ui
            .horizontal(|ui| {
                let finder = chrome::command(ui, "FINDER", !editing).on_hover_text(if editing {
                    "Discard this edit and return to the trail finder"
                } else {
                    "Find trails from a trailhead"
                });
                let manual = chrome::command_enabled(
                    ui,
                    editing
                        || (self.sinew.is_some()
                            && !self.forge_phase.active()
                            && self.corpus.is_none()),
                    "MANUAL",
                    editing,
                )
                .on_disabled_hover_text("Wait for the current operation to finish")
                .on_hover_text("Draw a trail with support points");
                (finder, manual)
            })
            .inner;
        crate::witness::anchor(ui, Target::Finder, finder.rect);
        crate::witness::anchor(ui, Target::Manual, manual.rect);
        if finder.clicked() && editing {
            self.cancel_editor();
            self.water.select(finder.rect);
            return true;
        }
        if manual.clicked() && !editing {
            self.begin_editor(EditorOrigin::New, None);
            self.water.select(manual.rect);
            return true;
        }
        false
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
            "climb",
            "CLIMB · M",
            &mut recipe.climb_m.min,
            &mut recipe.climb_m.max,
            10.0,
            [None, None],
        );
        let _load = ui
            .label(chrome::eyebrow("LOWER-LIMB LOAD · FGJW KM"))
            .on_hover_text(
                "Mass-normalized joint work, expressed as the equivalent distance on flat gravel.",
            );
        let load = ui
            .add(
                egui::DragValue::new(&mut recipe.lower_limb_load_km)
                    .prefix("TARGET ")
                    .range(0.0..=1_000.0)
                    .speed(0.5)
                    .max_decimals(1),
            )
            .on_hover_text(
                "Target flat-gravel joint-work-equivalent kilometers. This ranks routes; it is not a safety or capacity rating.",
            );
        crate::witness::anchor(ui, Target::LowerLimbLoad, load.rect);
        let load_edit = SearchEdit {
            changed: load.changed(),
            submitted: response_submitted(ui, &load),
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
                "CLICK TO ADD · DRAG TO MOVE · SHIFT+CLICK TO DELETE"
            },
        );
        if realizing {
            let _progress = ui.label(chrome::eyebrow("UPDATING ROUTE…"));
        }
        if let Some(fault) = fault {
            let _fault = ui.colored_label(chrome::HOT, chrome::muted(fault));
        } else if let Some(notice) = notice {
            let _notice = ui.colored_label(chrome::HOT, chrome::muted(notice));
        }
        ui.add_space(5.0);
        self.editor_shape_controls(ui);
        ui.add_space(5.0);
        let _undo_row = ui.horizontal(|ui| {
            let can_undo = self
                .view
                .editor()
                .is_some_and(|editor| editor.history.can_undo());
            let can_redo = self
                .view
                .editor()
                .is_some_and(|editor| editor.history.can_redo());
            let undo = ui
                .add_enabled(
                    can_undo,
                    chrome::command_button("UNDO", false).min_size(vec2(112.0, 27.0)),
                )
                .on_hover_text("Undo the last trail edit · Ctrl+Z");
            crate::witness::anchor(ui, "editor.undo", undo.rect);
            if undo.clicked() {
                self.undo_editor();
                self.water.click(undo.rect);
            }
            let redo = ui
                .add_enabled(
                    can_redo,
                    chrome::command_button("REDO", false).min_size(vec2(112.0, 27.0)),
                )
                .on_hover_text("Redo the last trail edit · Ctrl+Y or Ctrl+Shift+Z");
            crate::witness::anchor(ui, "editor.redo", redo.rect);
            if redo.clicked() {
                self.redo_editor();
                self.water.click(redo.rect);
            }
        });
        let clear = ui.add_enabled(
            count > 0,
            chrome::command_button("CLEAR", false).min_size(vec2(ui.available_width(), 27.0)),
        );
        crate::witness::anchor(ui, "editor.clear", clear.rect);
        if clear.clicked() {
            self.remember_editor();
            if let Some(editor) = self.view.editor_mut() {
                editor.support_points.clear();
            }
            let _serial = self.reforge_editor();
            self.water.click(clear.rect);
        }
        ui.add_space(5.0);
        let save = ui
            .add_enabled(
                ready,
                chrome::command_button("SAVE TRAIL", ready)
                    .min_size(vec2(ui.available_width(), 34.0)),
            )
            .on_hover_text("Save this trail to the project · Ctrl+S");
        crate::witness::anchor(ui, Target::EditorSave, save.rect);
        chrome::tension(ui, &save);
        if save.clicked() {
            self.save_editor();
            ui.ctx().request_repaint();
            self.water.thwack(save.rect, 0.7);
        }
        let cancel = ui
            .add(chrome::command_button("CANCEL", false).min_size(vec2(ui.available_width(), 27.0)))
            .on_hover_text("Discard this edit and restore the prior view · Esc");
        crate::witness::anchor(ui, "editor.cancel", cancel.rect);
        if cancel.clicked() {
            self.cancel_editor();
            self.water.click(cancel.rect);
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
        let mut opened = None;
        for trail in self.library.trails() {
            let selected = active.as_ref() == Some(&trail.id);
            let response = library_button(ui, trail, selected, navigable);
            #[cfg(feature = "egui-test")]
            crate::witness::anchor(
                ui,
                format!("library.trail/{}", trail.id.as_str()),
                response.rect,
            );
            let hovered = response.hovered();
            if hovered {
                if let Some(projection) = self
                    .saved_projections
                    .get(&trail.id)
                    .and_then(Option::as_ref)
                {
                    response.show_tooltip_ui(|ui| {
                        gallery::saved_preview(ui, trail, &projection.miniature);
                    });
                }
                self.hovered_saved = Some(trail.id.clone());
                self.water
                    .hover(("saved-library", &trail.id), response.rect);
            }
            if response.clicked()
                || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)))
            {
                opened = Some((trail.id.clone(), response.rect));
            }
            ui.add_space(3.0);
        }

        if let Some((id, rect)) = opened {
            self.rename = None;
            self.enter_focus(Focus::Saved(id));
            self.water.click(rect);
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
                self.flush_library();
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
            .show_inside(ui, |ui| self.toolbar(ui));
        let _counsel = egui::Panel::bottom("trail-counsel")
            .exact_size(42.0)
            .show_inside(ui, |ui| self.counsel(ui));
        if let Some(editor) = self.view.editor() {
            if editor.profile.is_some() {
                let _profile = egui::Panel::bottom("trail-profile")
                    .exact_size(PROFILE_HEIGHT)
                    .show_inside(ui, |ui| self.profile(ui));
            }
        } else if self.view.focus().is_some() {
            if self.has_profile() {
                let _profile = egui::Panel::bottom("trail-profile")
                    .exact_size(PROFILE_HEIGHT)
                    .show_inside(ui, |ui| self.profile(ui));
            }
        } else if self.results_open {
            let _results = egui::Panel::bottom("trail-results")
                .exact_size(RESULTS_HEIGHT)
                .show_inside(ui, |ui| self.results_gallery(ui));
        } else {
            self.water.hide_loading();
        }
        let _map = egui::CentralPanel::default().show_inside(ui, |ui| self.map(ui));
    }

    fn counsel(&self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        let _row = ui.horizontal(|ui| {
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
                    "Click a trail to place the first support point. Esc cancels."
                } else {
                    "Click to add support points; drag any bronze pin to reshape the trail."
                }
            } else if self.placing_trailhead {
                "Click a trail to place the trailhead. Alt+click also works; Esc cancels."
            } else if self.active_trailhead().is_none() {
                "Place a trailhead, or Alt+click the map, then choose Find trails."
            } else if self.trailhead_drag.is_some() {
                "Drag the trailhead to a new starting point."
            } else {
                &self.status
            };
            let _message = ui.add(
                egui::Label::new(RichText::new(message).monospace().color(chrome::TEXT)).wrap(),
            );
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
                let _metrics = toolbar_text(ui, metrics_summary(metrics), chrome::MUTED);
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
            match self.view.focus() {
                Some(Focus::Candidate { .. }) => {
                    let edit = chrome::command_enabled(
                        ui,
                        self.sinew.is_some() && self.corpus.is_none(),
                        "EDIT TRAIL",
                        false,
                    );
                    crate::witness::anchor(ui, Target::FocusEdit, edit.rect);
                    if edit.clicked() {
                        action = Some(FocusAction::Edit(edit.rect));
                    }
                    let save = chrome::command_enabled(
                        ui,
                        self.sinew.is_some() && self.corpus.is_none(),
                        "SAVE TRAIL",
                        true,
                    );
                    crate::witness::anchor(ui, Target::FocusSave, save.rect);
                    if save.clicked() {
                        action = Some(FocusAction::Save(save.rect));
                    }
                }
                Some(Focus::Saved(_)) => {
                    let edit = chrome::command_enabled(
                        ui,
                        self.sinew.is_some()
                            && self.corpus.is_none()
                            && self.focus_design().is_some(),
                        "EDIT TRAIL",
                        false,
                    )
                    .on_disabled_hover_text("This legacy trail has no support points");
                    crate::witness::anchor(ui, Target::FocusEdit, edit.rect);
                    if edit.clicked() {
                        action = Some(FocusAction::Edit(edit.rect));
                    }
                    let delete = chrome::command(ui, "DELETE TRAIL", false);
                    if delete.clicked() {
                        action = Some(FocusAction::Delete(delete.rect));
                    }
                }
                None => {}
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
                let rename =
                    chrome::command(ui, "✎", false).on_hover_text("Rename this saved trail · F2");
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
            .map(|realization| metrics_summary(&realization.route.metrics));
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
                let rename =
                    chrome::command(ui, "✎", false).on_hover_text("Rename this trail · F2");
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
            Some(FocusAction::Delete(rect)) => {
                self.delete_focused_trail();
                self.water.click(*rect);
            }
            None => {}
        }
    }

    fn results_gallery(&mut self, ui: &mut egui::Ui) {
        let Some(run) = self.candidates.as_ref() else {
            let striking = matches!(self.forge_phase, ForgePhase::Striking { .. });
            gallery_empty(
                ui,
                if striking {
                    "FINDING TRAILS…"
                } else {
                    "NO RESULTS YET"
                },
            );
            if striking {
                self.water
                    .show_loading(ui.ctx(), ui.available_rect_before_wrap());
            } else {
                self.water.hide_loading();
            }
            return;
        };
        self.water.hide_loading();
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
        let legend_claims_pointer = self.interact_trail_legend(ui, rect);
        let pointer = (!legend_claims_pointer)
            .then(|| response.interact_pointer_pos())
            .flatten();
        let support_under_pointer =
            pointer.and_then(|pointer| self.editor_support_at(pointer, rect));
        let excising_supports = self.view.is_editing() && ui.input(|input| input.modifiers.shift);
        let support_excision_hot = excising_supports && support_under_pointer.is_some();
        let area_handles_enabled = self.area_handles_enabled();
        let resize_event = if legend_claims_pointer {
            ResizeEvent::None
        } else {
            self.area_handles
                .interact(self.viewport, ui, rect, &self.regions, area_handles_enabled)
        };
        if !legend_claims_pointer
            && !excising_supports
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
            ui.ctx().set_cursor_icon(if excising_supports {
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
            || support_excision_hot
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
        self.paint_map_scene(ui, rect);

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

    fn paint_map_scene(&mut self, ui: &egui::Ui, rect: egui::Rect) {
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
            self.paint_support_points(&canvas, rect);
        } else if let Some(trailhead) = self.active_trailhead() {
            let (coord, seized) = self
                .trailhead_drag
                .as_ref()
                .map_or_else(|| (trailhead.coord(), false), |drag| (drag.preview, true));
            map::paint_start(&canvas, coord, self.viewport, rect, seized);
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

    fn interact_trail_legend(&mut self, ui: &egui::Ui, rect: egui::Rect) -> bool {
        let Some(sinew) = &mut self.sinew else {
            return false;
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
        ui.input(|input| {
            input
                .pointer
                .hover_pos()
                .zip(legend.rect)
                .is_some_and(|(pointer, legend)| legend.contains(pointer))
        })
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
                    projection.overlay.paint(
                        painter,
                        map::MapFramePlan::forge(self.viewport, rect),
                        self.trail_coloring,
                    );
                }
            }
            WorkbenchView::Browse => {
                if let Some(projection) = self
                    .hovered_saved
                    .as_ref()
                    .and_then(|id| self.saved_projections.get_mut(id))
                    .and_then(Option::as_mut)
                {
                    projection.overlay.paint(
                        painter,
                        map::MapFramePlan::forge(self.viewport, rect),
                        self.trail_coloring,
                    );
                } else if let Some(run) = &mut self.candidates {
                    run.overlay.paint(
                        painter,
                        map::MapFramePlan::forge(self.viewport, rect),
                        self.trail_coloring,
                    );
                }
            }
        }
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

    const fn shows_search_context(&self) -> bool {
        matches!(
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
        let excision_click =
            self.view.is_editing() && click_modifiers.is_some_and(|modifiers| modifiers.shift);
        if alt_click && let Some(pointer) = pointer {
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

    const fn trailhead_input_available(&self) -> bool {
        self.sinew.is_some()
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
        let hot = pointer.is_some_and(|pointer| crate::forge::pin_grip(anchor).contains(pointer));
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
        let text = if self.sinew.is_none() {
            Some(self.status.to_ascii_uppercase())
        } else if self.corpus.is_some() {
            Some(
                self.trail_data_status
                    .as_deref()
                    .unwrap_or("Updating trails…")
                    .to_ascii_uppercase(),
            )
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
        self.flush_library();
        self.schedule_revision();
        self.status = if distance_m < 20.0 {
            "Trailhead set.".to_owned()
        } else {
            format!("Trailhead set; snapped {distance_m:.0} m to the trail.")
        };
        self.water.click(crate::forge::pin_grip(pointer));
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
                crate::forge::pin_grip(anchor)
                    .contains(pointer)
                    .then_some(slot)
            })
    }

    fn paint_support_points(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(editor) = self.view.editor() else {
            return;
        };
        let (excising, hover_pointer) = painter
            .ctx()
            .input(|input| (input.modifiers.shift, input.pointer.hover_pos()));
        for (slot, support) in editor.support_points.iter().enumerate() {
            let anchor =
                map::screen_at(self.viewport, rect, map::world_from_coord(support.coord()));
            let hot = hover_pointer
                .is_some_and(|pointer| crate::forge::pin_grip(anchor).contains(pointer));
            crate::forge::pin(
                painter,
                anchor,
                editor.drag.as_ref().is_some_and(|drag| drag.slot == slot) || (excising && hot),
            );
            #[cfg(feature = "egui-test")]
            crate::witness::rect(
                painter.ctx(),
                Target::Support(slot),
                crate::forge::pin_grip(anchor),
            );
            painter.text(
                crate::forge::pin_bulb(anchor),
                egui::Align2::CENTER_CENTER,
                if excising {
                    "×".to_owned()
                } else {
                    slot.to_string()
                },
                egui::FontId::monospace(if excising { 17.0 } else { 12.0 }),
                chrome::TEXT,
            );
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
                editor.fault = Some("Move closer to a downloaded trail.".to_owned());
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
        let Some(editor) = self.view.editor_mut() else {
            return;
        };
        let changed = if editor.support_points.get(slot) != Some(&support)
            && let Some(current) = editor.support_points.get_mut(slot)
        {
            *current = support;
            true
        } else {
            false
        };
        if changed {
            let _serial = self.reforge_editor();
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
                editor.support_points = reversal.trail.support_points;
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

    fn tend_search(&mut self, ctx: &egui::Context) {
        let Some(due) = self.search_due else {
            return;
        };
        let remaining = due.saturating_duration_since(Instant::now());
        if !remaining.is_zero() || self.forge_phase.active() {
            ctx.request_repaint_after(if remaining.is_zero() {
                Duration::from_millis(20)
            } else {
                remaining
            });
            return;
        }
        self.strike();
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
        Ok(SearchRequest {
            serial,
            start,
            boundary: recipe.boundary.clone(),
            constraints: recipe.constraints(&self.defaults)?,
            params,
            solver: self.solver,
            count: CANDIDATE_COUNT,
            manual_defaults: self.defaults.clone(),
            edicts: self.edicts.clone(),
            warmth,
        })
    }

    fn absorb_events(&mut self, ctx: &egui::Context) {
        let events = self.sinew.as_ref().map_or_else(Vec::new, |sinew| {
            sinew.forge.events.try_iter().collect::<Vec<_>>()
        });
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

    fn absorb_editor_events(&mut self) {
        let events = self.sinew.as_ref().map_or_else(Vec::new, |sinew| {
            sinew.editor_forge.events.try_iter().collect::<Vec<_>>()
        });
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
            ctx.clone(),
            self.root.clone(),
            mutation,
        )?));
        self.trail_data_status = Some("Updating trails…".to_owned());
        Ok(())
    }

    fn absorb_corpus(&mut self, ctx: &egui::Context) {
        let Some(task) = &self.corpus else {
            return;
        };
        if let CorpusTask::Preparing(forge) = task {
            let Ok(result) = forge.event.try_recv() else {
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
        let events = corpus.events.try_iter().collect::<Vec<_>>();
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
                    self.flush_library();
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
            self.status = finder_counsel(&self.library);
        } else {
            "Updated trails are ready.".clone_into(&mut self.status);
        }
        self.flush_library();
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

    const fn active_trailhead(&self) -> Option<Trailhead> {
        self.library.search().trailhead
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
                self.flush_library();
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
            match launched {
                Ok(()) => {
                    editor.realizing = Some(serial);
                    editor.fault = None;
                    editor.notice = None;
                }
                Err(error) => {
                    editor.realizing = None;
                    editor.fault = Some(format!("Could not update this trail: {error:#}"));
                }
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
                self.fit = Fit::Saved(id);
                self.reconcile_saved_projections();
                self.flush_library();
                "Trail saved.".clone_into(&mut self.status);
            }
            Err(err) => self.status = format!("Could not save this trail: {err:#}"),
        }
    }

    fn cancel_editor(&mut self) {
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
        "Trail edit cancelled.".clone_into(&mut self.status);
    }

    fn delete_focused_trail(&mut self) {
        let Some(Focus::Saved(id)) = self.view.focus().cloned() else {
            return;
        };
        if self.library.remove_trail(&id) {
            self.saved_projections.remove(&id);
            self.rename = None;
            self.leave_focus();
            self.flush_library();
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
        self.view = WorkbenchView::Focus(next);
    }

    fn enter_focus(&mut self, focus: Focus) {
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
        if let Some(viewport) = self.focus_frame.pop() {
            self.viewport = viewport;
        }
        self.fit = Fit::None;
    }

    fn dissolve_focus(&mut self) {
        if matches!(self.view, WorkbenchView::Focus(_)) {
            self.view = WorkbenchView::Browse;
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
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
            if self.view.is_editing() {
                "Finish or cancel the trail before changing projects.".clone_into(&mut self.status);
            } else {
                self.workspace_signal = Some(Action::Projects);
            }
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F2)) {
            if self.view.is_editing() {
                self.begin_editor_rename();
                return;
            }
            if let Some(Focus::Saved(id)) = self.view.focus().cloned() {
                self.begin_rename(id);
                return;
            }
        }
        if ctx.text_edit_focused() {
            return;
        }
        let redo = ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z)
                || input.consume_key(egui::Modifiers::CTRL, egui::Key::Y)
        });
        if redo {
            if self.view.is_editing() {
                self.redo_editor();
                return;
            }
            if self.shows_search_context() && self.edict_history.can_redo() {
                self.redo_edict();
                return;
            }
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Z)) {
            if self.view.is_editing() {
                self.undo_editor();
                return;
            }
            if self.shows_search_context() && self.edict_history.can_undo() {
                self.undo_edict();
                return;
            }
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::S))
            && self.view.editor().is_some_and(TrailEditor::ready)
        {
            self.save_editor();
            return;
        }
        let widget_focused = ctx.memory(|memory| memory.focused().is_some());
        let find = !widget_focused
            && ctx.input_mut(|input| {
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)
            });
        if find {
            let search_open = self.shutters.get("search").copied().unwrap_or(true);
            if search_open
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
        if self.forge_phase.active() {
            self.stop_search();
        } else if self.view.is_editing() {
            self.cancel_editor();
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
        self.library_dirty = Some(Instant::now());
    }

    fn reconcile_saved_projections(&mut self) {
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
            self.projection_forge.strike(id.clone(), trail);
            let prior = self.saved_projections.insert(id, None);
            debug_assert!(prior.is_none());
        }
    }

    fn absorb_saved_projections(&mut self) {
        while let Ok((id, projection)) = self.projection_forge.events.try_recv() {
            if self.library.trail(&id).is_some() {
                let _pending = self.saved_projections.insert(id, Some(projection));
            }
        }
    }

    fn flush_library(&mut self) {
        match self.library.save(&self.root) {
            Ok(()) => {
                self.committed_library.clone_from(&self.library);
                self.library_dirty = None;
            }
            Err(err) => {
                self.status = format!("Could not save the trail library: {err:#}");
                self.library_dirty = Some(Instant::now());
            }
        }
    }

    fn tend_library(&mut self, ctx: &egui::Context) {
        if self.library == self.committed_library {
            self.library_dirty = None;
            return;
        }
        let dirty = self.library_dirty.get_or_insert_with(Instant::now);
        let settled = dirty.elapsed();
        if settled < STATE_SETTLE {
            ctx.request_repaint_after(STATE_SETTLE.saturating_sub(settled));
        } else {
            self.flush_library();
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

    fn tend_slate(&mut self, ctx: &egui::Context) {
        let current = self.snapshot();
        if current != self.observed_slate {
            self.observed_slate = current;
            self.slate_dirty = Some(Instant::now());
        }
        if self.observed_slate == self.committed_slate {
            self.slate_dirty = None;
            return;
        }
        let dirty = self.slate_dirty.get_or_insert_with(Instant::now);
        let settled = dirty.elapsed();
        if settled < STATE_SETTLE {
            ctx.request_repaint_after(STATE_SETTLE.saturating_sub(settled));
            return;
        }
        match self.observed_slate.save(&self.slate_path) {
            Ok(()) => {
                self.committed_slate.clone_from(&self.observed_slate);
                self.slate_dirty = None;
            }
            Err(err) => {
                self.status = format!("Could not save window state: {err:#}");
                self.slate_dirty = Some(Instant::now());
                ctx.request_repaint_after(STATE_SETTLE);
            }
        }
    }
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

fn toolbar_text(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) -> egui::Response {
    ui.label(
        RichText::new(text.into())
            .monospace()
            .size(12.0)
            .color(color),
    )
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

fn metrics_summary(metrics: &RouteMetrics) -> String {
    let head = format!(
        "{:.2} KM · LOAD {:.1} FGJW KM · MOVING {} · QUALITY {:.0}",
        metrics.distance_m / 1_000.0,
        metrics.lower_limb_load_km,
        moving_time(metrics.moving_time_s),
        metrics.quality
    );
    if metrics.elevation_fraction >= 0.8 {
        format!(
            "{head} · ASCENT {:.0} M · DESCENT {:.0} M",
            metrics.ascent_m, metrics.descent_m
        )
    } else {
        format!("{head} · ELEVATION UNAVAILABLE")
    }
}

fn distance_range(ui: &mut egui::Ui, floor_m: &mut f64, ceiling_m: &mut f64) -> SearchEdit {
    let mut low = *floor_m / 1_000.0;
    let mut high = *ceiling_m / 1_000.0;
    let edit = measure_range(
        ui,
        "distance",
        "DISTANCE · KM",
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
        "moving-time",
        "MOVING TIME · H",
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
    id: &'static str,
    label: &str,
    minimum: &mut f64,
    maximum: &mut f64,
    speed: f64,
    targets: [Option<Target>; 2],
) -> SearchEdit {
    ui.vertical(|ui| {
        let _label = ui.label(chrome::eyebrow(label));
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
                crate::witness::anchor(ui, format!("search.{id}.min"), low.rect);
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
                crate::witness::anchor(ui, format!("search.{id}.max"), high.rect);
            }
            let low_changed = low.changed();
            let high_changed = high.changed();
            reconcile_range(minimum, maximum, low_changed, high_changed);
            SearchEdit {
                changed: low_changed || high_changed,
                submitted: response_submitted(ui, &low) || response_submitted(ui, &high),
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

fn library_button(
    ui: &mut egui::Ui,
    trail: &SavedTrail,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 42.0), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            let fill = if selected {
                chrome::RAISED
            } else if response.hovered() {
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
                rect.left_top() + vec2(8.0, 6.0),
                egui::Align2::LEFT_TOP,
                trail.name.to_ascii_uppercase(),
                egui::FontId::monospace(12.5),
                ink,
            );
            ui.painter().text(
                rect.left_bottom() + vec2(8.0, -6.0),
                egui::Align2::LEFT_BOTTOM,
                library_measurements(&trail.metrics),
                egui::FontId::monospace(10.5),
                chrome::MUTED,
            );
            ui.painter().text(
                rect.right_bottom() + vec2(-8.0, -6.0),
                egui::Align2::RIGHT_BOTTOM,
                library_load(&trail.metrics),
                egui::FontId::monospace(10.5),
                chrome::MUTED,
            );
        }
        response
    })
    .inner
}

fn area_row(
    ui: &mut egui::Ui,
    name: Option<&str>,
    slot: usize,
    mutable: bool,
    renameable: bool,
) -> Option<AreaRowAction> {
    let mut action = None;
    let name = name.map_or_else(|| format!("AREA {slot:02}"), str::to_ascii_uppercase);
    let _row = ui.horizontal(|ui| {
        let rename = ui
            .add_enabled(
                renameable,
                chrome::command_button("✎", false).min_size(vec2(24.0, 22.0)),
            )
            .on_hover_text("Rename this map area");
        crate::witness::anchor(ui, Target::AreaRename(slot), rename.rect);
        let _label = ui.add(egui::Label::new(chrome::muted(name)).truncate());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let remove = ui
                .add_enabled(
                    mutable,
                    chrome::command_button("REMOVE", false).min_size(vec2(58.0, 22.0)),
                )
                .on_hover_text("Remove this downloaded area and update trails.");
            if remove.clicked() {
                action = Some(AreaRowAction::Remove(remove.rect));
            }
        });
        if action.is_none() && rename.clicked() {
            action = Some(AreaRowAction::Rename);
        }
    });
    action
}

fn library_measurements(metrics: &RouteMetrics) -> String {
    if metrics.elevation_fraction >= 0.8 {
        format!(
            "{:.1} KM · {} · +{:.0} M",
            metrics.distance_m / 1_000.0,
            moving_time(metrics.moving_time_s),
            metrics.ascent_m
        )
    } else {
        format!(
            "{:.1} KM · {}",
            metrics.distance_m / 1_000.0,
            moving_time(metrics.moving_time_s)
        )
    }
}

fn library_load(metrics: &RouteMetrics) -> String {
    format!("{:.0} FGJW KM", metrics.lower_limb_load_km)
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

fn editor_fault(error: &TrailgenError) -> String {
    match error {
        TrailgenError::ShapeMismatch { actual, expected } => {
            format!("This design forms {actual:?}, not {expected:?}. Move a support point.")
        }
        _ => error.to_string(),
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

fn moving_time(seconds: f64) -> String {
    let minutes = (seconds.max(0.0) / 60.0).round();
    format!("{:.0}:{:02.0}", (minutes / 60.0).floor(), minutes % 60.0)
}

impl Drop for TrailApp {
    fn drop(&mut self) {
        if self.library != self.committed_library
            && let Err(err) = self.library.save(&self.root)
        {
            eprintln!("could not save trailgen library: {err:#}");
        }
        let current = self.snapshot();
        if current != self.committed_slate
            && let Err(err) = current.save(&self.slate_path)
        {
            eprintln!("could not save trailgen workbench state: {err:#}");
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
    fn trail_name_gate_rejects_void_oversize_and_control_text() {
        assert!(trail_name_is_valid(" Harriman South Lows "));
        assert!(!trail_name_is_valid(" \n "));
        assert!(!trail_name_is_valid(&"x".repeat(81)));
        assert!(!trail_name_is_valid("Cedar\0Pond"));
    }

    #[test]
    fn rename_shortcuts_do_not_reenter_egui_input_lock() {
        let context = egui::Context::default();
        let mut name = "Harriman South Lows".to_owned();
        let _output = context.run_ui(egui::RawInput::default(), |ui| {
            let edit = ui.text_edit_singleline(&mut name);
            edit.request_focus();
            assert_eq!(rename_shortcuts(ui, &edit), (false, false));
        });

        let mut enter = egui::RawInput::default();
        enter.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _output = context.run_ui(enter, |ui| {
            let edit = ui.text_edit_singleline(&mut name);
            assert_eq!(rename_shortcuts(ui, &edit), (true, false));
        });
    }

    fn loop_with_spur(spur_m: f64) -> anyhow::Result<(WalkGraph, Vec<SupportPoint>, Coord)> {
        let junction = Coord::new(0.0, 0.0);
        let east = Coord::new(0.001, 0.0);
        let northeast = Coord::new(0.001, 0.001);
        let north = Coord::new(0.0, 0.001);
        let dead_end = Coord::new(0.0, -spur_m / 111_195.0);
        let feature = |id: &str, from: Coord, to: Coord| {
            serde_json::json!({
                "type": "Feature",
                "properties": {
                    "id": id,
                    "terrain": "trail",
                    "access": "open"
                },
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[from.lon, from.lat], [to.lon, to.lat]]
                }
            })
        };
        let source = serde_json::json!({
            "type": "FeatureCollection",
            "features": [
                feature("south", junction, east),
                feature("east", east, northeast),
                feature("north", northeast, north),
                feature("west", north, junction)
            ]
        });
        let drafts = trailgen_core::io::geojson::network_from_str(&source.to_string())?;
        let graph = trailgen_core::GraphBuilder::default().build(&drafts)?;
        let junction_vertex = graph.nearest_vertex(junction).expect("junction vertex");
        let mut vertices = graph.vertices;
        let dead_end_vertex = trailgen_core::VertexId(vertices.len());
        vertices.push(trailgen_core::Vertex {
            id: dead_end_vertex,
            coord: dead_end,
            junction: None,
        });
        let mut edges = graph.edges;
        let geometry =
            trailgen_core::LineString::new(vec![dead_end, junction]).expect("valid spur");
        let mut attr = edges[0].attr.clone();
        attr.length_m = geometry.length_m();
        edges.push(trailgen_core::Edge {
            id: trailgen_core::EdgeId(edges.len()),
            a: dead_end_vertex,
            b: junction_vertex,
            attr,
            geometry,
        });
        let graph = WalkGraph::new(vertices, edges);
        let supports = [dead_end, east, northeast, north]
            .into_iter()
            .map(|coord| SupportPoint::forge(coord).expect("fixture coordinates are valid"))
            .collect();
        Ok((graph, supports, junction))
    }

    #[test]
    fn cyclic_navigation_respects_gallery_order() {
        let order = [4, 1, 7];
        assert_eq!(cyclic_step(&order, 1, 1), Some(7));
        assert_eq!(cyclic_step(&order, 4, -1), Some(7));
        assert_eq!(cyclic_step(&order, 8, 1), None);
    }

    #[test]
    fn focus_frame_restores_the_view_that_opened_it() {
        let base = Viewport {
            center: [0.21, 0.37],
            zoom: 14.5,
        };
        let second_focus = Viewport {
            center: [0.72, 0.81],
            zoom: 19.0,
        };
        let mut frame = FocusFrame::default();

        frame.push(base);
        frame.push(second_focus);

        assert_eq!(frame.base(second_focus), base);
        assert_eq!(frame.pop(), Some(base));
        assert_eq!(frame.pop(), None);
    }

    #[test]
    fn search_progress_is_bounded_and_speaks_in_user_terms() {
        assert_eq!(
            search_progress_text(SearchProgress {
                stage: SearchStage::Preparing,
                explored: 17,
                limit: 100,
                candidates: 0,
            }),
            "Preparing search area · 17%"
        );
        assert_eq!(
            search_progress_text(SearchProgress {
                stage: SearchStage::Exploring,
                explored: usize::MAX,
                limit: 1,
                candidates: 3,
            }),
            "Searching · 100% · 3 candidates"
        );
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
    fn editor_support_excision_is_one_undoable_renumbering_gesture() {
        let first = SupportPoint::forge(Coord::new(-74.0, 41.0)).expect("valid support");
        let second = SupportPoint::forge(Coord::new(-73.99, 41.01)).expect("valid support");
        let third = SupportPoint::forge(Coord::new(-73.98, 41.02)).expect("valid support");
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
            shape: RouteShape::Open,
            support_points: vec![first, second, third],
            realization: None,
            realizing: None,
            shape_guard: None,
            profile: None,
            fault: None,
            notice: None,
            history: UndoLog::default(),
            drag: None,
        };

        assert!(editor.excise_support(1));
        assert_eq!(editor.support_points, vec![first, third]);
        assert!(editor.undo());
        assert_eq!(editor.support_points, vec![first, second, third]);
        assert!(editor.redo());
        assert_eq!(editor.support_points, vec![first, third]);
        assert!(!editor.excise_support(2));
    }

    #[test]
    fn undo_log_is_bounded_and_new_work_annihilates_the_abandoned_future() {
        let mut history = UndoLog::default();
        for state in 0..=UNDO_DEPTH {
            history.commit(state);
        }
        assert_eq!(history.past.len(), UNDO_DEPTH);
        assert_eq!(history.undo(UNDO_DEPTH + 1), Some(UNDO_DEPTH));
        assert_eq!(history.future.len(), 1);

        history.commit(1_000);

        assert!(history.future.is_empty());
        assert_eq!(history.undo(1_001), Some(1_000));
    }

    #[test]
    fn distance_range_moves_only_the_endpoint_the_user_changed() {
        let mut minimum = 20.0;
        let mut maximum = 10.0;
        reconcile_range(&mut minimum, &mut maximum, false, true);
        assert_eq!((minimum, maximum), (20.0, 20.0));

        let mut minimum = 30.0;
        let mut maximum = 20.0;
        reconcile_range(&mut minimum, &mut maximum, true, false);
        assert_eq!((minimum, maximum), (20.0, 20.0));
    }

    #[test]
    fn saved_trail_measurements_are_compact_and_unambiguous() {
        let metrics = RouteMetrics {
            distance_m: 12_340.0,
            ascent_m: 567.0,
            lower_limb_load_km: 67.6,
            moving_time_s: 3.5 * 3_600.0,
            elevation_fraction: 1.0,
            ..RouteMetrics::default()
        };
        assert_eq!(library_measurements(&metrics), "12.3 KM · 3:30 · +567 M");
        assert!(!library_measurements(&metrics).contains("ASCENT"));
        assert_eq!(library_load(&metrics), "68 FGJW KM");
    }

    #[test]
    fn unknown_path_status_remains_legible_on_dark_chrome() {
        assert_eq!(
            toolbar_standing_color(TrailStanding::Unknown),
            chrome::MUTED
        );
        assert_ne!(
            toolbar_standing_color(TrailStanding::Unknown),
            map::trail_standing_color(TrailStanding::Unknown)
        );
    }

    #[test]
    fn close_loop_preserves_spur_trailheads_and_accepts_the_lawful_retrace() -> anyhow::Result<()> {
        for spur_m in [5.0, 25.0] {
            let (graph, supports, _) = loop_with_spur(spur_m)?;
            let constraints =
                portfolio::manual_constraints(&LoopConstraints::default(), RouteShape::Loop);
            let realization = Trail::forge(
                RouteShape::Loop,
                supports.clone(),
                RoutingLaw::default(),
            )?
            .realize("lollipop loop", &graph, &constraints, TRAILHEAD_SNAP_M)?;

            assert_eq!(realization.trail.shape, RouteShape::Loop);
            assert_eq!(realization.trail.support_points[0], supports[0]);
            assert_eq!(
                realization
                    .graph()
                    .walk_edges(realization.route.start, &realization.route.edges),
                Some(realization.route.start)
            );
            assert_eq!(realization.route.metrics.shape, RouteShape::OutAndBack);
        }
        Ok(())
    }

    #[test]
    fn profile_cursor_lock_survives_hover_and_releases_to_live_motion() {
        let mut cursor = ProfileCursor::default();
        cursor.bind(Some(ProfileOwner::Candidate(7)));
        assert_eq!(cursor.resolve(Some(120.0), false, false), Some(120.0));
        assert_eq!(cursor.resolve(Some(240.0), true, false), Some(240.0));
        assert_eq!(cursor.resolve(Some(360.0), false, false), Some(240.0));
        assert_eq!(cursor.resolve(Some(360.0), false, true), Some(360.0));
        cursor.bind(Some(ProfileOwner::Candidate(8)));
        assert_eq!(cursor.locked_m, None);
    }

    #[test]
    fn reverse_loop_design_preserves_the_trailhead_and_inverts_the_walk() -> anyhow::Result<()> {
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
            .expect("fixture loop is inferable");
        let realization = trail.realize("candidate", &graph, &constraints, 1.0)?;
        let reversal = realization.reverse_loop(&graph)?;
        assert_eq!(reversal.added_supports, 0);
        let reversed = reversal.trail;
        assert_eq!(reversed.support_points[0], trail.support_points[0]);
        let reversed = reversed.realize("candidate", &graph, &constraints, 1.0)?;
        assert_eq!(
            reversed.route.geometry(reversed.graph()),
            realization.route.geometry(&graph).reversed()
        );
        Ok(())
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
