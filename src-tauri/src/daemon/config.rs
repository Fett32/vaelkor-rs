/// Agent configuration loaded from ~/.config/vaelkor/agents/*.yaml.
///
/// Each YAML file defines one agent. The filename (minus .yaml) is the agent ID.
/// On startup, all configs are loaded and agents are auto-registered.
/// Agents with `autolaunch: true` get their wrappers spawned automatically.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Child;

use super::session;
use super::state::{Agent, AppState};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns the user's home directory as a PathBuf.
fn dirs_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

// ---------------------------------------------------------------------------
// Config structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Display symbol for the UI.
    #[serde(default)]
    pub identity: Option<String>,
    /// Agent role: coder, orchestrator, reviewer, etc.
    #[serde(default = "default_role")]
    pub role: String,
    /// Command to launch the agent in its tmux session.
    #[serde(default)]
    pub command: Vec<String>,
    /// Whether to auto-launch a wrapper for this agent on startup.
    #[serde(default)]
    pub autolaunch: bool,
    /// Whether the wrapper should auto-connect to the daemon.
    #[serde(default)]
    pub autoconnect: bool,
    /// Constraints on what the agent can do.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Agent tier: "light", "heavy", etc.
    #[serde(default = "default_tier")]
    pub tier: String,
    /// Maximum concurrent tasks this agent can handle.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// What this agent should be used for (e.g. ["code", "review"]).
    #[serde(default)]
    pub use_for: Vec<String>,
    /// Working directory for the agent session.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Startup file to inject into the agent session.
    #[serde(default)]
    pub startup_file: Option<String>,
}

fn default_role() -> String {
    "coder".to_string()
}

fn default_tier() -> String {
    "light".to_string()
}

