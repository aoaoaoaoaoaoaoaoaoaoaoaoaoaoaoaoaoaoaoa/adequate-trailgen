use std::sync::OnceLock;

use eternalist_apps::{
    command_guide::{GuideGesture, GuideSection},
    commands::{
        CommandCanon, CommandScope, CommandSpec, Shortcut, ShortcutKey, ShortcutModifiers,
        TextFocusPolicy,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edict {
    OpenProjects,
    CreateProject,
    OpenProject,
    DrawMapArea,
    RefreshMapAreas,
    FindTrails,
    StopSearch,
    ToggleFinder,
    BeginManual,
    UndoSearchEdit,
    RedoSearchEdit,
    EditTrail,
    SaveCandidate,
    RenameFocused,
    DiscardTrailEdit,
    UndoTrailEdit,
    RedoTrailEdit,
    SaveTrail,
    RenameEditor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Context {
    Projects,
    Survey,
    Creator,
    Finder,
    Focus,
    Editor,
}

const OPEN_PROJECTS: [Shortcut; 1] = [Shortcut::primary('O')];
const FIND_TRAILS: [Shortcut; 1] = [Shortcut::new(
    ShortcutModifiers::PRIMARY,
    ShortcutKey::Enter,
)];
const RENAME: [Shortcut; 1] = [Shortcut::new(
    ShortcutModifiers::NONE,
    ShortcutKey::Function(3),
)];
const UNDO: [Shortcut; 1] = [Shortcut::primary('Z')];
const REDO: [Shortcut; 2] = [
    Shortcut::new(
        ShortcutModifiers::PRIMARY.plus(ShortcutModifiers::SHIFT),
        ShortcutKey::Character('Z'),
    ),
    Shortcut::primary('Y'),
];
const SAVE: [Shortcut; 1] = [Shortcut::primary('S')];
const EDIT: [Shortcut; 1] = [Shortcut::new(
    ShortcutModifiers::NONE,
    ShortcutKey::Character('E'),
)];
const DISCARD: [Shortcut; 1] = [Shortcut::new(ShortcutModifiers::ALT, ShortcutKey::Delete)];

const EDICTS: [CommandSpec<Edict, Context>; 19] = [
    CommandSpec::new(
        Edict::OpenProjects,
        "application.open_projects",
        "Projects",
        CommandScope::Global,
    )
    .with_detail("Opens the project deck without discarding the current project.")
    .with_default_shortcuts(&OPEN_PROJECTS)
    .with_mnemonic('P')
    .with_text_focus(TextFocusPolicy::Capture),
    CommandSpec::new(
        Edict::CreateProject,
        "projects.create",
        "Create Project",
        CommandScope::Context(Context::Projects),
    )
    .with_detail("Creates and enters the named project in its chosen parent folder.")
    .with_mnemonic('C')
    .with_text_focus(TextFocusPolicy::Capture),
    CommandSpec::new(
        Edict::OpenProject,
        "projects.open",
        "Open Project",
        CommandScope::Context(Context::Projects),
    )
    .with_detail("Enters the Trailgen project at the chosen folder.")
    .with_mnemonic('O')
    .with_text_focus(TextFocusPolicy::Capture),
    CommandSpec::new(
        Edict::DrawMapArea,
        "survey.draw_map_area",
        "Add Map Area",
        CommandScope::Context(Context::Survey),
    )
    .with_detail("Arms a rectangle gesture that downloads trails for a new area.")
    .with_mnemonic('A'),
    CommandSpec::new(
        Edict::RefreshMapAreas,
        "survey.refresh_map_areas",
        "Refresh Trails",
        CommandScope::Context(Context::Survey),
    )
    .with_detail("Reacquires and rebuilds trails for every downloaded map area.")
    .with_mnemonic('R'),
    CommandSpec::new(
        Edict::FindTrails,
        "finder.find_trails",
        "Find Trails",
        CommandScope::Context(Context::Finder),
    )
    .with_detail("Starts a search with the current trailhead, bounds, and route recipe.")
    .with_default_shortcuts(&FIND_TRAILS)
    .with_mnemonic('F'),
    CommandSpec::new(
        Edict::StopSearch,
        "finder.stop_search",
        "Stop Search",
        CommandScope::Context(Context::Finder),
    )
    .with_detail("Stops the running search while retaining candidates already found."),
    CommandSpec::new(
        Edict::ToggleFinder,
        "creator.toggle_finder",
        "Finder",
        CommandScope::Context(Context::Creator),
    )
    .with_detail("Opens or closes the trail finder without changing trail focus."),
    CommandSpec::new(
        Edict::BeginManual,
        "creator.begin_manual",
        "Manual",
        CommandScope::Context(Context::Creator),
    )
    .with_detail("Starts a new trail design authored directly from support points.")
    .with_mnemonic('M'),
    CommandSpec::new(
        Edict::UndoSearchEdit,
        "finder.undo_segment_edict",
        "Undo Segment Edit",
        CommandScope::Context(Context::Finder),
    )
    .with_detail("Restores the preceding set of required and excluded trail segments.")
    .with_default_shortcuts(&UNDO),
    CommandSpec::new(
        Edict::RedoSearchEdit,
        "finder.redo_segment_edict",
        "Redo Segment Edit",
        CommandScope::Context(Context::Finder),
    )
    .with_detail("Reapplies the next reverted segment requirement or exclusion.")
    .with_default_shortcuts(&REDO),
    CommandSpec::new(
        Edict::EditTrail,
        "focus.edit_trail",
        "Edit",
        CommandScope::Context(Context::Focus),
    )
    .with_detail("Opens the focused trail's support-point design in the editor.")
    .with_default_shortcuts(&EDIT),
    CommandSpec::new(
        Edict::SaveCandidate,
        "focus.save_candidate",
        "Save Trail",
        CommandScope::Context(Context::Focus),
    )
    .with_detail("Gives the focused candidate durable identity in Saved Trails.")
    .with_mnemonic('S'),
    CommandSpec::new(
        Edict::RenameFocused,
        "focus.rename_trail",
        "Rename Trail",
        CommandScope::Context(Context::Focus),
    )
    .with_detail("Begins inline renaming for the focused saved trail.")
    .with_default_shortcuts(&RENAME)
    .with_mnemonic('R'),
    CommandSpec::new(
        Edict::DiscardTrailEdit,
        "editor.discard_trail_edit",
        "Discard Trail Edit",
        CommandScope::Context(Context::Editor),
    )
    .with_detail("Discards the unfinished edit and restores its exact return view.")
    .with_default_shortcuts(&DISCARD),
    CommandSpec::new(
        Edict::UndoTrailEdit,
        "editor.undo_trail_edit",
        "Undo Trail Edit",
        CommandScope::Context(Context::Editor),
    )
    .with_detail("Restores the preceding support-point or shape design.")
    .with_default_shortcuts(&UNDO),
    CommandSpec::new(
        Edict::RedoTrailEdit,
        "editor.redo_trail_edit",
        "Redo Trail Edit",
        CommandScope::Context(Context::Editor),
    )
    .with_detail("Reapplies the next reverted support-point or shape design.")
    .with_default_shortcuts(&REDO),
    CommandSpec::new(
        Edict::SaveTrail,
        "editor.save_trail",
        "Save Trail",
        CommandScope::Context(Context::Editor),
    )
    .with_detail("Commits the realized design to Saved Trails and enters its detail view.")
    .with_default_shortcuts(&SAVE)
    .with_mnemonic('S'),
    CommandSpec::new(
        Edict::RenameEditor,
        "editor.rename_trail",
        "Rename Trail",
        CommandScope::Context(Context::Editor),
    )
    .with_detail("Begins inline renaming without leaving the trail editor.")
    .with_default_shortcuts(&RENAME)
    .with_mnemonic('R'),
];

const ENTER: [Shortcut; 1] = [Shortcut::new(ShortcutModifiers::NONE, ShortcutKey::Enter)];
const ESCAPE: [Shortcut; 1] = [Shortcut::new(ShortcutModifiers::NONE, ShortcutKey::Escape)];
const FOCUS_ARROWS: [Shortcut; 2] = [
    Shortcut::new(ShortcutModifiers::NONE, ShortcutKey::ArrowLeft),
    Shortcut::new(ShortcutModifiers::NONE, ShortcutKey::ArrowRight),
];
const TOGGLE_SIDEBAR: [Shortcut; 1] = [Shortcut::new(
    ShortcutModifiers::NONE,
    ShortcutKey::Function(9),
)];
const NEXT_SIDEBAR_SECTION: [Shortcut; 1] =
    [Shortcut::new(ShortcutModifiers::CONTROL, ShortcutKey::Tab)];
const PREVIOUS_SIDEBAR_SECTION: [Shortcut; 1] = [Shortcut::new(
    ShortcutModifiers::CONTROL.plus(ShortcutModifiers::SHIFT),
    ShortcutKey::Tab,
)];

const SIDEBAR_GESTURES: [GuideGesture; 3] = [
    GuideGesture::new(
        "Show or hide sidebar",
        "Conceals or reveals the project controls.",
        &TOGGLE_SIDEBAR,
    ),
    GuideGesture::new(
        "Next sidebar section",
        "Moves focus to the next control section.",
        &NEXT_SIDEBAR_SECTION,
    ),
    GuideGesture::new(
        "Previous sidebar section",
        "Moves focus to the previous control section.",
        &PREVIOUS_SIDEBAR_SECTION,
    ),
];
const PROJECT_GESTURES: [GuideGesture; 2] = [
    GuideGesture::new(
        "Submit focused field",
        "Creates or opens the project described by the focused text field.",
        &ENTER,
    ),
    GuideGesture::new(
        "Return to open project",
        "Closes the project deck when a project remains open behind it.",
        &ESCAPE,
    ),
];
const MAP_GESTURES: [GuideGesture; 2] = [
    GuideGesture::new(
        "Pan map",
        "Drag unclaimed map ground to move the viewport.",
        &[],
    ),
    GuideGesture::new(
        "Zoom map",
        "Use the wheel over the map to change scale.",
        &[],
    ),
];
const SURVEY_GESTURES: [GuideGesture; 3] = [
    GuideGesture::new(
        "Draw map area",
        "Drag a rectangle after Add Map Area is armed.",
        &[],
    ),
    GuideGesture::new(
        "Resize map area",
        "Drag one of a downloaded area's bronze corner handles.",
        &[],
    ),
    GuideGesture::new(
        "Cancel map gesture",
        "Disarms drawing or restores an area whose resize is still in flight.",
        &ESCAPE,
    ),
];
const FINDER_GESTURES: [GuideGesture; 4] = [
    GuideGesture::new(
        "Find trails",
        "Starts the current recipe when no focused control owns activation.",
        &ENTER,
    ),
    GuideGesture::new(
        "Place trailhead",
        "Alt-click map ground to place or move the trailhead.",
        &[],
    ),
    GuideGesture::new(
        "Edit segment disposition",
        "Click a trail to require it; Shift-click to exclude it.",
        &[],
    ),
    GuideGesture::new(
        "Cancel finder action",
        "Stops search or cancels the active map tool without clearing results.",
        &ESCAPE,
    ),
];
const FOCUS_GESTURES: [GuideGesture; 2] = [
    GuideGesture::new(
        "Previous or next trail",
        "Moves through the current candidate or saved-trail sequence.",
        &FOCUS_ARROWS,
    ),
    GuideGesture::new(
        "Return to map",
        "Restores the viewport that preceded trail detail.",
        &ESCAPE,
    ),
];
const EDITOR_GESTURES: [GuideGesture; 4] = [
    GuideGesture::new(
        "Add support point",
        "Click the realized trail to split a leg, or map ground to append a destination.",
        &[],
    ),
    GuideGesture::new(
        "Move support point",
        "Drag a numbered pin; rerouting commits only when it is released.",
        &[],
    ),
    GuideGesture::new(
        "Delete support point",
        "Shift-click a numbered pin and immediately renumber its successors.",
        &[],
    ),
    GuideGesture::new(
        "Discard trail edit",
        "Discards the unfinished design and restores its exact return view.",
        &DISCARD,
    ),
];
const PROFILE_GESTURES: [GuideGesture; 2] = [
    GuideGesture::new(
        "Inspect route distance",
        "Hover the elevation profile to project its position onto the map; click to lock it.",
        &[],
    ),
    GuideGesture::new(
        "Release profile lock",
        "Right-click the elevation profile to resume hover tracking.",
        &[],
    ),
];

const PROJECT_IDIOM: GuideSection = GuideSection::new("PROJECT DECK", &PROJECT_GESTURES);
const SIDEBAR_IDIOM: GuideSection = GuideSection::new("SIDEBAR", &SIDEBAR_GESTURES);
const MAP_IDIOM: GuideSection = GuideSection::new("MAP", &MAP_GESTURES);
const SURVEY_IDIOM: GuideSection = GuideSection::new("MAP AREAS", &SURVEY_GESTURES);
const FINDER_IDIOM: GuideSection = GuideSection::new("FINDER", &FINDER_GESTURES);
const FOCUS_IDIOM: GuideSection = GuideSection::new("TRAIL DETAIL", &FOCUS_GESTURES);
const EDITOR_IDIOM: GuideSection = GuideSection::new("TRAIL EDITOR", &EDITOR_GESTURES);
const PROFILE_IDIOM: GuideSection = GuideSection::new("ELEVATION PROFILE", &PROFILE_GESTURES);

pub const PROJECT_IDIOMS: [GuideSection; 1] = [PROJECT_IDIOM];
pub const SURVEY_IDIOMS: [GuideSection; 3] = [SIDEBAR_IDIOM, MAP_IDIOM, SURVEY_IDIOM];
pub const BROWSE_IDIOMS: [GuideSection; 2] = [SIDEBAR_IDIOM, MAP_IDIOM];
pub const FINDER_IDIOMS: [GuideSection; 3] = [SIDEBAR_IDIOM, MAP_IDIOM, FINDER_IDIOM];
pub const CANDIDATE_IDIOMS: [GuideSection; 5] = [
    SIDEBAR_IDIOM,
    MAP_IDIOM,
    FINDER_IDIOM,
    FOCUS_IDIOM,
    PROFILE_IDIOM,
];
pub const SAVED_IDIOMS: [GuideSection; 4] = [SIDEBAR_IDIOM, MAP_IDIOM, FOCUS_IDIOM, PROFILE_IDIOM];
pub const EDITOR_IDIOMS: [GuideSection; 4] =
    [SIDEBAR_IDIOM, MAP_IDIOM, EDITOR_IDIOM, PROFILE_IDIOM];

pub const fn scope_name(context: Context) -> &'static str {
    match context {
        Context::Projects => "PROJECT DECK",
        Context::Survey => "MAP AREAS",
        Context::Creator => "TRAIL CREATOR",
        Context::Finder => "FINDER",
        Context::Focus => "TRAIL DETAIL",
        Context::Editor => "TRAIL EDITOR",
    }
}

pub fn canon() -> &'static CommandCanon<Edict, Context> {
    static CANON: OnceLock<CommandCanon<Edict, Context>> = OnceLock::new();
    CANON.get_or_init(|| CommandCanon::new(&EDICTS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edict_canon_is_valid() {
        assert_eq!(canon().specs().len(), EDICTS.len());
    }
}
