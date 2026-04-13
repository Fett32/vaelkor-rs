/// Tauri IPC command handlers.
///
/// These are the functions the frontend calls via `invoke(...)`.
/// All commands receive the shared AppState via Tauri's managed state.

use crate::daemon::session::SessionInfo;
use crate::daemon::state::{Agent, AppState, Task, TaskState};
use crate::terminal::bridge::TerminalBridge;
use crate::terminal::pane_manager::PaneManager;
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
/// The SessionInfo is created once at startup and stored in Tauri managed state.
#[tauri::command]
pub fn get_session_info(info: State<'_, SessionInfo>) -> Result<SessionInfo, String> {
    Ok(info.inner().clone())
}

// ---------------------------------------------------------------------------
// Terminal commands (PTY relay to vaelkor-main)
// ---------------------------------------------------------------------------

/// Check if the PTY relay is running.
#[tauri::command]
pub async fn terminal_attach(
    bridge: State<'_, TerminalBridge>,
) -> Result<bool, String> {
    Ok(bridge.is_running().await)
}

/// Send keystrokes to the PTY (tmux routes to active pane).
#[tauri::command]
pub async fn terminal_send_keys(
    bridge: State<'_, TerminalBridge>,
    keys: String,
) -> Result<(), String> {
    bridge.send_keys(&keys).await.map_err(err)
}

/// Resize the PTY to match xterm.js dimensions.
#[tauri::command]
pub async fn terminal_resize(
    bridge: State<'_, TerminalBridge>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    bridge.resize(cols, rows).await.map_err(err)
}

// ---------------------------------------------------------------------------
// Pane management commands
// ---------------------------------------------------------------------------

/// Show an agent's pane in vaelkor-main.
#[tauri::command]
pub async fn pane_show(
    pm: State<'_, PaneManager>,
    agent_id: String,
) -> Result<(), String> {
    pm.add_agent_pane(&agent_id).await.map_err(err)
}

/// Hide an agent's pane from vaelkor-main.
#[tauri::command]
pub async fn pane_hide(
    pm: State<'_, PaneManager>,
    agent_id: String,
) -> Result<(), String> {
    pm.remove_agent_pane(&agent_id).await.map_err(err)
}

/// Get list of agents with visible panes.
#[tauri::command]
pub async fn pane_list(
    pm: State<'_, PaneManager>,
) -> Result<Vec<String>, String> {
    Ok(pm.visible_agents().await)
}
