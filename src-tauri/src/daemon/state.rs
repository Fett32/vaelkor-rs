use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Task state machine
// ---------------------------------------------------------------------------

/// All valid states a task can be in.
/// Transitions are enforced by `TaskEntry::transition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
    /// Orchestrator has created the task and nominated an agent.
    Assigned,
    /// Agent has acknowledged and begun work.
    Accepted,
    /// Agent finished successfully.
    Completed,
    /// Agent is waiting on a dependency or external event.
    Blocked,
    /// Orchestrator or user explicitly cancelled the task.
    Cancelled,
    /// Agent refused to take the task.
    Rejected,
    /// Watchdog deadline passed without a status update.
    TimedOut,
    /// Agent was interrupted mid-run (e.g. tmux pane died).
    Interrupted,
    /// Agent is attempting to resume after an interruption.
    Recovering,
    /// Task was assigned but no acknowledgement received within the grace window.
    Stale,
}

impl TaskState {
    /// Returns `true` if the state is terminal (no further transitions allowed).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::Completed
                | TaskState::Cancelled
                | TaskState::Rejected
                | TaskState::TimedOut
        )
    }

    /// Validate whether a transition from `self` to `next` is legal.
    pub fn can_transition_to(&self, next: &TaskState) -> bool {
        if self.is_terminal() {
            return false;
        }
        use TaskState::*;
        matches!(
            (self, next),
            (Assigned, Accepted)
                | (Assigned, Rejected)
                | (Assigned, Cancelled)
                | (Assigned, Stale)
                | (Accepted, Completed)
                | (Accepted, Blocked)
                | (Accepted, Cancelled)
                | (Accepted, Interrupted)
                | (Accepted, TimedOut)
                | (Blocked, Accepted)
                | (Blocked, Cancelled)
                | (Blocked, TimedOut)
                | (Interrupted, Recovering)
                | (Interrupted, Cancelled)
                | (Recovering, Accepted)
                | (Recovering, Cancelled)
                | (Recovering, Interrupted)
                | (Stale, Accepted)
                | (Stale, Cancelled)
        )
    }
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub state: TaskState,
    /// Agent ID this task is assigned to (may be empty if unassigned).
    pub assigned_to: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            description: description.into(),
            state: TaskState::Assigned,
            assigned_to: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Attempt a state transition.  Returns `Err` if the transition is illegal.
    pub fn transition(&mut self, next: TaskState) -> anyhow::Result<()> {
        if self.state.can_transition_to(&next) {
            tracing::info!(
                task_id = %self.id,
                from = ?self.state,
                to = ?next,
                "task state transition"
            );
            self.state = next;
            self.updated_at = Utc::now();
            Ok(())
        } else {
            anyhow::bail!(
                "illegal transition {:?} -> {:?} for task {}",
                self.state,
                next,
                self.id
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// tmux session name this agent lives in.
    pub tmux_session: Option<String>,
    /// Path to the Unix socket the wrapper exposes (if connected).
    pub socket_path: Option<String>,
    pub connected: bool,
    pub registered_at: DateTime<Utc>,
}

impl Agent {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            tmux_session: None,
            socket_path: None,
            connected: false,
            registered_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// AppState — Tauri managed state
// ---------------------------------------------------------------------------

use std::sync::Arc;

#[derive(Default, Clone)]
pub struct AppState {
    inner: Arc<Mutex<StateInner>>,
}

#[derive(Default)]
struct StateInner {
    tasks: HashMap<Uuid, Task>,
    agents: HashMap<String, Agent>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StateInner::default())),
        }
    }

    // --- tasks ---------------------------------------------------------------

    pub fn add_task(&self, task: Task) {
        let mut s = self.inner.lock().unwrap();
        s.tasks.insert(task.id, task);
    }

    pub fn get_task(&self, id: Uuid) -> Option<Task> {
        self.inner.lock().unwrap().tasks.get(&id).cloned()
    }

    pub fn all_tasks(&self) -> Vec<Task> {
        self.inner.lock().unwrap().tasks.values().cloned().collect()
    }

    /// Transition a task to a new state.  Returns the updated task on success.
    pub fn transition_task(&self, id: Uuid, next: TaskState) -> anyhow::Result<Task> {
        let mut s = self.inner.lock().unwrap();
        let task = s
            .tasks
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("task {} not found", id))?;
        task.transition(next)?;
        Ok(task.clone())
    }

    /// Assign a task to an agent (sets `assigned_to` and transitions to Assigned).
    pub fn assign_task_to_agent(
        &self,
        task_id: Uuid,
        agent_id: &str,
    ) -> anyhow::Result<Task> {
        let mut s = self.inner.lock().unwrap();
        let task = s
            .tasks
            .get_mut(&task_id)
            .ok_or_else(|| anyhow::anyhow!("task {} not found", task_id))?;
        task.assigned_to = Some(agent_id.to_string());
        task.updated_at = Utc::now();
        Ok(task.clone())
    }

    // --- agents --------------------------------------------------------------

    pub fn register_agent(&self, agent: Agent) {
        let mut s = self.inner.lock().unwrap();
        s.agents.insert(agent.id.clone(), agent);
    }

    pub fn all_agents(&self) -> Vec<Agent> {
        self.inner
            .lock()
            .unwrap()
            .agents
            .values()
            .cloned()
            .collect()
    }

    pub fn get_agent(&self, id: &str) -> Option<Agent> {
        self.inner.lock().unwrap().agents.get(id).cloned()
    }

    /// Update the connected status of an agent.
    pub fn set_agent_connected(&self, id: &str, connected: bool) {
        let mut s = self.inner.lock().unwrap();
        if let Some(agent) = s.agents.get_mut(id) {
            agent.connected = connected;
            tracing::info!(agent_id = id, connected, "agent connection status updated");
        } else {
            // Auto-register agent on first connection
            let mut agent = Agent::new(id, id);
            agent.connected = connected;
            tracing::info!(agent_id = id, "agent auto-registered on connection");
            s.agents.insert(id.to_string(), agent);
        }
    }
}
