use crate::{chrome, witness};
use egui::{CursorIcon, Response, RichText, Ui};

const GLOSS_TITLE_SIZE: f32 = 12.0;
const GLOSS_BODY_SIZE: f32 = 12.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Term {
    BasePace,
    Fgjw,
    MovingTime,
}

impl Term {
    const ALL: [Self; 3] = [Self::BasePace, Self::Fgjw, Self::MovingTime];

    const fn title(self) -> &'static str {
        match self {
            Self::BasePace => "BASE PACE",
            Self::Fgjw => "FGJW KM",
            Self::MovingTime => "MOVING TIME",
        }
    }

    const fn definition(self) -> &'static str {
        match self {
            Self::BasePace => {
                "Your expected speed on flat, firm ground. Trailgen scales terrain-adjusted moving times from this value."
            }
            Self::Fgjw => {
                "Flat-gravel joint-work-equivalent kilometers: modeled lower-limb load expressed as an equivalent flat gravel walk."
            }
            Self::MovingTime => "Calculated using your Base Pace.",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Glosses(u8);

impl Glosses {
    pub const NONE: Self = Self(0);
    pub const BASE_PACE: Self = Self(1 << Term::BasePace as u8);
    pub const FGJW: Self = Self(1 << Term::Fgjw as u8);
    pub const MOVING_TIME: Self = Self(1 << Term::MovingTime as u8);
    pub const ROUTE_METRICS: Self = Self(Self::FGJW.0 | Self::MOVING_TIME.0);

    #[must_use]
    pub const fn contains(self, term: Term) -> bool {
        self.0 & (1 << term as u8) != 0
    }

    pub fn explain(self, response: Response) -> Response {
        if self.0 == 0 {
            return response;
        }
        response
            .on_hover_cursor(CursorIcon::Help)
            .on_hover_ui(|ui| self.card(ui))
    }

    pub fn card(self, ui: &mut Ui) {
        let _column = ui.vertical(|ui| {
            for term in Term::ALL.into_iter().filter(|term| self.contains(*term)) {
                let _title = ui.label(chrome::eyebrow(term.title()).size(GLOSS_TITLE_SIZE));
                let _definition = ui.add(
                    egui::Label::new(
                        RichText::new(term.definition())
                            .monospace()
                            .size(GLOSS_BODY_SIZE)
                            .color(chrome::TEXT),
                    )
                    .wrap(),
                );
            }
            witness::anchor(ui, trailgen_contract::Target::GlossCard, ui.min_rect());
        });
    }
}

#[derive(Clone, Debug)]
pub struct ExplainedText {
    text: String,
    glosses: Glosses,
}

impl ExplainedText {
    #[must_use]
    pub fn forge(text: impl Into<String>, glosses: Glosses) -> Self {
        Self {
            text: text.into(),
            glosses,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn explain(self, response: Response) -> Response {
        self.glosses.explain(response)
    }
}

impl From<&str> for ExplainedText {
    fn from(text: &str) -> Self {
        Self::forge(text, Glosses::default())
    }
}

impl From<String> for ExplainedText {
    fn from(text: String) -> Self {
        Self::forge(text, Glosses::default())
    }
}
