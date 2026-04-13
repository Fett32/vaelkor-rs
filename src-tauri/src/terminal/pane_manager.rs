/// Pane manager — controls the vaelkor-main tmux session layout.
///
/// Architecture:
///   Each agent has its own durable tmux session (vaelkor-<agent>).
///   vaelkor-main is a display session whose panes attach to agent sessions.
///   This module manages pane creation, removal, and layout in vaelkor-main.
///
///   Pane command: `TMUX='' tmux new-session -A -t vaelkor-<agent>`
///   This attaches to the agent session inside a pane, keeping it durable.
///   If vaelkor-main dies, agent sessions survive. If the agent session dies,
///   the pane exits and can be respawned.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;

const MAIN_SESSION: &str = "vaelkor-main";
/// Maximum right-side columns before we start stacking more aggressively.
const MAX_RIGHT_COLUMNS: usize = 3;

// ---------------------------------------------------------------------------
// Pane tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PaneInfo {
    /// tmux pane ID (e.g. "%5")
    pane_id: String,
    /// Agent ID this pane shows
    agent_id: String,
    /// Which right-side column this pane belongs to (0-based). Orchestrator has None.
    column: Option<usize>,
}

// ---------------------------------------------------------------------------
// PaneManager
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct PaneManager {
    /// Maps agent_id → pane info
    panes: Arc<Mutex<HashMap<String, PaneInfo>>>,
}

