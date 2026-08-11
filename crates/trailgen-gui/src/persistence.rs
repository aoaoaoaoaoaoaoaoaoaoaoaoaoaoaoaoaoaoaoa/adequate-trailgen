use anyhow::{Context as _, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};
use tempfile::TempPath;

/// A same-directory file replacement whose abandoned staging file is reaped
/// by ownership.
pub struct AtomicReplacement {
    target: PathBuf,
    staging: Option<TempPath>,
    file: Option<File>,
}

impl AtomicReplacement {
    pub fn raise(target: &Path) -> Result<Self> {
        let parent = target
            .parent()
            .context("replacement target has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create replacement directory {}", parent.display()))?;
        let mut prefix = target
            .file_name()
            .context("replacement target has no file name")?
            .to_os_string();
        prefix.push(".atomic-");
        let temporary = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".partial")
            .tempfile_in(parent)
            .with_context(|| format!("raise staging file beside {}", target.display()))?;
        let (file, staging) = temporary.into_parts();
        Ok(Self {
            target: target.to_owned(),
            staging: Some(staging),
            file: Some(file),
        })
    }

    pub fn take_file(&mut self) -> Result<File> {
        self.file
            .take()
            .context("replacement staging file already taken")
    }

    pub fn commit(mut self) -> Result<()> {
        anyhow::ensure!(
            self.file.is_none(),
            "replacement staging file was never taken"
        );
        let staging = self
            .staging
            .take()
            .context("replacement staging path already committed")?;
        OpenOptions::new()
            .write(true)
            .open(&staging)
            .with_context(|| format!("open staging file for {}", self.target.display()))?
            .sync_all()
            .with_context(|| format!("sync staging file for {}", self.target.display()))?;
        staging
            .persist(&self.target)
            .with_context(|| format!("commit replacement {}", self.target.display()))?;
        sync_directory(
            self.target
                .parent()
                .expect("validated replacement target must retain its parent"),
        )
    }
}

pub fn owns_staging_path(target: &Path, candidate: &Path) -> bool {
    let Some(target) = target.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(candidate) = candidate.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    candidate
        .strip_prefix(target)
        .and_then(|suffix| suffix.strip_prefix(".atomic-"))
        .and_then(|suffix| suffix.strip_suffix(".partial"))
        .is_some_and(|identity| {
            !identity.is_empty() && identity.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

pub fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut replacement = AtomicReplacement::raise(path)?;
    let mut file = replacement.take_file()?;
    file.write_all(bytes)
        .with_context(|| format!("write staging file for {}", path.display()))?;
    drop(file);
    replacement.commit()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open replacement directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync replacement directory {}", path.display()))
}

#[cfg(not(unix))]
const fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_overwrites_and_reaps_abandoned_staging_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("ledger");
        replace(&target, b"first")?;
        replace(&target, b"second")?;
        let abandoned = AtomicReplacement::raise(&target)?;
        drop(abandoned);

        assert_eq!(fs::read(&target)?, b"second");
        assert_eq!(fs::read_dir(temp.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn staging_identity_cannot_claim_an_unrelated_or_named_backup() {
        let target = Path::new("basemap.pmtiles");
        assert!(owns_staging_path(
            target,
            Path::new("basemap.pmtiles.atomic-a71B.partial")
        ));
        assert!(!owns_staging_path(
            target,
            Path::new("dem.pmtiles.atomic-a71B.partial")
        ));
        assert!(!owns_staging_path(
            target,
            Path::new("basemap.pmtiles.backup.partial")
        ));
    }
}
