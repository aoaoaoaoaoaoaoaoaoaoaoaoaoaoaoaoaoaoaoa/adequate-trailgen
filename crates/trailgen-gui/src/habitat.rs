use crate::persistence;
use anyhow::{Context as _, Result, ensure};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    env,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
};

const LIBRARY: &str = "trailgen";
const PREFERENCES: &str = "preferences.toml";
const SESSION: &str = "session.json";
const SLATE: &str = "slate.toml";
const SLATES: &str = "projects";
const PROJECT_MARK: &str = "trailgen.toml";
const PROJECT_DIRS: [&str; 5] = ["cache", "reports", "routes", "seeds", "sources"];

#[derive(Serialize)]
struct ProjectMark<'a> {
    name: &'a str,
}

#[derive(Clone, Debug)]
pub struct Habitat {
    config: PathBuf,
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
    pub(crate) fn read(root: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct Mark {
            name: String,
        }

        let root = canonical_project(root)?;
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
        let config = platform.config_dir().to_owned();
        let state = platform
            .state_dir()
            .unwrap_or_else(|| platform.data_local_dir())
            .to_owned();
        let library =
            UserDirs::new().and_then(|dirs| dirs.document_dir().map(|root| root.join(LIBRARY)));
        Ok(Self {
            config,
            library,
            state,
        })
    }

    pub fn resume(&self) -> Result<Option<PathBuf>> {
        self.resume_from(&env::current_dir().context("resolve current project directory")?)
    }

    pub fn remember(&self, root: &Path) -> Result<()> {
        let root = root
            .canonicalize()
            .with_context(|| format!("remember project {}", root.display()))?;
        create_private_dir(&self.state)?;
        write_state(
            &self.state.join(SESSION),
            &serde_json::to_vec_pretty(&Session {
                last_project: root,
                chosen: true,
            })?,
        )
        .with_context(|| format!("write project session under {}", self.state.display()))
    }

    pub fn library_root(&self) -> Option<&Path> {
        self.library.as_deref()
    }

    pub fn preferences_path(&self) -> PathBuf {
        self.config.join(PREFERENCES)
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
        if let Some(session) = self.recall()?
            && session.chosen
            && is_project(&session.last_project)
        {
            projects.push(ProjectPlace::read(&session.last_project)?);
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
            if is_project(&path) {
                let project = ProjectPlace::read(&path)?;
                if !projects.iter().any(|known| known.root == project.root) {
                    projects.push(project);
                }
            }
        }
        projects.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        Ok(projects)
    }

    fn resume_from(&self, current: &Path) -> Result<Option<PathBuf>> {
        if is_project(current) {
            return canonical_project(current).map(Some);
        }
        if let Some(session) = self.recall()?
            && session.chosen
            && is_project(&session.last_project)
        {
            return canonical_project(&session.last_project).map(Some);
        }
        Ok(None)
    }

    fn recall(&self) -> Result<Option<Session>> {
        let path = self.state.join(SESSION);
        match fs::read(&path) {
            Ok(raw) => Ok(serde_json::from_slice::<Session>(&raw).ok()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(err).with_context(|| format!("read project session {}", path.display()))
            }
        }
    }
}

fn write_state(path: &Path, bytes: &[u8]) -> Result<()> {
    persistence::replace(path, bytes)
        .with_context(|| format!("commit state file {}", path.display()))
}

pub fn platform_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "adequate", "trailgen")
        .context("platform exposes no trailgen application directories")
}

fn is_project(root: &Path) -> bool {
    root.join(PROJECT_MARK).is_file()
}

fn canonical_project(root: &Path) -> Result<PathBuf> {
    root.canonicalize()
        .with_context(|| format!("resolve project {}", root.display()))
}

pub fn create_project(root: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim();
    ensure!(!name.is_empty(), "project name must not be empty");
    ensure!(
        !is_project(root),
        "{} is already a trailgen project",
        root.display()
    );
    let existed = root.exists();
    if existed {
        ensure!(root.is_dir(), "{} is not a directory", root.display());
        ensure!(
            fs::read_dir(root)
                .with_context(|| format!("inspect {}", root.display()))?
                .next()
                .is_none(),
            "{} is neither a trailgen project nor an empty folder",
            root.display()
        );
    }
    fs::create_dir_all(root).with_context(|| format!("create project {}", root.display()))?;
    let mut cradle = ProjectCradle::new(root, !existed);
    for dir in PROJECT_DIRS {
        let path = root.join(dir);
        fs::create_dir(&path).with_context(|| format!("create project directory {dir}"))?;
        cradle.created.push(path);
    }
    fs::write(
        root.join(PROJECT_MARK),
        toml::to_string_pretty(&ProjectMark { name })?,
    )
    .with_context(|| format!("write project mark under {}", root.display()))?;
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve created project {}", root.display()))?;
    cradle.disarm();
    Ok(root)
}

struct ProjectCradle {
    root: PathBuf,
    created: Vec<PathBuf>,
    remove_root: bool,
    armed: bool,
}

impl ProjectCradle {
    fn new(root: &Path, remove_root: bool) -> Self {
        Self {
            root: root.to_owned(),
            created: Vec::with_capacity(PROJECT_DIRS.len()),
            remove_root,
            armed: true,
        }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProjectCradle {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _mark = fs::remove_file(self.root.join(PROJECT_MARK));
        for path in self.created.iter().rev() {
            let _dir = fs::remove_dir(path);
        }
        if self.remove_root {
            let _root = fs::remove_dir(&self.root);
        }
    }
}

#[cfg(unix)]
pub fn create_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .with_context(|| format!("create private application state {}", path.display()))
}

#[cfg(not(unix))]
pub fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create private application state {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn habitat(root: &Path) -> Habitat {
        Habitat {
            config: root.join("config/trailgen"),
            library: Some(root.join("documents/trailgen")),
            state: root.join("state/trailgen"),
        }
    }

    fn marked_project(root: &Path, name: &str) -> Result<()> {
        fs::create_dir_all(root.join("cache"))?;
        fs::write(root.join(PROJECT_MARK), format!("name = '{name}'"))?;
        Ok(())
    }

    #[test]
    fn creation_rejects_a_foreign_nonempty_folder() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("occupied");
        fs::create_dir(&root)?;
        fs::write(root.join("precious"), "data")?;
        let error = create_project(&root, "Collision").unwrap_err();
        assert!(error.to_string().contains("nor an empty folder"));
        assert_eq!(fs::read_to_string(root.join("precious"))?, "data");
        Ok(())
    }

    #[test]
    fn project_slates_are_stable_separate_and_migrate_matching_legacy_state() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let alpha = temp.path().join("alpha");
        let beta = temp.path().join("beta");
        marked_project(&alpha, "Alpha")?;
        marked_project(&beta, "Beta")?;
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