fn default_max_concurrent() -> u32 {
    1
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load all agent configs from ~/.config/vaelkor/agents/*.yaml.
/// Returns (agent_id, config) pairs. The agent_id is the filename stem.
pub fn load_agent_configs() -> Result<Vec<(String, AgentConfig)>> {
    let config_dir = session::config_dir()?;
    let agents_dir = config_dir.join("agents");

    if !agents_dir.exists() {
        tracing::info!("no agents config dir at {}", agents_dir.display());
        return Ok(vec![]);
    }

    let mut configs = Vec::new();

    let entries = std::fs::read_dir(&agents_dir)
        .with_context(|| format!("failed to read {}", agents_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }

        let agent_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        if let Some(id) = agent_id {
            match load_one(&path) {
                Ok(cfg) => {
                    tracing::info!(agent_id = %id, role = %cfg.role, "loaded agent config");
                    configs.push((id, cfg));
                }
                Err(e) => {
                    tracing::warn!("failed to load {}: {e:#}", path.display());
                }
            }
        }
    }

    Ok(configs)
}

fn load_one(path: &Path) -> Result<AgentConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: AgentConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(config)
}

/// Register agents from config into AppState.
/// Only registers agents that aren't already registered.
pub fn register_agents_from_config(state: &AppState, configs: &[(String, AgentConfig)]) {
    let existing = state.all_agents();
    let existing_ids: std::collections::HashSet<&str> =
        existing.iter().map(|a| a.id.as_str()).collect();

    for (id, cfg) in configs {
        if existing_ids.contains(id.as_str()) {
            tracing::debug!(agent_id = %id, "agent already registered, skipping");
            continue;
        }

        let display_name = if let Some(ref identity) = cfg.identity {
            format!("{identity} {id}")
        } else {
            id.clone()
        };

        let mut agent = Agent::new(id.clone(), display_name);
        agent.tmux_session = Some(format!("vaelkor-{id}"));
        state.register_agent(agent);

        tracing::info!(agent_id = %id, role = %cfg.role, "agent registered from config");
    }
}

// ---------------------------------------------------------------------------
// Wrapper auto-launch
// ---------------------------------------------------------------------------

/// Find the vaelkor-wrapper binary. Looks next to the current executable first,
/// then falls back to PATH.
pub fn find_wrapper_binary() -> Result<PathBuf> {
    // Try next to the current executable (workspace builds put both in target/debug/).
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent().unwrap_or(Path::new(".")).join("vaelkor-wrapper");
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    // Fall back to PATH lookup.
    if let Ok(output) = std::process::Command::new("which")
        .arg("vaelkor-wrapper")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    anyhow::bail!("vaelkor-wrapper binary not found")
}

/// Spawn wrapper processes for agents with `autolaunch: true`.
/// Returns the child processes so the caller can track/kill them.
pub fn launch_wrappers(configs: &[(String, AgentConfig)]) -> Vec<(String, Child)> {
    let wrapper_bin = match find_wrapper_binary() {
        Ok(bin) => {
            tracing::info!(path = %bin.display(), "found vaelkor-wrapper binary");
            bin
        }
        Err(e) => {
            tracing::warn!("cannot auto-launch wrappers: {e}");
            return vec![];
        }
    };

    let mut children = Vec::new();

    for (id, cfg) in configs {
        if !cfg.autolaunch {
            tracing::debug!(agent_id = %id, "autolaunch disabled, skipping");
            continue;
        }

        let mut cmd = std::process::Command::new(&wrapper_bin);
        cmd.arg(id);

        // Pass the command if specified.
        if !cfg.command.is_empty() {
            cmd.arg("--command").arg(cfg.command.join(" "));
        }

        // Pass working directory with ~ expansion.
        if let Some(ref wd) = cfg.working_dir {
            let expanded = if wd.starts_with('~') {
                dirs_home().join(wd.strip_prefix("~/").unwrap_or(&wd[1..]))
            } else {
                PathBuf::from(wd)
            };
            cmd.arg("--workdir").arg(expanded);
        }

        // Pass startup file with ~ expansion.
        if let Some(ref sf) = cfg.startup_file {
            let expanded = if sf.starts_with('~') {
                dirs_home().join(sf.strip_prefix("~/").unwrap_or(&sf[1..]))
            } else {
                PathBuf::from(sf)
            };
            cmd.arg("--startup-file").arg(expanded);
        }

        match cmd
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
        {
            Ok(child) => {
                tracing::info!(agent_id = %id, pid = child.id(), "wrapper auto-launched");
                children.push((id.clone(), child));
            }
            Err(e) => {
                tracing::error!(agent_id = %id, "failed to launch wrapper: {e}");
            }
        }
    }

    children
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

pub const DEFAULT_RULES: &str = r#"# Vaelkor Agent Rules

These rules are injected into every agent session managed by Vaelkor.
Edit this file to customize agent behavior. Reset to defaults by deleting
this file — Vaelkor will recreate it on next launch.

## Core Rules

- Do not edit files without explicit authorization from the user or orchestrator.
- Propose changes and wait for approval before making them.
- Keep diffs minimal — only change what is needed for the task.
- Do not add features, refactoring, or cleanup beyond what was asked.
- Ask before acting on ambiguous instructions.

## Context Discipline

- Read only the files and docs pointed to in your task context.
- Do not explore the broader codebase unless the task requires it.
- If you need information not in your context, ask the orchestrator.

## Communication

- Report completion clearly — state what was done and what changed.
- If blocked, explain what you need and from whom.
- If you discover something relevant to other tasks, note it in your completion summary.

## Safety

- Never commit directly to main/master without explicit permission.
- Never run destructive commands (rm -rf, DROP TABLE, force push) without confirmation.
- Never expose secrets, credentials, or API keys in output or commits.
"#;

/// Load rules from ~/.config/vaelkor/rules.md.
/// If the file doesn't exist, create it with DEFAULT_RULES.
pub fn load_rules() -> Result<String> {
    let config_dir = session::config_dir()?;
    let rules_path = config_dir.join("rules.md");

    if rules_path.exists() {
        let content = std::fs::read_to_string(&rules_path)
            .with_context(|| format!("failed to read {}", rules_path.display()))?;
        Ok(content)
    } else {
        // Create the config dir if needed, then write defaults.
        std::fs::create_dir_all(&config_dir)
            .with_context(|| format!("failed to create {}", config_dir.display()))?;
        std::fs::write(&rules_path, DEFAULT_RULES)
            .with_context(|| format!("failed to write {}", rules_path.display()))?;
        tracing::info!(path = %rules_path.display(), "created default rules.md");
        Ok(DEFAULT_RULES.to_string())
    }
}

// ---------------------------------------------------------------------------
// Task context formatting
// ---------------------------------------------------------------------------

/// Format a task description with full context for an agent.
pub fn format_task_context(
    project: Option<&str>,
    instance: &str,
    task_id: &str,
    summary: &str,
    files: &[String],
    refs: &[String],
    body: &str,
) -> String {
    let mut ctx = String::new();

    ctx.push_str("# Task Assignment\n\n");

    if let Some(proj) = project {
        ctx.push_str(&format!("**Project:** {proj}\n"));
    }
    ctx.push_str(&format!("**Instance:** {instance}\n"));
    ctx.push_str(&format!("**Task ID:** {task_id}\n"));
    ctx.push_str(&format!("**Summary:** {summary}\n\n"));

    if !files.is_empty() {
        ctx.push_str("## Relevant Files\n\n");
        for f in files {
            ctx.push_str(&format!("- {f}\n"));
        }
        ctx.push('\n');
    }

    if !refs.is_empty() {
        ctx.push_str("## References\n\n");
        for r in refs {
            ctx.push_str(&format!("- {r}\n"));
        }
        ctx.push('\n');
    }

    if !body.is_empty() {
        ctx.push_str("## Details\n\n");
        ctx.push_str(body);
        ctx.push('\n');
    }

    ctx
}
