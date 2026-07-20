use crate::{gallery::CandidateSort, map::Viewport};
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
};
use trailgen_core::{Coord, LoopConstraints, SearchParams, SolverKind, VertexId};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayerSlate {
    pub basemap: bool,
    pub network: bool,
    pub terrain: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchDraft {
    pub constraints: LoopConstraints,
    pub params: SearchParams,
    pub solver: SolverKind,
    pub count: usize,
    pub requested_start: Coord,
    pub start: VertexId,
}

impl Default for LayerSlate {
    fn default() -> Self {
        Self {
            basemap: true,
            network: true,
            terrain: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Slate {
    pub project: PathBuf,
    pub viewport: Option<Viewport>,
    pub shutters: BTreeMap<String, bool>,
    pub inspector_scroll: f32,
    pub sort: CandidateSort,
    pub selected: Option<usize>,
    pub focus: bool,
    #[serde(default = "saved_routes_visible")]
    pub saved_routes_visible: bool,
    pub search: Option<SearchDraft>,
    pub layers: LayerSlate,
}

impl Default for Slate {
    fn default() -> Self {
        Self {
            project: PathBuf::new(),
            viewport: None,
            shutters: BTreeMap::new(),
            inspector_scroll: 0.0,
            sort: CandidateSort::default(),
            selected: None,
            focus: false,
            saved_routes_visible: true,
            search: None,
            layers: LayerSlate::default(),
        }
    }
}

const fn saved_routes_visible() -> bool {
    true
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
        if !slate.inspector_scroll.is_finite() {
            slate.inspector_scroll = 0.0;
        }
        slate.inspector_scroll = slate.inspector_scroll.max(0.0);
        slate
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("slate path has no parent")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create state directory {}", parent.display()))?;
        let body = toml::to_string_pretty(self).context("serialize workbench slate")?;
        let temporary = path.with_extension("toml.tmp");
        {
            let mut file = std::fs::File::create(&temporary)
                .with_context(|| format!("create state staging file {}", temporary.display()))?;
            file.write_all(body.as_bytes())
                .with_context(|| format!("write state staging file {}", temporary.display()))?;
            file.sync_all()
                .with_context(|| format!("sync state staging file {}", temporary.display()))?;
        }
        std::fs::rename(&temporary, path).with_context(|| {
            format!(
                "replace workbench slate {} with {}",
                temporary.display(),
                path.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        slate.search = Some(SearchDraft {
            constraints: LoopConstraints::default(),
            params: SearchParams::default(),
            solver: SolverKind::Exact,
            count: 9,
            requested_start: Coord::new(-74.1, 41.2),
            start: VertexId(3),
        });
        slate.shutters.insert("terrain".to_owned(), true);
        slate.save(&path)?;
        assert_eq!(Slate::load(&path, &alpha), slate);
        let foreign = Slate::load(&path, &beta);
        assert_eq!(foreign.project, beta);
        assert!(foreign.viewport.is_none());
        assert!(foreign.shutters.is_empty());
        assert!(foreign.search.is_none());
        Ok(())
    }

    #[test]
    fn old_slates_keep_saved_candidates_visible() -> Result<()> {
        let slate = toml::from_str::<Slate>("project = '/tmp/alpha'")?;
        assert!(slate.saved_routes_visible);
        assert!(slate.search.is_none());
        Ok(())
    }
}
