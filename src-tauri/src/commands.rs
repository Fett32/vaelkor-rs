/// Tauri IPC command handlers.
///
/// These are the functions the frontend calls via `invoke(...)`.
/// All commands receive the shared AppState via Tauri's managed state.

use crate::daemon::session::SessionInfo;
use crate::daemon::state::{Agent, AppState, Task, TaskState};
use crate::terminal::bridge::TerminalBridge;
use crate::wrapper::protocol::{Envelope, TaskAssign, MSG_TASK_ASSIGN};
use crate::wrapper::server::SocketServer;
use tauri::State;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Error helper — Tauri commands must return String errors for serialization
// ---------------------------------------------------------------------------

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Task commands
// ---------------------------------------------------------------------------

/// Return all tasks (unsorted).
#[tauri::command]
pub fn get_tasks(state: State<'_, AppState>) -> Vec<Task> {
    state.all_tasks()
}

/// Return one task by UUID string.
#[tauri::command]
pub fn get_task(state: State<'_, AppState>, id: String) -> Result<Task, String> {
    let uuid = Uuid::parse_str(&id).map_err(err)?;
    state.get_task(uuid).ok_or_else(|| format!("task {id} not found"))
}

/// Create a new task and assign it to an agent.
///
/// If agent_id is provided and the wrapper is connected, the task is sent
/// to the wrapper immediately. If dispatch fails, task transitions to Stale.
#[tauri::command]
pub async fn assign_task(
    state: State<'_, AppState>,
    server: State<'_, SocketServer>,
    title: String,
    description: String,
    agent_id: Option<String>,
) -> Result<Task, String> {
    let mut task = Task::new(title.clone(), description.clone());

    if let Some(ref aid) = agent_id {
        task.assigned_to = Some(aid.clone());
    }

    let task_id = task.id;
    state.add_task(task.clone());

    tracing::info!(
        task_id = %task_id,
        agent = ?agent_id,
        "task created"
    );

    // Send to wrapper if agent is specified
    if let Some(ref aid) = agent_id {
        if !server.is_connected(aid).await {
            // Agent not connected - mark task as Stale immediately
            tracing::warn!(agent_id = aid, "wrapper not connected, marking task Stale");
            let _ = state.transition_task(task_id, TaskState::Stale);
            return state.get_task(task_id).ok_or_else(|| "task not found".to_string());
        }

        let payload = TaskAssign {
            task_id,
            title,
            description,
            timeout_secs: None,
        };

        let envelope = Envelope::new(MSG_TASK_ASSIGN, &payload).map_err(err)?;

        if let Err(e) = server.send_to(aid, &envelope).await {
            // Dispatch failed - mark task as Stale
            tracing::error!(agent_id = aid, "dispatch failed: {e}, marking task Stale");
            let _ = state.transition_task(task_id, TaskState::Stale);
            return state.get_task(task_id).ok_or_else(|| "task not found".to_string());
        }

        tracing::info!(task_id = %task_id, agent_id = aid, "task dispatched to wrapper");
    }

    state.get_task(task_id).ok_or_else(|| "task not found".to_string())
}

/// Cancel a task by UUID string.
#[tauri::command]
pub fn cancel_task(state: State<'_, AppState>, id: String) -> Result<Task, String> {
    let uuid = Uuid::parse_str(&id).map_err(err)?;
    state
        .transition_task(uuid, TaskState::Cancelled)
        .map_err(err)
}

// ---------------------------------------------------------------------------
// Agent commands
// ---------------------------------------------------------------------------

/// Return all registered agents.
#[tauri::command]
pub fn get_agents(state: State<'_, AppState>) -> Vec<Agent> {
    state.all_agents()
}

/// Register a new agent.  If an agent with this ID already exists it is
/// overwritten (useful for reconnects).
#[tauri::command]
pub fn register_agent(
    state: State<'_, AppState>,
    id: String,
    name: String,
    tmux_session: Option<String>,
) -> Agent {
    let mut agent = Agent::new(id, name);
    agent.tmux_session = tmux_session;
    let clone = agent.clone();
    state.register_agent(agent);
    tracing::info!(agent_id = %clone.id, name = %clone.name, "agent registered via IPC");
    clone
}

// ---------------------------------------------------------------------------
// Session info
// ---------------------------------------------------------------------------

/// Return lightweight session metadata (started_at, pid, version).
#[tauri::command]
pub fn get_session_info() -> Result<SessionInfo, String> {
    let info = SessionInfo::current();
    // Write to disk as a side effect so the file stays fresh.
    if let Err(e) = info.write() {
        tracing::warn!("failed to write session file: {e}");
    }
    Ok(info)
}

// ---------------------------------------------------------------------------
// Terminal commands
// ---------------------------------------------------------------------------

/// Start streaming terminal output for an agent.
/// The frontend should listen for "terminal-output" events.
#[tauri::command]
pub async fn terminal_attach(
    bridge: State<'_, TerminalBridge>,
    agent_id: String,
) -> Result<String, String> {
    if !bridge.session_exists(&agent_id) {
        return Err(format!("no tmux session for agent {agent_id}"));
    }

    if bridge.start_streaming(&agent_id).await {
        // Return initial content
        bridge.get_full_content(&agent_id).await.map_err(err)
    } else {
        // Already streaming, just return current content
        bridge.get_full_content(&agent_id).await.map_err(err)
    }
}

/// Stop streaming terminal output for an agent.
#[tauri::command]
pub async fn terminal_detach(
    bridge: State<'_, TerminalBridge>,
    agent_id: String,
) -> Result<(), String> {
    bridge.stop_streaming(&agent_id).await;
    Ok(())
}

/// Send keystrokes to an agent's terminal.
#[tauri::command]
pub fn terminal_send_keys(
    bridge: State<'_, TerminalBridge>,
    agent_id: String,
    keys: String,
) -> Result<(), String> {
    bridge.send_keys(&agent_id, &keys).map_err(err)
}

/// Get current terminal content for an agent (one-shot, no streaming).
#[tauri::command]
pub async fn terminal_capture(
    bridge: State<'_, TerminalBridge>,
    agent_id: String,
) -> Result<String, String> {
    bridge.capture_pane(&agent_id).map_err(err)
}
