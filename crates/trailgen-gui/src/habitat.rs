use anyhow::{Context as _, Result, bail, ensure};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process,
};

const LIBRARY: &str = "trailgen";
const STARTER: &str = "starter-loop";
const SESSION: &str = "session.json";
const SLATE: &str = "slate.toml";
const PROJECT_MARK: &str = "trailgen.toml";
const STARTER_DIRS: [&str; 5] = ["cache", "reports", "routes", "seeds", "sources"];
const STARTER_FILES: [(&str, &[u8]); 3] = [
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

    pub fn resume(&self) -> Result<PathBuf> {
        self.resume_from(&env::current_dir().context("resolve current project directory")?)
    }

    pub fn remember(&self, root: &Path) -> Result<()> {
        let root = root
            .canonicalize()
            .with_context(|| format!("remember project {}", root.display()))?;
        create_private_dir(&self.state)?;
        fs::write(
            self.state.join(SESSION),
            serde_json::to_vec_pretty(&Session { last_project: root })?,
        )
        .with_context(|| format!("write project session under {}", self.state.display()))
    }

    pub fn starter_root(&self) -> Option<PathBuf> {
        self.library.as_ref().map(|root| root.join(STARTER))
    }

    pub fn slate_path(&self) -> PathBuf {
        self.state.join(SLATE)
    }

    fn resume_from(&self, current: &Path) -> Result<PathBuf> {
        if is_project(current) {
            return Ok(current.to_owned());
        }
        if let Some(recalled) = self.recall()?
            && is_project(&recalled)
        {
            return Ok(recalled);
        }
        let root = self.starter_root().context(
            "the operating system exposes no Documents directory; choose a project root",
        )?;
        forge_starter(&root)
    }

    fn recall(&self) -> Result<Option<PathBuf>> {
        let path = self.state.join(SESSION);
        match fs::read(&path) {
            Ok(raw) => serde_json::from_slice::<Session>(&raw)
                .with_context(|| format!("parse project session {}", path.display()))
                .map(|session| Some(session.last_project)),
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

pub fn forge_starter(root: &Path) -> Result<PathBuf> {
    if is_project(root) {
        return Ok(root.to_owned());
    }
    ensure!(
        !root.exists(),
        "managed starter path {} exists but is not a trailgen project",
        root.display()
    );
    let parent = root
        .parent()
        .context("managed starter project has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create managed project library {}", parent.display()))?;
    let mut scaffold = Scaffold::raise(parent)?;
    for dir in STARTER_DIRS {
        fs::create_dir_all(scaffold.path().join(dir))
            .with_context(|| format!("forge starter directory {dir}"))?;
    }
    for (relative, body) in STARTER_FILES {
        fs::write(scaffold.path().join(relative), body)
            .with_context(|| format!("forge starter artifact {relative}"))?;
    }
    match fs::rename(scaffold.path(), root) {
        Ok(()) => scaffold.disarm(),
        Err(_) if is_project(root) => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("install managed starter project {}", root.display()));
        }
    }
    Ok(root.to_owned())
}

struct Scaffold(PathBuf);

impl Scaffold {
    fn raise(parent: &Path) -> Result<Self> {
        for nonce in 0..64 {
            let path = parent.join(format!(".trailgen-starter-{}-{nonce}", process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("raise starter scaffold in {}", parent.display())
                    });
                }
            }
        }
        bail!(
            "starter scaffold namespace exhausted in {}",
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

    #[test]
    fn current_project_dominates_session_and_starter() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let current = temp.path().join("current");
        fs::create_dir(&current)?;
        fs::write(current.join(PROJECT_MARK), b"name = 'current'")?;
        assert_eq!(habitat(temp.path()).resume_from(&current)?, current);
        Ok(())
    }

    #[test]
    fn session_round_trips_the_canonical_project() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("remembered");
        fs::create_dir(&root)?;
        fs::write(root.join(PROJECT_MARK), b"name = 'remembered'")?;
        let habitat = habitat(temp.path());
        habitat.remember(&root)?;
        assert_eq!(habitat.resume_from(temp.path())?, root.canonicalize()?);
        Ok(())
    }

    #[test]
    fn starter_is_an_atomic_living_project() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let habitat = habitat(temp.path());
        let root = habitat.resume_from(temp.path())?;
        assert_eq!(root, temp.path().join("documents/trailgen/starter-loop"));
        let project = Project::open(&root)?;
        assert_eq!(project.config.name, "Starter Loop");
        assert_eq!(project.graph.vertices.len(), 5);
        assert_eq!(project.routes.len(), 3);
        assert!((project.config.constraints.min_distance_m - 3_000.0).abs() < f64::EPSILON);
        Ok(())
    }
}
