use anyhow::{Context as _, Result, bail, ensure};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    env,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    process,
};

const LIBRARY: &str = "trailgen";
const SAMPLE: &str = "starter-loop";
const SESSION: &str = "session.json";
const SLATE: &str = "slate.toml";
const SLATES: &str = "projects";
const PROJECT_MARK: &str = "trailgen.toml";
const SAMPLE_DIRS: [&str; 5] = ["cache", "reports", "routes", "seeds", "sources"];
const SAMPLE_FILES: [(&str, &[u8]); 3] = [
    (
        "trailgen.toml",
        include_bytes!("../assets/starter/trailgen.toml"),
    ),
    (
        "cache/graph.json",
        include_bytes!("../assets/starter/cache/graph.json"),
    ),
    (
        "routes/generated.routes.json",
        include_bytes!("../assets/starter/routes/generated.routes.json"),
    ),
];

#[derive(Clone, Debug)]
pub struct Habitat {
    library: Option<PathBuf>,
    state: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct Session {
    last_project: PathBuf,
    #[serde(default)]
    chosen: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectPlace {
    pub root: PathBuf,
    pub name: String,
}

impl ProjectPlace {
    fn read(root: PathBuf) -> Result<Self> {
        #[derive(Deserialize)]
        struct Mark {
            name: String,
        }

        let path = root.join(PROJECT_MARK);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read project mark {}", path.display()))?;
        let mark = toml::from_str::<Mark>(&text)
            .with_context(|| format!("parse project mark {}", path.display()))?;
        Ok(Self {
            root,
            name: mark.name,
        })
    }
}

impl Habitat {
    pub fn discover() -> Result<Self> {
        let platform = platform_dirs()?;
        let state = platform
            .state_dir()
            .unwrap_or_else(|| platform.data_local_dir())
            .to_owned();
        let library =
            UserDirs::new().and_then(|dirs| dirs.document_dir().map(|root| root.join(LIBRARY)));
        Ok(Self { library, state })
    }

    pub fn resume(&self) -> Result<Option<PathBuf>> {
        self.resume_from(&env::current_dir().context("resolve current project directory")?)
    }

    pub fn remember(&self, root: &Path) -> Result<()> {
        let root = root
            .canonicalize()
            .with_context(|| format!("remember project {}", root.display()))?;
        create_private_dir(&self.state)?;
        fs::write(
            self.state.join(SESSION),
            serde_json::to_vec_pretty(&Session {
                last_project: root,
                chosen: true,
            })?,
        )
        .with_context(|| format!("write project session under {}", self.state.display()))
    }

    pub fn library_root(&self) -> Option<&Path> {
        self.library.as_deref()
    }

    pub fn sample_root(&self) -> Option<PathBuf> {
        self.library.as_ref().map(|root| root.join(SAMPLE))
    }

    pub fn slate_path(&self, root: &Path) -> PathBuf {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_owned());
        let digest = Sha256::digest(root.to_string_lossy().as_bytes());
        let mut name = String::with_capacity(29);
        for byte in &digest[..12] {
            let _hex = write!(name, "{byte:02x}");
        }
        name.push_str(".toml");
        let path = self.state.join(SLATES).join(name);
        self.migrate_slate(&root, &path);
        path
    }

    fn migrate_slate(&self, root: &Path, path: &Path) {
        #[derive(Deserialize)]
        struct SlateMark {
            project: PathBuf,
        }

        if path.exists() {
            return;
        }
        let legacy = self.state.join(SLATE);
        let Some(text) = fs::read_to_string(&legacy).ok() else {
            return;
        };
        let Ok(mark) = toml::from_str::<SlateMark>(&text) else {
            return;
        };
        if mark.project != root {
            return;
        }
        let Some(parent) = path.parent() else {
            return;
        };
        if create_private_dir(parent).is_ok() {
            let _migrated = fs::rename(legacy, path);
        }
    }

    pub fn known_projects(&self) -> Result<Vec<ProjectPlace>> {
        let mut projects = Vec::new();
        let sample = self.sample_root();
        if let Some(session) = self.recall()?
            && session.chosen
            && sample.as_ref() != Some(&session.last_project)
            && is_living_project(&session.last_project)
        {
            projects.push(ProjectPlace::read(session.last_project)?);
        }
        let Some(library) = &self.library else {
            return Ok(projects);
        };
        let entries = match fs::read_dir(library) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(projects),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("read project library {}", library.display()));
            }
        };
        for entry in entries {
            let path = entry?.path();
            if sample.as_ref() != Some(&path)
                && is_living_project(&path)
                && !projects.iter().any(|project| project.root == path)
            {
                projects.push(ProjectPlace::read(path)?);
            }
        }
        projects.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        Ok(projects)
    }

    fn resume_from(&self, current: &Path) -> Result<Option<PathBuf>> {
        if is_living_project(current) {
            return Ok(Some(current.to_owned()));
        }
        if let Some(session) = self.recall()?
            && session.chosen
            && is_living_project(&session.last_project)
        {
            return Ok(Some(session.last_project));
        }
        Ok(None)
    }

    fn recall(&self) -> Result<Option<Session>> {
        let path = self.state.join(SESSION);
        match fs::read(&path) {
            Ok(raw) => serde_json::from_slice::<Session>(&raw)
                .with_context(|| format!("parse project session {}", path.display()))
                .map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(err).with_context(|| format!("read project session {}", path.display()))
            }
        }
    }
}

pub fn platform_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "adequate", "trailgen")
        .context("platform exposes no trailgen application directories")
}

