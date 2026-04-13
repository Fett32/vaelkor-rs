/// Socket server for wrapper connections.
///
/// Listens on /tmp/vaelkor/daemon.sock and handles:
/// - wrapper.register — wrapper announces its agent_id
/// - task.accept/complete/blocked — task state updates
/// - status.response — heartbeat replies
///
/// Outbound messages (task.assign, status.request) are sent via the
/// connection registry.

use crate::daemon::state::{AppState, TaskState};
use crate::wrapper::protocol::{
    Envelope, TaskAccept, TaskBlocked, TaskComplete, WrapperError, WrapperRegister,
    MSG_ERROR, MSG_REGISTER, MSG_TASK_ACCEPT, MSG_TASK_BLOCKED, MSG_TASK_COMPLETE,
    MSG_STATUS_RESPONSE,
};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

pub const DAEMON_SOCKET: &str = "/tmp/vaelkor/daemon.sock";

/// A connected wrapper's write half, keyed by agent_id.
type WriterMap = Arc<Mutex<HashMap<String, tokio::net::unix::OwnedWriteHalf>>>;

/// Shared state for the socket server.
#[derive(Clone)]
pub struct SocketServer {
    writers: WriterMap,
    app_state: AppState,
}

impl SocketServer {
    pub fn new(app_state: AppState) -> Self {
        Self {
            writers: Arc::new(Mutex::new(HashMap::new())),
            app_state,
        }
    }

    /// Start listening. Call this in a spawned task.
    pub async fn run(&self) -> Result<()> {
        // Ensure parent directory exists
        let sock_path = Path::new(DAEMON_SOCKET);
        if let Some(parent) = sock_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }

        // Remove stale socket file
        if sock_path.exists() {
            tokio::fs::remove_file(sock_path).await.ok();
        }

        let listener = UnixListener::bind(sock_path)
            .context("bind daemon socket")?;
        info!(path = DAEMON_SOCKET, "socket server listening");

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let server = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = server.handle_connection(stream).await {
                            warn!("connection handler error: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    error!("accept error: {e}");
                }
            }
        }
    }

    /// Handle one wrapper connection.
    async fn handle_connection(&self, stream: UnixStream) -> Result<()> {
        let (read_half, write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();

        // First message must be wrapper.register
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(()); // EOF before register
        }

        let env: Envelope = serde_json::from_str(line.trim())
            .context("parse register envelope")?;

        if env.kind != MSG_REGISTER {
            anyhow::bail!("first message must be wrapper.register, got {}", env.kind);
        }

        let reg: WrapperRegister = env.decode_payload()
            .context("decode WrapperRegister")?;
        let agent_id = reg.agent_id.clone();

        info!(agent_id = %agent_id, "wrapper registered");

        // Store the write half
        {
            let mut writers = self.writers.lock().await;
            writers.insert(agent_id.clone(), write_half);
        }

        // Update agent status in app state
        self.app_state.set_agent_connected(&agent_id, true);

        // Read loop
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                info!(agent_id = %agent_id, "wrapper disconnected");
                break;
            }

            let env: Envelope = match serde_json::from_str(line.trim()) {
                Ok(e) => e,
                Err(e) => {
                    warn!(agent_id = %agent_id, "malformed message: {e}");
                    continue;
                }
            };

            self.handle_message(&agent_id, env).await;
        }

        // Cleanup on disconnect
        {
            let mut writers = self.writers.lock().await;
            writers.remove(&agent_id);
        }
        self.app_state.set_agent_connected(&agent_id, false);

        Ok(())
    }

    /// Handle one envelope from a wrapper.
    async fn handle_message(&self, agent_id: &str, env: Envelope) {
        match env.kind.as_str() {
            MSG_TASK_ACCEPT => {
                if let Ok(payload) = env.decode_payload::<TaskAccept>() {
                    info!(agent_id, task_id = %payload.task_id, "task accepted");
                    if let Err(e) = self.app_state.transition_task(payload.task_id, TaskState::Accepted) {
                        warn!("transition to Accepted failed: {e}");
                    }
                }
            }

            MSG_TASK_COMPLETE => {
                if let Ok(payload) = env.decode_payload::<TaskComplete>() {
                    info!(agent_id, task_id = %payload.task_id, "task complete");
                    if let Err(e) = self.app_state.transition_task(payload.task_id, TaskState::Completed) {
                        warn!("transition to Completed failed: {e}");
                    }
                }
            }

            MSG_TASK_BLOCKED => {
                if let Ok(payload) = env.decode_payload::<TaskBlocked>() {
                    info!(agent_id, task_id = %payload.task_id, reason = %payload.reason, "task blocked");
                    if let Err(e) = self.app_state.transition_task(payload.task_id, TaskState::Blocked) {
                        warn!("transition to Blocked failed: {e}");
                    }
                }
            }

            MSG_STATUS_RESPONSE => {
                // Could update heartbeat timestamp here
                info!(agent_id, "status response received");
            }

            MSG_ERROR => {
                if let Ok(payload) = env.decode_payload::<WrapperError>() {
                    error!(agent_id, message = %payload.message, "wrapper error");
                }
            }

            other => {
                warn!(agent_id, kind = other, "unknown message type");
            }
        }
    }

    /// Send an envelope to a specific wrapper.
    pub async fn send_to(&self, agent_id: &str, envelope: &Envelope) -> Result<()> {
        let mut writers = self.writers.lock().await;
        let writer = writers.get_mut(agent_id)
            .ok_or_else(|| anyhow::anyhow!("no connection for agent {agent_id}"))?;

        let mut line = serde_json::to_string(envelope)?;
        line.push('\n');
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;

        Ok(())
    }

    /// Check if a wrapper is connected.
    pub async fn is_connected(&self, agent_id: &str) -> bool {
        self.writers.lock().await.contains_key(agent_id)
    }

    /// List all connected agent IDs.
    pub async fn connected_agents(&self) -> Vec<String> {
        self.writers.lock().await.keys().cloned().collect()
    }
}
