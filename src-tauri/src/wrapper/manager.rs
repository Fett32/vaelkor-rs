/// Wrapper connection registry.
///
/// Each agent wrapper exposes a Unix socket at /tmp/vaelkor/<id>.sock.
/// The manager keeps track of which wrappers are connected and provides
/// async send helpers.
///
/// Currently this is a thin stub — actual socket I/O will be wired up
/// once the frontend and wrapper binary are ready.

use crate::daemon::session::socket_path;
use crate::wrapper::protocol::{Envelope, TaskAssign, MSG_TASK_ASSIGN};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// WrapperHandle — represents one connected wrapper
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WrapperHandle {
    pub agent_id: String,
    pub socket: std::path::PathBuf,
    /// true once we've confirmed the socket is answering
    pub alive: bool,
}

impl WrapperHandle {
    pub fn new(agent_id: impl Into<String>) -> Self {
        let id = agent_id.into();
        let socket = socket_path(&id);
        Self {
            agent_id: id,
            socket,
            alive: false,
        }
    }

    /// Attempt to write one newline-delimited JSON message to the socket.
    ///
    /// This is a fire-and-forget best-effort send.  The orchestrator should
    /// also update local state optimistically and rely on timeouts / status
    /// polling to detect failures.
    pub async fn send(&self, envelope: &Envelope) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(&self.socket).await?;
        let mut line = serde_json::to_string(envelope)?;
        line.push('\n');
        stream.write_all(line.as_bytes()).await?;
        stream.flush().await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// WrapperManager
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct WrapperManager {
    handles: Arc<Mutex<HashMap<String, WrapperHandle>>>,
}

impl WrapperManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, handle: WrapperHandle) {
        let mut map = self.handles.lock().unwrap();
        tracing::info!(agent_id = %handle.agent_id, "wrapper registered");
        map.insert(handle.agent_id.clone(), handle);
    }

    pub fn remove(&self, agent_id: &str) {
        self.handles.lock().unwrap().remove(agent_id);
        tracing::info!(agent_id, "wrapper removed");
    }

    pub fn get(&self, agent_id: &str) -> Option<WrapperHandle> {
        self.handles.lock().unwrap().get(agent_id).cloned()
    }

    pub fn all(&self) -> Vec<WrapperHandle> {
        self.handles.lock().unwrap().values().cloned().collect()
    }

    /// Assign a task to a specific wrapper (fire-and-forget async).
    /// Caller is responsible for updating AppState.
    pub async fn send_task_assign(
        &self,
        agent_id: &str,
        task_id: Uuid,
        title: &str,
        description: &str,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<()> {
        let handle = self
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("no wrapper registered for agent {agent_id}"))?;

        let payload = TaskAssign {
            task_id,
            title: title.to_string(),
            description: description.to_string(),
            timeout_secs,
        };
        let envelope = Envelope::new(MSG_TASK_ASSIGN, &payload)?;
        handle.send(&envelope).await?;
        Ok(())
    }
}
