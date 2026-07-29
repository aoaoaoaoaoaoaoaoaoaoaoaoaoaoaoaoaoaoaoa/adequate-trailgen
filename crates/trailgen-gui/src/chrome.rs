pub use dwemer_poolrooms::chrome::*;
use egui::{RichText, Stroke, Vec2};

pub fn command_button(text: impl Into<String>, selected: bool) -> egui::Button<'static> {
    let text = RichText::new(text.into())
        .size(13.0)
        .strong()
        .color(if selected { HOT } else { TEXT });
    let button = egui::Button::new(text).min_size(Vec2::new(24.0, 20.0));
    if selected {
        button.fill(RAISED).stroke(Stroke::new(1.4_f32, HOT))
    } else {
        button
    }
}

pub fn command(ui: &mut egui::Ui, text: impl Into<String>, selected: bool) -> egui::Response {
    let response = ui.add(command_button(text, selected));
    tension(ui, &response);
    response
}

pub fn command_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    text: impl Into<String>,
    selected: bool,
) -> egui::Response {
    let response = ui.add_enabled(enabled, command_button(text, selected));
    tension(ui, &response);
    response
}
