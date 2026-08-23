use crate::{
    gallery::TrailSort,
    library::validate_trail_name,
    map::{TrailColoring, Viewport},
    persistence,
};
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use trailgen_core::{RouteShape, SupportPoint};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManualDraft {
    pub name: String,
    pub shape: RouteShape,
    pub support_points: Vec<SupportPoint>,
    pub viewport: Viewport,
}

impl ManualDraft {
    pub fn normalize(mut self) -> Option<Self> {
        if !matches!(self.shape, RouteShape::Open | RouteShape::Loop)
            || self.support_points.is_empty()
            || !self
                .support_points
                .iter()
                .all(|support| SupportPoint::forge(support.coord()).is_some())
            || !self.viewport.zoom.is_finite()
            || !self.viewport.center.into_iter().all(f64::is_finite)
        {
            return None;
        }
        self.name = validate_trail_name(&self.name)
            .unwrap_or("New Trail")
            .to_owned();
        self.viewport.normalize();
        Some(self)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct SessionState {
    pub project: PathBuf,
    pub viewport: Option<Viewport>,
    pub manual_draft: Option<ManualDraft>,
    #[serde(alias = "shutters")]
    pub panel_folds: BTreeMap<String, bool>,
    pub inspector_scroll: f32,
    pub sort: TrailSort,
    pub trail_coloring: TrailColoring,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            project: PathBuf::new(),
            viewport: None,
            manual_draft: None,
            panel_folds: BTreeMap::new(),
            inspector_scroll: 0.0,
            sort: TrailSort::default(),
            trail_coloring: TrailColoring::default(),
        }
    }
}

impl SessionState {
    pub fn load(path: &Path, project: &Path) -> Self {
        let mut session_state = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str::<Self>(&text).ok())
            .filter(|session_state| session_state.project == project)
            .unwrap_or_default();
        project.clone_into(&mut session_state.project);
        session_state.viewport = session_state.viewport.filter(|viewport| {
            viewport.zoom.is_finite() && viewport.center.into_iter().all(f64::is_finite)
        });
        if let Some(viewport) = &mut session_state.viewport {
            viewport.normalize();
        }
        session_state.manual_draft = session_state.manual_draft.and_then(ManualDraft::normalize);
        if !session_state.inspector_scroll.is_finite() {
            session_state.inspector_scroll = 0.0;
        }
        session_state.inspector_scroll = session_state.inspector_scroll.max(0.0);
        session_state.panel_folds.retain(|section, _| {
            matches!(
                section.as_str(),
                "search" | "library" | "calibration" | "areas" | "overlays"
            )
        });
        session_state
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self).context("serialize workbench session state")?;
        persistence::replace(path, body.as_bytes())
            .with_context(|| format!("replace workbench session state {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailgen_core::Coord;

    fn support(lon: f64, lat: f64) -> SupportPoint {
        SupportPoint::forge(Coord::new(lon, lat)).expect("fixture support must be valid")
    }

    #[test]
    fn session_state_round_trips_and_repels_other_projects() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session_state.toml");
        let alpha = temp.path().join("alpha");
        let beta = temp.path().join("beta");
        let mut session_state = SessionState::load(&path, &alpha);
        session_state.viewport = Some(Viewport {
            center: [0.29, 0.37],
            zoom: 15.5,
        });
        session_state.panel_folds.insert("areas".to_owned(), true);
        session_state.trail_coloring = TrailColoring::Terrain;
        session_state.manual_draft = Some(ManualDraft {
            name: "unfinished crossing".to_owned(),
            shape: RouteShape::Open,
            support_points: vec![support(-74.02, 40.71), support(-73.98, 40.72)],
            viewport: Viewport {
                center: [0.294, 0.376],
                zoom: 16.0,
            },
        });
        session_state.save(&path)?;
        assert_eq!(SessionState::load(&path, &alpha), session_state);
        let foreign = SessionState::load(&path, &beta);
        assert_eq!(foreign.project, beta);
        assert!(foreign.viewport.is_none());
        assert!(foreign.manual_draft.is_none());
        assert!(foreign.panel_folds.is_empty());
        Ok(())
    }

    #[test]
    fn malformed_manual_drafts_do_not_possess_the_workbench() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session_state.toml");
        let project = temp.path().join("alpha");
        let mut session_state = SessionState::load(&path, &project);
        session_state.manual_draft = Some(ManualDraft {
            name: "irrelevant".to_owned(),
            shape: RouteShape::OutAndBack,
            support_points: vec![support(-74.02, 40.71)],
            viewport: Viewport::WORLD,
        });
        session_state.save(&path)?;

        assert!(SessionState::load(&path, &project).manual_draft.is_none());
        Ok(())
    }
}
