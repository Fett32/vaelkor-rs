/// Session persistence helpers.
///
/// Vaelkor uses three directories:
///   config  → ~/.config/vaelkor/
///   data    → ~/.local/share/vaelkor/
///   sockets → /tmp/vaelkor/
///
/// `ensure_dirs()` must be called at startup before any path is used.

use anyhow::Context;
use directories::ProjectDirs;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

fn project_dirs() -> anyhow::Result<ProjectDirs> {
    ProjectDirs::from("", "", "vaelkor")
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))
}

pub fn config_dir() -> anyhow::Result<PathBuf> {
    Ok(project_dirs()?.config_dir().to_path_buf())
}

pub fn data_dir() -> anyhow::Result<PathBuf> {
    Ok(project_dirs()?.data_dir().to_path_buf())
}

pub fn socket_dir() -> PathBuf {
    PathBuf::from("/tmp/vaelkor")
}

pub fn socket_path(agent_id: &str) -> PathBuf {
    socket_dir().join(format!("{agent_id}.sock"))
}

// ---------------------------------------------------------------------------
// Ensure all directories exist at startup
// ---------------------------------------------------------------------------

pub fn ensure_dirs() -> anyhow::Result<()> {
    let config = config_dir().context("config dir")?;
    let data = data_dir().context("data dir")?;
    let sockets = socket_dir();

    std::fs::create_dir_all(&config)
        .with_context(|| format!("create {}", config.display()))?;
    std::fs::create_dir_all(&data)
        .with_context(|| format!("create {}", data.display()))?;
    std::fs::create_dir_all(&sockets)
        .with_context(|| format!("create {}", sockets.display()))?;

    tracing::debug!(
        config = %config.display(),
        data = %data.display(),
        sockets = %sockets.display(),
        "runtime directories ready"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Session file (simple JSON snapshot)
// ---------------------------------------------------------------------------

/// Path to the running-session snapshot file.
pub fn session_file() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("session.json"))
}

/// Lightweight metadata written to disk so the UI can show "last session" info.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub pid: u32,
    pub version: String,
}

impl SessionInfo {
    pub fn current() -> Self {
        Self {
            started_at: chrono::Utc::now(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn write(&self) -> anyhow::Result<()> {
        let path = session_file()?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)
            .with_context(|| format!("write session file {}", path.display()))?;
        Ok(())
    }

    pub fn read() -> anyhow::Result<Self> {
        let path = session_file()?;
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read session file {}", path.display()))?;
        Ok(serde_json::from_str(&raw)?)
    }
}
