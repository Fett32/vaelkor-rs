/// Terminal bridge — connects xterm.js (frontend) to tmux (backend).
///
/// Architecture:
///   Frontend xterm.js  ←→  Tauri IPC  ←→  this bridge  ←→  tmux
///
/// tmux owns sessions.  Rust's job is:
///   1. know which tmux session belongs to which agent (vaelkor-<agent>)
///   2. send user input via `tmux send-keys`
///   3. stream output via `tmux capture-pane` polling + Tauri events

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A chunk of terminal output sent from the bridge to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalChunk {
    pub agent_id: String,
    pub data: String,
}

/// Tracks the last captured content to send only diffs.
#[derive(Default)]
struct CaptureState {
    last_content: String,
    last_cursor_y: usize,
}

// ---------------------------------------------------------------------------
// Bridge
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct TerminalBridge {
    /// Track capture state per agent to detect changes.
    states: Arc<Mutex<HashMap<String, CaptureState>>>,
    /// Agents currently being streamed.
    active: Arc<Mutex<HashMap<String, bool>>>,
}

impl TerminalBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the tmux session name for an agent.
    fn session_name(agent_id: &str) -> String {
        format!("vaelkor-{}", agent_id)
    }

    /// Send keystrokes to an agent's tmux session.
    pub async fn send_keys(&self, agent_id: &str, keys: &str) -> Result<()> {
        let session = Self::session_name(agent_id);
        let target = format!("{}:0.0", session);

        let output = Command::new("tmux")
            .args(["send-keys", "-t", &target, "-l", keys])
            .output()
            .await
            .context("tmux send-keys spawn failed")?;

        if !output.status.success() {
            anyhow::bail!(
                "tmux send-keys failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }

    /// Capture the current pane content.
    pub async fn capture_pane(&self, agent_id: &str) -> Result<String> {
        let session = Self::session_name(agent_id);
        let target = format!("{}:0.0", session);

        let output = Command::new("tmux")
            .args(["capture-pane", "-t", &target, "-p", "-e"])
            .output()
            .await
            .context("tmux capture-pane spawn failed")?;

        if !output.status.success() {
            anyhow::bail!(
                "tmux capture-pane failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Check if a session exists.
    pub async fn session_exists(&self, agent_id: &str) -> bool {
        let session = Self::session_name(agent_id);
        Command::new("tmux")
            .args(["has-session", "-t", &session])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Start streaming for an agent. Returns true if started, false if already active.
    pub async fn start_streaming(&self, agent_id: &str) -> bool {
        let mut active = self.active.lock().await;
        if active.get(agent_id).copied().unwrap_or(false) {
            return false;
        }
        active.insert(agent_id.to_string(), true);

        // Initialize capture state
        let mut states = self.states.lock().await;
        states.insert(agent_id.to_string(), CaptureState::default());

        true
    }

    /// Stop streaming for an agent.
    pub async fn stop_streaming(&self, agent_id: &str) {
        self.active.lock().await.remove(agent_id);
        self.states.lock().await.remove(agent_id);
    }

    /// Check if streaming is active for an agent.
    pub async fn is_streaming(&self, agent_id: &str) -> bool {
        self.active.lock().await.get(agent_id).copied().unwrap_or(false)
    }

    /// Get all agent IDs currently being streamed.
    pub async fn streaming_agents(&self) -> Vec<String> {
        self.active.lock().await
            .iter()
            .filter(|(_, &v)| v)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Poll for changes and return new content if any.
    /// Returns Some(new_content) if there's new data, None if unchanged.
    pub async fn poll_changes(&self, agent_id: &str) -> Option<String> {
        if !self.is_streaming(agent_id).await {
            return None;
        }

        let current = match self.capture_pane(agent_id).await {
            Ok(c) => c,
            Err(_) => return None,
        };

        let mut states = self.states.lock().await;
        let state = states.entry(agent_id.to_string()).or_default();

        if current != state.last_content {
            state.last_content = current.clone();
            // For simplicity, if content changed, send the whole thing
            // A smarter diff could be done but capture-pane is already efficient
            Some(current)
        } else {
            None
        }
    }

    /// Get the full current content (for initial load).
    pub async fn get_full_content(&self, agent_id: &str) -> Result<String> {
        self.capture_pane(agent_id).await
    }
}
