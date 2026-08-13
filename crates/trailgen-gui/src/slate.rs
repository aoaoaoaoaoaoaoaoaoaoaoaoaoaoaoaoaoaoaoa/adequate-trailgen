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
            .unwrap_or("manual trail")
            .to_owned();
        self.viewport.normalize();
        Some(self)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Slate {
    pub project: PathBuf,
    pub viewport: Option<Viewport>,
    pub manual_draft: Option<ManualDraft>,
    pub shutters: BTreeMap<String, bool>,
    pub inspector_scroll: f32,
    pub sort: TrailSort,
    pub trail_coloring: TrailColoring,
}

impl Default for Slate {
    fn default() -> Self {
        Self {
            project: PathBuf::new(),
            viewport: None,
            manual_draft: None,
            shutters: BTreeMap::new(),
            inspector_scroll: 0.0,
            sort: TrailSort::default(),
            trail_coloring: TrailColoring::default(),
        }
    }
}

impl Slate {
    pub fn load(path: &Path, project: &Path) -> Self {
        let mut slate = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str::<Self>(&text).ok())
            .filter(|slate| slate.project == project)
            .unwrap_or_default();
        project.clone_into(&mut slate.project);
        slate.viewport = slate.viewport.filter(|viewport| {
            viewport.zoom.is_finite() && viewport.center.into_iter().all(f64::is_finite)
        });
        if let Some(viewport) = &mut slate.viewport {
            viewport.normalize();
        }
        slate.manual_draft = slate.manual_draft.and_then(ManualDraft::normalize);
        if !slate.inspector_scroll.is_finite() {
            slate.inspector_scroll = 0.0;
        }
        slate.inspector_scroll = slate.inspector_scroll.max(0.0);
        slate.shutters.retain(|section, _| {
            matches!(
                section.as_str(),
                "search" | "library" | "calibration" | "areas" | "overlays"
            )
        });
        slate
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self).context("serialize workbench slate")?;
        persistence::replace(path, body.as_bytes())
            .with_context(|| format!("replace workbench slate {}", path.display()))
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
    fn slate_round_trips_and_repels_other_projects() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("slate.toml");
        let alpha = temp.path().join("alpha");
        let beta = temp.path().join("beta");
        let mut slate = Slate::load(&path, &alpha);
        slate.viewport = Some(Viewport {
            center: [0.29, 0.37],
            zoom: 15.5,
        });
        slate.shutters.insert("areas".to_owned(), true);
        slate.trail_coloring = TrailColoring::Terrain;
        slate.manual_draft = Some(ManualDraft {
            name: "unfinished crossing".to_owned(),
            shape: RouteShape::Open,
            support_points: vec![support(-74.02, 40.71), support(-73.98, 40.72)],
            viewport: Viewport {
                center: [0.294, 0.376],
                zoom: 16.0,
            },
        });
        slate.save(&path)?;
        assert_eq!(Slate::load(&path, &alpha), slate);
        let foreign = Slate::load(&path, &beta);
        assert_eq!(foreign.project, beta);
        assert!(foreign.viewport.is_none());
        assert!(foreign.manual_draft.is_none());
        assert!(foreign.shutters.is_empty());
        Ok(())
    }

    #[test]
    fn malformed_manual_drafts_do_not_possess_the_workbench() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("slate.toml");
        let project = temp.path().join("alpha");
        let mut slate = Slate::load(&path, &project);
        slate.manual_draft = Some(ManualDraft {
            name: "irrelevant".to_owned(),
            shape: RouteShape::OutAndBack,
            support_points: vec![support(-74.02, 40.71)],
            viewport: Viewport::WORLD,
        });
        slate.save(&path)?;

        assert!(Slate::load(&path, &project).manual_draft.is_none());
        Ok(())
    }
}
