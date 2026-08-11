use anyhow::{Context as _, Result, bail, ensure};
use egui_tester_witness::{Error as WitnessError, ObservationJournal};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const STARTUP_LIMIT: Duration = Duration::from_secs(45);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments.next();
    ensure!(
        arguments.next().is_none(),
        "trailgen-portability accepts at most one mode"
    );
    match mode.as_deref() {
        None => prove_present(),
        Some(mode) if mode == "present" => prove_present(),
        Some(mode) if mode == "lifecycle" => prove_lifecycle(),
        Some(mode) => bail!(
            "unknown trailgen-portability mode {}; expected `present` or `lifecycle`",
            Path::new(mode).display()
        ),
    }
}

fn prove_present() -> Result<()> {
    let binary = binary()?;
    prove_cli(&binary)?;

    let cell = Cell::forge("present")?;
    let witness = cell.path().join("trailgen.observations");
    let frames = cell.path().join("trailgen.frames");
    let launch = format!(
        "trailgen-portability-{}-{}",
        std::env::consts::OS,
        std::process::id()
    );
    let mut command = Command::new(&binary);
    let _command = command
        .args(["gui", "--offline"])
        .env("EGUI_TESTER_WITNESS", &witness)
        .env("EGUI_TESTER_FRAMES", &frames)
        .env("EGUI_TESTER_LAUNCH", &launch)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_host_paths(&mut command, cell.path())?;

    let child = command
        .spawn()
        .with_context(|| format!("launch {}", binary.display()))?;
    let mut captive = Captive::new(child);
    let verdict = await_project_deck(captive.child_mut()?, &witness, &launch)?;
    let output = captive.finish()?;
    std::fs::write(cell.path().join("trailgen.stdout"), &output.stdout)
        .context("retain Trailgen portability stdout")?;
    std::fs::write(cell.path().join("trailgen.stderr"), &output.stderr)
        .context("retain Trailgen portability stderr")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match verdict {
        Startup::Ready => {}
        Startup::Exited(status) => bail!(
            "Trailgen exited before its first witnessed frame with {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ),
        Startup::TimedOut => bail!(
            "Trailgen presented no project deck within {STARTUP_LIMIT:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ),
    }
    println!(
        "Trailgen portability passed: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    Ok(())
}

fn prove_lifecycle() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .context("resolve Trailgen workspace root")?;
    let cell = tempfile::Builder::new()
        .prefix("trailgen-lifecycle-")
        .tempdir()
        .context("forge installation lifecycle cell")?;
    let prefix = cell.path().join("prefix");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let mut install = Command::new(&cargo);
    let _install = install
        .arg("install")
        .arg("--path")
        .arg(root.join("crates/trailgen-cli"))
        .arg("--bin")
        .arg("trailgen")
        .arg("--root")
        .arg(&prefix)
        .args(["--locked", "--force"]);
    run_checked(&mut install, "install ordinary Trailgen product")?;

    let binary = prefix
        .join("bin")
        .join(format!("trailgen{}", std::env::consts::EXE_SUFFIX));
    ensure!(
        binary.is_file(),
        "cargo install omitted {}",
        binary.display()
    );
    prove_cli(&binary)?;

    let mut uninstall = Command::new(cargo);
    let _uninstall = uninstall
        .arg("uninstall")
        .arg("--root")
        .arg(&prefix)
        .arg("trailgen");
    run_checked(&mut uninstall, "uninstall ordinary Trailgen product")?;
    ensure!(
        !binary.exists(),
        "cargo uninstall left {}",
        binary.display()
    );
    println!(
        "Trailgen lifecycle passed: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    Ok(())
}

fn run_checked(command: &mut Command, operation: &str) -> Result<()> {
    let invocation = format!("{command:?}");
    let output = command
        .output()
        .with_context(|| format!("{operation}: {invocation}"))?;
    ensure!(
        output.status.success(),
        "{operation} failed with {}: {invocation}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TRAILGEN_PORTABILITY_BINARY") {
        return Ok(PathBuf::from(path));
    }
    let executable = std::env::current_exe().context("resolve portability executable")?;
    let parent = executable
        .parent()
        .context("portability executable has no parent")?;
    Ok(parent.join(format!("trailgen{}", std::env::consts::EXE_SUFFIX)))
}

fn prove_cli(binary: &Path) -> Result<()> {
    let version = Command::new(binary)
        .arg("--version")
        .output()
        .with_context(|| format!("run {} --version", binary.display()))?;
    let stdout = String::from_utf8_lossy(&version.stdout);
    let stderr = String::from_utf8_lossy(&version.stderr);
    ensure!(
        version.status.success(),
        "{} --version failed with {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        binary.display(),
        version.status
    );
    ensure!(
        stdout.trim().starts_with("trailgen "),
        "{} --version returned an alien identity: {stdout:?}",
        binary.display()
    );

    let help = Command::new(binary)
        .arg("--help")
        .output()
        .with_context(|| format!("run {} --help", binary.display()))?;
    ensure!(
        help.status.success(),
        "{} --help failed with {}\nstdout:\n{}\nstderr:\n{}",
        binary.display(),
        help.status,
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    Ok(())
}

fn isolate_host_paths(command: &mut Command, root: &Path) -> Result<()> {
    let home = root.join("home");
    let roaming = home.join("AppData").join("Roaming");
    let local = home.join("AppData").join("Local");
    let roots = [
        ("HOME", home.clone()),
        ("USERPROFILE", home),
        ("APPDATA", roaming),
        ("LOCALAPPDATA", local),
        ("XDG_CACHE_HOME", root.join("cache")),
        ("XDG_CONFIG_HOME", root.join("config")),
        ("XDG_DATA_HOME", root.join("data")),
        ("XDG_STATE_HOME", root.join("state")),
        ("XDG_RUNTIME_DIR", root.join("runtime")),
    ];
    for (name, path) in roots {
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create isolated {name} at {}", path.display()))?;
        let _command = command.env(name, path);
    }
    Ok(())
}

fn await_project_deck(child: &mut Child, path: &Path, launch: &str) -> Result<Startup> {
    let begun = Instant::now();
    let mut journal = ObservationJournal::sealed(path, launch);
    while begun.elapsed() < STARTUP_LIMIT {
        if let Some(status) = child.try_wait().context("poll Trailgen process")? {
            return Ok(Startup::Exited(status));
        }
        match journal.read_new::<Value>() {
            Ok(frames) => {
                for frame in frames {
                    if presented_project_deck(&frame)? {
                        return Ok(Startup::Ready);
                    }
                }
            }
            Err(WitnessError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read Trailgen witness"),
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(Startup::TimedOut)
}

fn presented_project_deck(frame: &Value) -> Result<bool> {
    let Some(state) = frame.get("state") else {
        return Ok(false);
    };
    let contract = state
        .get("contract")
        .and_then(Value::as_str)
        .context("witness state has no contract")?;
    ensure!(
        contract == trailgen_contract::UI_FINGERPRINT,
        "Trailgen UI contract mismatch: expected {}, observed {contract}",
        trailgen_contract::UI_FINGERPRINT
    );
    let presented = frame
        .get("surface_sequence")
        .and_then(Value::as_u64)
        .is_some_and(|sequence| sequence > 0);
    let workspace = state.get("workspace").and_then(Value::as_str);
    let view = state.get("view").and_then(Value::as_str);
    Ok(presented && workspace == Some("projects") && view == Some("projects"))
}

enum Startup {
    Ready,
    Exited(ExitStatus),
    TimedOut,
}

struct Cell {
    path: PathBuf,
    _temporary: Option<tempfile::TempDir>,
}

impl Cell {
    fn forge(label: &str) -> Result<Self> {
        if let Some(path) = std::env::var_os("TRAILGEN_PORTABILITY_ARTIFACTS") {
            let path = PathBuf::from(path).join(format!(
                "{}-{}-{}-{label}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                std::process::id()
            ));
            std::fs::create_dir_all(&path)
                .with_context(|| format!("create portability artifacts at {}", path.display()))?;
            return Ok(Self {
                path,
                _temporary: None,
            });
        }
        let temporary = tempfile::tempdir().context("forge portability cell")?;
        Ok(Self {
            path: temporary.path().to_path_buf(),
            _temporary: Some(temporary),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

struct Captive(Option<Child>);

impl Captive {
    const fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> Result<&mut Child> {
        self.0.as_mut().context("Trailgen child was already reaped")
    }

    fn finish(mut self) -> Result<Output> {
        let mut child = self.0.take().context("Trailgen child was already reaped")?;
        if child
            .try_wait()
            .context("poll Trailgen before teardown")?
            .is_none()
        {
            child
                .kill()
                .context("terminate Trailgen after portability proof")?;
        }
        child.wait_with_output().context("reap Trailgen process")
    }
}

impl Drop for Captive {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _killed = child.kill();
            let _reaped = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_deck_requires_contract_surface_workspace_and_view() -> Result<()> {
        let ready = json!({
            "surface_sequence": 1,
            "state": {
                "contract": trailgen_contract::UI_FINGERPRINT,
                "workspace": "projects",
                "view": "projects"
            },
        });
        assert!(presented_project_deck(&ready)?);
        assert!(!presented_project_deck(&json!({
            "surface_sequence": 0,
            "state": {
                "contract": trailgen_contract::UI_FINGERPRINT,
                "workspace": "projects",
                "view": "projects"
            },
        }))?);
        Ok(())
    }

    #[test]
    fn alien_contract_is_fatal() {
        let alien = json!({
            "surface_sequence": 1,
            "state": {
                "contract": "trailgen.ui/alien",
                "workspace": "projects",
                "view": "projects"
            },
        });
        assert!(presented_project_deck(&alien).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_known_folders_remain_inside_the_isolated_profile() -> Result<()> {
        let cell = tempfile::tempdir()?;
        let mut command = Command::new("trailgen.exe");
        isolate_host_paths(&mut command, cell.path())?;
        let environment = command
            .get_envs()
            .filter_map(|(name, value)| value.map(|value| (name, value)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let profile = Path::new(environment[std::ffi::OsStr::new("USERPROFILE")]);
        for variable in ["APPDATA", "LOCALAPPDATA"] {
            let path = Path::new(environment[std::ffi::OsStr::new(variable)]);
            assert!(path.starts_with(profile), "{variable} escaped USERPROFILE");
            assert!(path.is_dir(), "{variable} was not forged");
        }
        Ok(())
    }
}
