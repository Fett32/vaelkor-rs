/// Wire protocol between the Vaelkor orchestrator and agent wrappers.
///
/// All messages are newline-delimited JSON on a Unix socket at
/// /tmp/vaelkor/<agent_id>.sock
///
/// Direction conventions:
///   O→W  orchestrator sends to wrapper
///   W→O  wrapper sends back to orchestrator

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Message type constants (used as the `type` discriminant in JSON)
// ---------------------------------------------------------------------------

pub const MSG_TASK_ASSIGN: &str = "task.assign";
pub const MSG_TASK_ACCEPT: &str = "task.accept";
pub const MSG_TASK_REJECT: &str = "task.reject";
pub const MSG_TASK_COMPLETE: &str = "task.complete";
pub const MSG_TASK_BLOCKED: &str = "task.blocked";
pub const MSG_STATUS_REQUEST: &str = "status.request";
pub const MSG_STATUS_RESPONSE: &str = "status.response";
pub const MSG_REGISTER: &str = "wrapper.register";
pub const MSG_ERROR: &str = "wrapper.error";
pub const MSG_SHUTDOWN: &str = "daemon.shutdown";

// ---------------------------------------------------------------------------
// Envelope — every message on the wire is wrapped in this
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// One of the MSG_* constants above.
    #[serde(rename = "type")]
    pub kind: String,
    /// Correlation ID so responses can be matched to requests.
    pub correlation_id: Uuid,
    /// The actual payload, type-erased as raw JSON.
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new(kind: &str, payload: impl Serialize) -> anyhow::Result<Self> {
        Ok(Self {
            kind: kind.to_string(),
            correlation_id: Uuid::new_v4(),
            payload: serde_json::to_value(payload)?,
        })
    }

    pub fn decode_payload<T: for<'de> serde::Deserialize<'de>>(&self) -> anyhow::Result<T> {
        Ok(serde_json::from_value(self.payload.clone())?)
    }
}

// ---------------------------------------------------------------------------
// O→W  task.assign
// ---------------------------------------------------------------------------

/// Orchestrator assigns a task to an agent wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssign {
    pub task_id: Uuid,
    pub title: String,
    pub description: String,
    /// Optional timeout in seconds; wrapper should report TimedOut if exceeded.
    pub timeout_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// W→O  task.accept
// ---------------------------------------------------------------------------

/// Wrapper acknowledges it has received and will begin the task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAccept {
    pub task_id: Uuid,
}

// ---------------------------------------------------------------------------
// W→O  task.reject
// ---------------------------------------------------------------------------

/// Wrapper refuses to handle the task (e.g. precondition not met).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReject {
    pub task_id: Uuid,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// W→O  task.complete
// ---------------------------------------------------------------------------

/// Wrapper reports successful completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskComplete {
    pub task_id: Uuid,
    /// Short human-readable summary of what was done.
    pub summary: Option<String>,
    /// Machine-readable output data (free-form JSON).
    pub output: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// W→O  task.blocked
// ---------------------------------------------------------------------------

/// Wrapper cannot proceed and is waiting for something external.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBlocked {
    pub task_id: Uuid,
    pub reason: String,
    /// If the wrapper knows what it needs, it can suggest it here.
    pub waiting_for: Option<String>,
}

// ---------------------------------------------------------------------------
// O→W  status.request  /  W→O  status.response
// ---------------------------------------------------------------------------

/// Orchestrator pings the wrapper for a heartbeat / current status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRequest {
    /// If present, ask about a specific task; otherwise ask for general status.
    pub task_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub agent_id: String,
    pub task_id: Option<Uuid>,
    pub alive: bool,
    pub details: Option<String>,
}

// ---------------------------------------------------------------------------
// W→O  wrapper.register — first message after connecting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrapperRegister {
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// W→O  wrapper.error — something went wrong
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrapperError {
    pub agent_id: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// O→W  daemon.shutdown — daemon is going away
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonShutdown {}
