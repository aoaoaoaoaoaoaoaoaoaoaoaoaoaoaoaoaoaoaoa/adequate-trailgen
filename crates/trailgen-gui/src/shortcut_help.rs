use crate::chrome;
use egui::{Color32, RichText, Stroke, vec2};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Projects,
    Survey,
    Browse,
    Focus,
    Editor,
}

#[derive(Default)]
pub struct ShortcutHelp {
    open: bool,
}

impl ShortcutHelp {
    /// Capture the help chord and suppress application shortcuts while the
    /// guide owns the keyboard. Escape remains for [`Self::show`] to consume.
    pub fn capture(&mut self, ctx: &egui::Context) -> bool {
        let toggled = ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::SHIFT, egui::Key::Questionmark)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::Questionmark)
        });
        if toggled {
            self.open = !self.open;
        }
        self.open
    }

    #[must_use]
    pub const fn open(&self) -> bool {
        self.open
    }

    pub fn button(&mut self, ui: &mut egui::Ui) {
        let response =
            chrome::command(ui, "?", self.open()).on_hover_text("Keyboard shortcuts · ?");
        crate::witness::anchor(ui, trailgen_contract::Target::ShortcutHelp, response.rect);
        if response.clicked() {
            self.open = !self.open;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, mode: Mode) {
        if !self.open {
            return;
        }
        let screen = ctx.content_rect();
        let frame = egui::Frame::new()
            .fill(chrome::SURFACE)
            .stroke(Stroke::new(1.2_f32, chrome::EDGE_STRONG))
            .corner_radius(2)
            .inner_margin(egui::Margin::same(18));
        let _backdrop = egui::Area::new(egui::Id::new("shortcut-help-backdrop"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .movable(false)
            .interactable(true)
            .show(ctx, |ui| {
                let (rect, _blocker) =
                    ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
                ui.painter()
                    .rect_filled(rect, 0.0, Color32::from_black_alpha(142));
            });
        let shown = egui::Area::new(egui::Id::new("shortcut-help-modal"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(screen.center())
            .constrain_to(screen)
            .movable(false)
            .interactable(true)
            .show(ctx, |ui| {
                let card = frame.show(ui, |ui| {
                    ui.set_width(590.0);
                    let close = ui.horizontal(|ui| {
                        let _title = ui.label(chrome::title("KEYBOARD + GESTURE GUIDE"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(chrome::command_button("CLOSE", false))
                                .on_hover_text("Close shortcut guide · Esc")
                                .clicked()
                        })
                        .inner
                    });
                    ui.add_space(7.0);
                    shortcut_group(ui, "GLOBAL", mode, None, GLOBAL);
                    shortcut_group(ui, "MAP AREAS", mode, Some(Mode::Survey), SURVEY);
                    shortcut_group(ui, "FIND TRAILS", mode, Some(Mode::Browse), BROWSE);
                    shortcut_group(ui, "TRAIL DETAIL", mode, Some(Mode::Focus), FOCUS);
                    shortcut_group(ui, "TRAIL EDITOR", mode, Some(Mode::Editor), EDITOR);
                    close.inner
                });
                crate::witness::anchor(
                    ui,
                    trailgen_contract::Target::ShortcutHelpCard,
                    card.response.rect,
                );
                card.inner
            });
        let escape =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if shown.inner || escape {
            self.open = false;
        }
    }
}

const GLOBAL: &[(&str, &str)] = &[
    ("?", "Open or close this guide"),
    ("Ctrl+O", "Open the project deck"),
];
const SURVEY: &[(&str, &str)] = &[
    ("Drag", "Draw a downloaded map area"),
    ("Drag corner", "Resize a downloaded map area"),
    ("Esc", "Cancel the active map gesture"),
];
const BROWSE: &[(&str, &str)] = &[
    ("Enter", "Find trails with the current recipe"),
    ("Alt+Click", "Place or move the trailhead"),
    ("Click trail", "Require that segment"),
    ("Shift+Click trail", "Forbid that segment"),
    ("Ctrl+Z", "Undo the last segment edict"),
    ("Ctrl+Y", "Redo the last segment edict"),
    ("Esc", "Stop search or cancel the active map tool"),
];
const FOCUS: &[(&str, &str)] = &[
    ("← / →", "Show the previous or next trail"),
    ("F2", "Rename a saved trail"),
    ("Esc", "Return to the prior map viewport"),
];
const EDITOR: &[(&str, &str)] = &[
    ("Click trail", "Add a support point"),
    ("Drag pin", "Move a support point and reroute"),
    ("Ctrl+Z", "Undo the last trail edit"),
    ("Ctrl+Y", "Redo the last trail edit"),
    ("Ctrl+S", "Save the trail"),
    ("F2", "Rename while continuing to edit"),
    ("Esc", "Cancel the trail edit"),
];

fn shortcut_group(
    ui: &mut egui::Ui,
    label: &str,
    mode: Mode,
    owner: Option<Mode>,
    entries: &[(&str, &str)],
) {
    let active =
        owner.is_none_or(|owner| owner == mode) || matches!((mode, owner), (Mode::Projects, None));
    let color = if active { chrome::HOT } else { chrome::MUTED };
    let heading = if active && owner.is_some() {
        format!("{label} · ACTIVE")
    } else {
        label.to_owned()
    };
    ui.add_space(7.0);
    let _heading = ui.label(chrome::eyebrow(heading));
    let _grid = egui::Grid::new(("shortcut-grid", label))
        .num_columns(2)
        .spacing(vec2(18.0, 3.0))
        .show(ui, |ui| {
            for &(chord, intent) in entries {
                let _chord = ui.label(
                    RichText::new(chord)
                        .monospace()
                        .strong()
                        .size(12.5)
                        .color(color),
                );
                let _intent = ui.label(
                    RichText::new(intent)
                        .monospace()
                        .size(11.5)
                        .color(chrome::TEXT),
                );
                ui.end_row();
            }
        });
}