impl PaneManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensure vaelkor-main exists. Creates it if needed.
    /// Called once at startup.
    pub async fn ensure_main_session(&self) -> Result<()> {
        if self.session_exists(MAIN_SESSION).await {
            tracing::info!("reusing existing {MAIN_SESSION} session");
            self.scan_existing_panes().await;
            return Ok(());
        }

        // Create with a placeholder — first real agent pane will replace it.
        let output = Command::new("tmux")
            .args([
                "new-session", "-d", "-s", MAIN_SESSION,
                "-x", "200", "-y", "50",
            ])
            .output()
            .await
            .context("create vaelkor-main session")?;

        if !output.status.success() {
            anyhow::bail!(
                "tmux new-session failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Put a status message in the initial pane.
        let _ = Command::new("tmux")
            .args([
                "send-keys", "-t", MAIN_SESSION,
                "echo 'Vaelkor — waiting for agents...'", "Enter",
            ])
            .output()
            .await;

        tracing::info!("created {MAIN_SESSION} session");
        Ok(())
    }

    /// Add an agent's session as a pane in vaelkor-main.
    /// The pane runs `tmux attach -t vaelkor-<agent>` so the agent session
    /// stays durable even if vaelkor-main is destroyed.
    pub async fn add_agent_pane(&self, agent_id: &str) -> Result<()> {
        let mut panes = self.panes.lock().await;

        // Already has a pane?
        if panes.contains_key(agent_id) {
            tracing::debug!(agent_id, "agent already has a pane in {MAIN_SESSION}");
            return Ok(());
        }

        let agent_session = format!("vaelkor-{agent_id}");
        let pane_count = self.count_panes().await;

        // The attach command — TMUX='' prevents "sessions should be nested" error.
        let attach_cmd = format!("TMUX='' tmux attach -t {agent_session}");

        let is_orchestrator = agent_id == "orchestrator";

        // Layout strategy:
        //   Orchestrator = full-height left column (pane 0, column=None).
        //   Other agents fill columns to the right.
        //   New column (-h split) until MAX_RIGHT_COLUMNS, then stack (-v split)
        //   within the column with fewest panes.

        // Count agents per right-side column.
        let mut column_counts: HashMap<usize, usize> = HashMap::new();
        let mut column_panes: HashMap<usize, String> = HashMap::new(); // column → any pane_id in it
        let mut max_column: Option<usize> = None;
        for info in panes.values() {
            if let Some(col) = info.column {
                *column_counts.entry(col).or_insert(0) += 1;
                column_panes.entry(col).or_insert_with(|| info.pane_id.clone());
                max_column = Some(max_column.map_or(col, |m: usize| m.max(col)));
            }
        }
        let num_columns = column_counts.len();

        let (pane_id, column) = if pane_count <= 1 && panes.is_empty() {
            // First real agent — use the existing initial pane.
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", &format!("{MAIN_SESSION}:0.0"), "C-c"])
                .output()
                .await;

            let output = Command::new("tmux")
                .args([
                    "respawn-pane", "-k",
                    "-t", &format!("{MAIN_SESSION}:0.0"),
                    &attach_cmd,
                ])
                .output()
                .await
                .context("respawn initial pane")?;

            if !output.status.success() {
                anyhow::bail!(
                    "respawn-pane failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }

            let pid = self.get_pane_id(MAIN_SESSION, 0).await
                .unwrap_or_else(|| "%0".to_string());
            let col = if is_orchestrator { None } else { Some(0) };
            (pid, col)

        } else if is_orchestrator {
            // Orchestrator always gets a new column on the left via -h split,
            // then we swap it to pane 0.
            let pid = self.split_horizontal(MAIN_SESSION, &attach_cmd).await?;
            // Swap to position 0 so it's on the left.
            let _ = Command::new("tmux")
                .args(["swap-pane", "-s", &pid, "-t", &format!("{MAIN_SESSION}:0.0")])
                .output()
                .await;
            let pid = self.get_pane_id(MAIN_SESSION, 0).await
                .unwrap_or(pid);
            (pid, None)

        } else if num_columns < MAX_RIGHT_COLUMNS {
            // Room for a new column — split horizontally (new column to the right).
            let pid = self.split_horizontal(MAIN_SESSION, &attach_cmd).await?;
            let col = max_column.map_or(0, |m| m + 1);
            (pid, Some(col))

        } else {
            // All columns full — stack in the column with fewest panes.
            let target_col = (0..num_columns)
                .min_by_key(|c| column_counts.get(c).copied().unwrap_or(0))
                .unwrap_or(0);

            let split_target = column_panes.get(&target_col)
                .cloned()
                .unwrap_or_else(|| format!("{MAIN_SESSION}:0.1"));

            let pid = self.split_vertical(&split_target, &attach_cmd).await?;
            (pid, Some(target_col))
        };

        // Rebalance: orchestrator gets left column, right side evens out.
        self.rebalance_layout().await;

        tracing::info!(agent_id, pane_id = %pane_id, column = ?column, "added agent pane to {MAIN_SESSION}");

        panes.insert(agent_id.to_string(), PaneInfo {
            pane_id,
            agent_id: agent_id.to_string(),
            column,
        });

        Ok(())
    }

    /// Remove an agent's pane from vaelkor-main.
    pub async fn remove_agent_pane(&self, agent_id: &str) -> Result<()> {
        let mut panes = self.panes.lock().await;

        let info = match panes.remove(agent_id) {
            Some(info) => info,
            None => return Ok(()),  // No pane to remove.
        };

        // Don't kill the last pane — tmux would destroy the session.
        let pane_count = self.count_panes().await;
        if pane_count <= 1 {
            tracing::info!(agent_id, "last pane, keeping placeholder");
            // Respawn as a placeholder instead of killing.
            let _ = Command::new("tmux")
                .args([
                    "respawn-pane", "-k",
                    "-t", &info.pane_id,
                    "echo", "Vaelkor — waiting for agents...",
                ])
                .output()
                .await;
            return Ok(());
        }

        let output = Command::new("tmux")
            .args(["kill-pane", "-t", &info.pane_id])
            .output()
            .await
            .context("kill agent pane")?;

        if !output.status.success() {
            tracing::warn!(
                agent_id,
                "kill-pane failed (pane may already be gone): {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        self.rebalance_layout().await;

        tracing::info!(agent_id, "removed agent pane from {MAIN_SESSION}");
        Ok(())
    }

    /// Check if an agent has a visible pane.
    pub async fn has_pane(&self, agent_id: &str) -> bool {
        self.panes.lock().await.contains_key(agent_id)
    }

    /// Get list of agents with visible panes.
    pub async fn visible_agents(&self) -> Vec<String> {
        self.panes.lock().await.keys().cloned().collect()
    }

    /// Split horizontally (new column to the right).
    async fn split_horizontal(&self, session: &str, cmd: &str) -> Result<String> {
        let output = Command::new("tmux")
            .args([
                "split-window", "-t", session,
                "-h",  // new column
                "-P", "-F", "#{pane_id}",
                cmd,
            ])
            .env("TMUX", "")
            .output()
            .await
            .context("split-window -h")?;

        if !output.status.success() {
            anyhow::bail!(
                "split-window -h failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Split vertically within an existing pane (stack top/bottom).
    async fn split_vertical(&self, target: &str, cmd: &str) -> Result<String> {
        let output = Command::new("tmux")
            .args([
                "split-window", "-t", target,
                "-v",  // stack within column
                "-P", "-F", "#{pane_id}",
                cmd,
            ])
            .env("TMUX", "")
            .output()
            .await
            .context("split-window -v")?;

        if !output.status.success() {
            anyhow::bail!(
                "split-window -v failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Rebalance the pane layout in vaelkor-main.
    /// Uses main-vertical: orchestrator gets full left column, agents stack on right.
    async fn rebalance_layout(&self) {
        let _ = Command::new("tmux")
            .args(["select-layout", "-t", MAIN_SESSION, "main-vertical"])
            .output()
            .await;
    }

    /// Count current panes in vaelkor-main.
    async fn count_panes(&self) -> usize {
        let output = Command::new("tmux")
            .args(["list-panes", "-t", MAIN_SESSION, "-F", "#{pane_id}"])
            .output()
            .await;

        match output {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .count()
            }
            _ => 0,
        }
    }

    /// Get the pane ID for a specific pane index.
    async fn get_pane_id(&self, session: &str, index: usize) -> Option<String> {
        let target = format!("{session}:0.{index}");
        let output = Command::new("tmux")
            .args(["display-message", "-t", &target, "-p", "#{pane_id}"])
            .output()
            .await
            .ok()?;

        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            None
        }
    }

    /// Scan existing panes in vaelkor-main and populate the in-memory map.
    /// Called on startup when reusing an existing session to prevent duplicates.
    async fn scan_existing_panes(&self) {
        let output = Command::new("tmux")
            .args([
                "list-panes", "-t", MAIN_SESSION,
                "-F", "#{pane_id} #{pane_start_command}",
            ])
            .output()
            .await;

        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return,
        };

        let mut panes = self.panes.lock().await;
        let text = String::from_utf8_lossy(&output.stdout);

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Format: "%5 TMUX='' tmux attach -t vaelkor-claude"
            // Extract pane_id and agent_id from the attach target.
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            if parts.len() < 2 {
                continue;
            }
            let pane_id = parts[0].to_string();
            let cmd = parts[1];

            // Look for "vaelkor-<agent>" in the command.
            if let Some(pos) = cmd.find("vaelkor-") {
                let after = &cmd[pos + 8..]; // skip "vaelkor-"
                let agent_id = after
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches('"')
                    .to_string();

                if !agent_id.is_empty() && !panes.contains_key(&agent_id) {
                    // Orchestrator has no column; others get auto-assigned.
                    let column = if agent_id == "orchestrator" {
                        None
                    } else {
                        // Assign to next available column based on scan order.
                        let existing_cols: std::collections::HashSet<usize> = panes.values()
                            .filter_map(|p| p.column)
                            .collect();
                        let next = (0..).find(|c| !existing_cols.contains(c)).unwrap_or(0);
                        Some(next)
                    };
                    tracing::info!(agent_id, pane_id, column = ?column, "found existing pane on scan");
                    panes.insert(agent_id.clone(), PaneInfo {
                        pane_id,
                        agent_id,
                        column,
                    });
                }
            }
        }

        tracing::info!(count = panes.len(), "scanned existing panes in {MAIN_SESSION}");
    }

    /// Check if a tmux session exists.
    async fn session_exists(&self, name: &str) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", name])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
