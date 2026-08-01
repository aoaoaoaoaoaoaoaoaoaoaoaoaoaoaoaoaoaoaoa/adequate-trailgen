//! Opt-in responsiveness evidence for the native application shell.
//!
//! `ETERNALIST_TRACE=/path/to/trace.json` records production-path spans in
//! Chrome trace format. Instrumentation is dormant when the variable is
//! absent; call sites then reduce to `tracing`'s disabled-span fast path.

use anyhow::{Context as _, Result};
use std::{
    fs::File,
    path::Path,
    time::{Duration, Instant},
};
use tracing_subscriber::{
    Layer as _, filter, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

pub const TRACE_PATH_ENV: &str = "ETERNALIST_TRACE";
pub const TRACE_SECONDS_ENV: &str = "ETERNALIST_TRACE_SECONDS";
const TRACE_TARGET_ROOT: &str = "eternalist";

/// Owns the trace writer until the native application terminates.
pub struct TraceGuard {
    writer: Option<tracing_chrome::FlushGuard>,
}

impl TraceGuard {
    /// Install the fleet trace collector when `ETERNALIST_TRACE` names a file.
    pub fn arm() -> Result<Self> {
        let Some(path) = std::env::var_os(TRACE_PATH_ENV) else {
            return Ok(Self { writer: None });
        };
        let path = Path::new(&path);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create trace directory {}", parent.display()))?;
        }
        let file = File::create(path)
            .with_context(|| format!("create responsiveness trace {}", path.display()))?;
        let (layer, writer) = tracing_chrome::ChromeLayerBuilder::new()
            .writer(file)
            .include_args(true)
            .include_locations(false)
            .build();
        let fleet_only = filter::filter_fn(|metadata| {
            metadata.target() == TRACE_TARGET_ROOT
                || metadata
                    .target()
                    .strip_prefix(TRACE_TARGET_ROOT)
                    .is_some_and(|suffix| suffix.starts_with("::"))
        });
        tracing_subscriber::registry()
            .with(layer.with_filter(fleet_only))
            .try_init()
            .context("install responsiveness trace collector")?;
        eprintln!("responsiveness trace: {}", path.display());
        Ok(Self {
            writer: Some(writer),
        })
    }

    /// Push all completed events to the trace file without ending collection.
    pub fn flush(&self) {
        if let Some(writer) = &self.writer {
            writer.flush();
        }
    }
}

pub(crate) fn deadline() -> Result<Option<Instant>> {
    let Some(raw) = std::env::var_os(TRACE_SECONDS_ENV) else {
        return Ok(None);
    };
    let seconds = raw
        .to_str()
        .context("ETERNALIST_TRACE_SECONDS is not Unicode")?
        .parse::<u64>()
        .context("ETERNALIST_TRACE_SECONDS is not a positive integer")?;
    anyhow::ensure!(seconds > 0, "ETERNALIST_TRACE_SECONDS must be positive");
    anyhow::ensure!(
        std::env::var_os(TRACE_PATH_ENV).is_some(),
        "ETERNALIST_TRACE_SECONDS requires ETERNALIST_TRACE"
    );
    Ok(Some(Instant::now() + Duration::from_secs(seconds)))
}