fn is_project(root: &Path) -> bool {
    root.join(PROJECT_MARK).is_file()
}

fn is_living_project(root: &Path) -> bool {
    is_project(root) && root.join("cache/graph.json").is_file()
}

pub fn forge_sample(root: &Path) -> Result<PathBuf> {
    if is_living_project(root) {
        return Ok(root.to_owned());
    }
    ensure!(
        !root.exists(),
        "managed sample path {} exists but is not a materialized trailgen project",
        root.display()
    );
    let parent = root
        .parent()
        .context("managed sample project has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create managed project library {}", parent.display()))?;
    let mut scaffold = Scaffold::raise(parent)?;
    for dir in SAMPLE_DIRS {
        fs::create_dir_all(scaffold.path().join(dir))
            .with_context(|| format!("forge sample directory {dir}"))?;
    }
    for (relative, body) in SAMPLE_FILES {
        fs::write(scaffold.path().join(relative), body)
            .with_context(|| format!("forge sample artifact {relative}"))?;
    }
    match fs::rename(scaffold.path(), root) {
        Ok(()) => scaffold.disarm(),
        Err(_) if is_living_project(root) => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("install managed sample project {}", root.display()));
        }
    }
    Ok(root.to_owned())
}

struct Scaffold(PathBuf);

impl Scaffold {
    fn raise(parent: &Path) -> Result<Self> {
        for nonce in 0..64 {
            let path = parent.join(format!(".trailgen-sample-{}-{nonce}", process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("raise sample scaffold in {}", parent.display()));
                }
            }
        }
        bail!(
            "sample scaffold namespace exhausted in {}",
            parent.display()
        )
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn disarm(&mut self) {
        self.0.clear();
    }
}

impl Drop for Scaffold {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .with_context(|| format!("create private application state {}", path.display()))
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create private application state {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    fn habitat(root: &Path) -> Habitat {
        Habitat {
            library: Some(root.join("documents/trailgen")),
            state: root.join("state/trailgen"),
        }
    }

    fn living_project(root: &Path, name: &str) -> Result<()> {
        fs::create_dir_all(root.join("cache"))?;
        fs::write(root.join(PROJECT_MARK), format!("name = '{name}'"))?;
        fs::write(root.join("cache/graph.json"), b"{}")?;
        Ok(())
    }

    #[test]
    fn current_project_dominates_session() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let current = temp.path().join("current");
        living_project(&current, "current")?;
        assert_eq!(habitat(temp.path()).resume_from(&current)?, Some(current));
        Ok(())
    }

    #[test]
    fn chosen_session_round_trips_the_canonical_project() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("remembered");
        living_project(&root, "remembered")?;
        let habitat = habitat(temp.path());
        habitat.remember(&root)?;
        assert_eq!(
            habitat.resume_from(temp.path())?,
            Some(root.canonicalize()?)
        );
        Ok(())
    }

    #[test]
    fn legacy_implicit_sample_is_not_resumed() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("remembered");
        living_project(&root, "legacy")?;
        let habitat = habitat(temp.path());
        create_private_dir(&habitat.state)?;
        fs::write(
            habitat.state.join(SESSION),
            serde_json::json!({ "last_project": root }).to_string(),
        )?;
        assert_eq!(habitat.resume_from(temp.path())?, None);
        Ok(())
    }

    #[test]
    fn sample_is_explicit_and_atomic() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let habitat = habitat(temp.path());
        assert_eq!(habitat.resume_from(temp.path())?, None);
        let root = forge_sample(&habitat.sample_root().context("sample root")?)?;
        assert_eq!(root, temp.path().join("documents/trailgen/starter-loop"));
        let project = Project::open(&root)?;
        assert_eq!(project.config.name, "Sample · Colorado");
        assert_eq!(project.graph.vertices.len(), 5);
        assert_eq!(project.routes.len(), 3);
        assert!((project.config.constraints.min_distance_m - 3_000.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn known_projects_deduplicate_the_recent_library_entry() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("documents/trailgen/alpha");
        living_project(&root, "Alpha")?;
        let habitat = habitat(temp.path());
        habitat.remember(&root)?;
        assert_eq!(
            habitat.known_projects()?,
            vec![ProjectPlace {
                root: root.canonicalize()?,
                name: "Alpha".to_owned(),
            }]
        );
        Ok(())
    }

    #[test]
    fn sample_has_one_explicit_home_outside_known_projects() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let habitat = habitat(temp.path());
        let root = forge_sample(&habitat.sample_root().context("sample root")?)?;
        habitat.remember(&root)?;
        assert!(habitat.known_projects()?.is_empty());
        Ok(())
    }

    #[test]
    fn project_slates_are_stable_separate_and_migrate_matching_legacy_state() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let alpha = temp.path().join("alpha");
        let beta = temp.path().join("beta");
        living_project(&alpha, "Alpha")?;
        living_project(&beta, "Beta")?;
        let habitat = habitat(temp.path());
        create_private_dir(&habitat.state)?;
        fs::write(
            habitat.state.join(SLATE),
            format!("project = {:?}", alpha.canonicalize()?),
        )?;

        let beta_slate = habitat.slate_path(&beta);
        let alpha_slate = habitat.slate_path(&alpha);
        assert_ne!(alpha_slate, beta_slate);
        assert_eq!(alpha_slate, habitat.slate_path(&alpha));
        assert!(alpha_slate.is_file());
        assert!(!beta_slate.exists());
        assert!(!habitat.state.join(SLATE).exists());
        Ok(())
    }
}
