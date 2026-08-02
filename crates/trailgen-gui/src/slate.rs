use crate::{
    gallery::TrailSort,
    map::{TrailColoring, Viewport},
};
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Slate {
    pub project: PathBuf,
    pub viewport: Option<Viewport>,
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
        if !slate.inspector_scroll.is_finite() {
            slate.inspector_scroll = 0.0;
        }
        slate.inspector_scroll = slate.inspector_scroll.max(0.0);
        slate.shutters.retain(|section, _| {
            matches!(
                section.as_str(),
                "search" | "library" | "areas" | "overlays"
            )
        });
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
        slate.shutters.insert("areas".to_owned(), true);
        slate.trail_coloring = TrailColoring::Terrain;
        slate.save(&path)?;
        assert_eq!(Slate::load(&path, &alpha), slate);
        let foreign = Slate::load(&path, &beta);
        assert_eq!(foreign.project, beta);
        assert!(foreign.viewport.is_none());
        assert!(foreign.shutters.is_empty());
        Ok(())
    }

    #[test]
    fn obsolete_organizer_state_evaporates_on_load() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("slate.toml");
        let project = temp.path().join("alpha");
        std::fs::write(
            &path,
            format!(
                "project = {:?}\ngallery = \"library\"\nactive_family = 7\n[shutters]\nfamilies = true\nsearch = false\n",
                project.to_string_lossy()
            ),
        )?;

        let slate = Slate::load(&path, &project);
        assert_eq!(slate.shutters.len(), 1);
        assert_eq!(slate.shutters.get("search"), Some(&false));
        slate.save(&path)?;
        assert!(!std::fs::read_to_string(path)?.contains("family"));
        Ok(())
    }
}
