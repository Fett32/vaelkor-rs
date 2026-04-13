/// Terminal bridge — connects xterm.js (frontend) to tmux (backend).
///
/// Architecture (revised):
///   Frontend has a single xterm.js instance rendering vaelkor-main.
///   vaelkor-main is a tmux session with panes linked to agent sessions.
///   This bridge polls vaelkor-main and pipes the output to xterm.js.
///   User input goes to vaelkor-main, tmux routes it to the active pane.
///
///   One poller, one xterm.js, one stream. tmux handles the tiling.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;

const MAIN_SESSION: &str = "vaelkor-main";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A chunk of terminal output sent from the bridge to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalChunk {
    pub data: String,
}

// ---------------------------------------------------------------------------
// Bridge
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct TerminalBridge {
    /// Last captured content, for change detection.
    last_content: Arc<Mutex<String>>,
    /// Whether streaming is active.
    streaming: Arc<Mutex<bool>>,
}

impl TerminalBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Send keystrokes to vaelkor-main (tmux routes to active pane).
    pub async fn send_keys(&self, keys: &str) -> Result<()> {
        let output = Command::new("tmux")
            .args(["send-keys", "-t", MAIN_SESSION, "-l", keys])
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

    /// Capture the full vaelkor-main content (all panes visible).
    pub async fn capture(&self) -> Result<String> {
        let output = Command::new("tmux")
            .args(["capture-pane", "-t", MAIN_SESSION, "-p", "-e"])
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

    /// Check if vaelkor-main exists.
    pub async fn session_exists(&self) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", MAIN_SESSION])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Start streaming. Returns true if started, false if already active.
    pub async fn start_streaming(&self) -> bool {
        let mut streaming = self.streaming.lock().await;
        if *streaming {
            return false;
        }
        *streaming = true;
        true
    }

    /// Stop streaming.
    pub async fn stop_streaming(&self) {
        *self.streaming.lock().await = false;
    }

    /// Check if streaming is active.
    pub async fn is_streaming(&self) -> bool {
        *self.streaming.lock().await
    }

    /// Poll for changes. Returns Some(new_content) if changed, None if same.
    pub async fn poll_changes(&self) -> Option<String> {
        if !self.is_streaming().await {
            return None;
        }

        let current = match self.capture().await {
            Ok(c) => c,
            Err(_) => return None,
        };

        let mut last = self.last_content.lock().await;
        if current != *last {
            *last = current.clone();
            Some(current)
        } else {
            None
        }
    }

    /// Get full current content (for initial load).
    pub async fn get_full_content(&self) -> Result<String> {
        self.capture().await
    }
}
