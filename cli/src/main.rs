use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use uuid::Uuid;

const DAEMON_SOCKET: &str = "/tmp/vaelkor/daemon.sock";

// ---------------------------------------------------------------------------
// Wire protocol (subset — mirrors daemon's protocol.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    correlation_id: Uuid,
    payload: serde_json::Value,
}

impl Envelope {
    fn new(kind: &str, payload: impl Serialize) -> Result<Self> {
        Ok(Self {
            kind: kind.to_string(),
            correlation_id: Uuid::new_v4(),
            payload: serde_json::to_value(payload)?,
        })
    }
}

// ---------------------------------------------------------------------------
// CLI message payloads
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CliStatusRequest {}

#[derive(Serialize)]
struct CliTaskList {}

#[derive(Serialize)]
struct CliTaskCreate {
    title: String,
    description: String,
}

#[derive(Serialize)]
struct CliTaskCancel {
    task_id: Uuid,
}

#[derive(Serialize)]
struct CliSpawn {
    agent: String,
    role: String,
}

#[derive(Serialize)]
struct CliKill {
    instance: String,
}

#[derive(Serialize)]
struct CliProjectGet {
    name: String,
}

#[derive(Serialize)]
struct CliProjectSave {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stack: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    doc_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_index: Option<String>,
}

#[derive(Serialize)]
struct CliAssign {
    task_id: Uuid,
    agent_id: String,
}

#[derive(Serialize)]
struct CliTaskGet {
    task_id: Uuid,
}

// ---------------------------------------------------------------------------
// Response types (for display)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AgentInfo {
    id: String,
    name: String,
    connected: bool,
    tmux_session: Option<String>,
}

#[derive(Deserialize)]
struct TaskInfo {
    id: Uuid,
    title: String,
    state: String,
    assigned_to: Option<String>,
    #[allow(dead_code)]
    description: String,
}

#[derive(Deserialize)]
struct StatusResponsePayload {
    agents: Vec<AgentInfo>,
    tasks: Vec<TaskInfo>,
}

#[derive(Deserialize)]
struct TaskResponsePayload {
    task: TaskInfo,
}

#[derive(Deserialize)]
struct TaskListResponsePayload {
    tasks: Vec<TaskInfo>,
}

#[derive(Deserialize)]
struct SpawnResponsePayload {
    instance: String,
    pid: u32,
}

#[derive(Deserialize)]
struct ErrorResponsePayload {
    message: String,
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "vaelkor", about = "Vaelkor multi-agent orchestrator CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show all agents and active tasks
    Status,
    /// Task management
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Project profile management
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Assign a task to an agent
    Assign {
        /// Task ID (UUID)
        task_id: Uuid,
        /// Agent ID to assign to
        agent_id: String,
    },
    /// Spawn a new agent instance
    Spawn {
        /// Agent kind (e.g. "claude", "codex")
        agent: String,
        /// Role for this instance (e.g. "impl", "review")
        #[arg(long, default_value = "impl")]
        role: String,
    },
    /// Stream events from the daemon (long-lived)
    Event {
        #[command(subcommand)]
        action: EventAction,
    },
    /// Kill a running agent instance
    Kill {
        /// Instance name (e.g. "claude-impl-1")
        instance: String,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// List all project profiles
    List,
    /// Show a project profile
    Get {
        /// Project name
        name: String,
    },
    /// Create or update a project profile
    Save {
        /// Project name
        name: String,
        /// Project description
        #[arg(long)]
        description: Option<String>,
        /// Root directory
        #[arg(long)]
        root_dir: Option<String>,
        /// Tech stack (comma-separated)
        #[arg(long, value_delimiter = ',')]
        stack: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
enum EventAction {
    /// Stream all daemon events to stdout (JSON, one per line)
    Stream,
}

#[derive(Subcommand)]
enum TaskAction {
    /// List all tasks
    List,
    /// Show a specific task
    Get {
        /// Task ID (UUID)
        task_id: Uuid,
    },
    /// Create a new task
    Create {
        /// Task title
        title: String,
        /// Task description
        description: String,
    },
    /// Cancel a task
    Cancel {
        /// Task ID (UUID)
        task_id: Uuid,
    },
}

// ---------------------------------------------------------------------------
// Daemon communication
// ---------------------------------------------------------------------------

async fn send_request(envelope: &Envelope) -> Result<Envelope> {
    let stream = UnixStream::connect(DAEMON_SOCKET)
        .await
        .context("failed to connect to daemon — is vaelkor running?")?;

    let (read_half, mut write_half) = stream.into_split();

    let mut line = serde_json::to_string(envelope)?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    let mut reader = BufReader::new(read_half);
    let mut response_line = String::new();
    let n = reader.read_line(&mut response_line).await?;
    if n == 0 {
        anyhow::bail!("daemon closed connection without response");
    }

    let response: Envelope = serde_json::from_str(response_line.trim())
        .context("failed to parse daemon response")?;

    Ok(response)
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

async fn cmd_status() -> Result<()> {
    let req = Envelope::new("cli.status", CliStatusRequest {})?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let status: StatusResponsePayload = serde_json::from_value(resp.payload)?;

    println!("=== Agents ===");
    if status.agents.is_empty() {
        println!("  (none registered)");
    }
    for agent in &status.agents {
        let conn = if agent.connected { "connected" } else { "disconnected" };
        let tmux = agent.tmux_session.as_deref().unwrap_or("-");
        println!("  {} ({}) [{}] tmux:{}", agent.id, agent.name, conn, tmux);
    }

    println!("\n=== Tasks ===");
    if status.tasks.is_empty() {
        println!("  (none)");
    }
    for task in &status.tasks {
        let assignee = task.assigned_to.as_deref().unwrap_or("unassigned");
        println!("  {} [{}] {} → {}", task.id, task.state, task.title, assignee);
    }

    Ok(())
}

async fn cmd_task_list() -> Result<()> {
    let req = Envelope::new("cli.task.list", CliTaskList {})?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let list: TaskListResponsePayload = serde_json::from_value(resp.payload)?;

    if list.tasks.is_empty() {
        println!("No tasks.");
        return Ok(());
    }

    for task in &list.tasks {
        let assignee = task.assigned_to.as_deref().unwrap_or("unassigned");
        println!("{} [{}] {} → {}", task.id, task.state, task.title, assignee);
    }

    Ok(())
}

async fn cmd_task_get(task_id: Uuid) -> Result<()> {
    let req = Envelope::new("cli.task.get", CliTaskGet { task_id })?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let info: TaskResponsePayload = serde_json::from_value(resp.payload)?;
    let t = &info.task;
    let assignee = t.assigned_to.as_deref().unwrap_or("unassigned");
    println!("ID:          {}", t.id);
    println!("Title:       {}", t.title);
    println!("State:       {}", t.state);
    println!("Assigned to: {}", assignee);
    println!("Description: {}", t.description);

    Ok(())
}

async fn cmd_task_create(title: String, description: String) -> Result<()> {
    let req = Envelope::new("cli.task.create", CliTaskCreate { title, description })?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let info: TaskResponsePayload = serde_json::from_value(resp.payload)?;
    println!("Created task: {}", info.task.id);

    Ok(())
}

async fn cmd_task_cancel(task_id: Uuid) -> Result<()> {
    let req = Envelope::new("cli.task.cancel", CliTaskCancel { task_id })?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    println!("Task {} cancelled.", &task_id.to_string()[..8]);

    Ok(())
}

async fn cmd_assign(task_id: Uuid, agent_id: String) -> Result<()> {
    let req = Envelope::new("cli.assign", CliAssign { task_id, agent_id: agent_id.clone() })?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    println!("Task {} assigned to {}.", &task_id.to_string()[..8], agent_id);

    Ok(())
}

async fn cmd_project_list() -> Result<()> {
    let req = Envelope::new("cli.project.list", serde_json::json!({}))?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let projects = resp.payload.get("projects")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if projects.is_empty() {
        println!("No project profiles.");
        return Ok(());
    }

    for p in &projects {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let desc = p.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let root = p.get("root_dir").and_then(|v| v.as_str()).unwrap_or("-");
        println!("  {} — {} ({})", name, desc, root);
    }

    Ok(())
}

async fn cmd_project_get(name: String) -> Result<()> {
    let req = Envelope::new("cli.project.get", CliProjectGet { name })?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let pretty = serde_json::to_string_pretty(&resp.payload)?;
    println!("{pretty}");

    Ok(())
}

async fn cmd_project_save(
    name: String,
    description: Option<String>,
    root_dir: Option<String>,
    stack: Option<Vec<String>>,
) -> Result<()> {
    let req = Envelope::new("cli.project.save", CliProjectSave {
        name: name.clone(),
        description,
        stack,
        root_dir,
        key_files: None,
        doc_paths: None,
        memory_index: None,
    })?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let path = resp.payload.get("path").and_then(|v| v.as_str()).unwrap_or("?");
    println!("Project '{}' saved to {}", name, path);

    Ok(())
}

async fn cmd_event_stream() -> Result<()> {
    let stream = UnixStream::connect(DAEMON_SOCKET)
        .await
        .context("failed to connect to daemon — is vaelkor running?")?;

    let (read_half, mut write_half) = stream.into_split();

    // Send the event stream registration.
    let req = Envelope::new("cli.event.stream", serde_json::json!({}))?;
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    // Read events as they come.
    let mut reader = BufReader::new(read_half);
    let mut event_line = String::new();
    loop {
        event_line.clear();
        let n = reader.read_line(&mut event_line).await?;
        if n == 0 {
            break; // Daemon disconnected.
        }
        // Print each event as-is (JSON).
        print!("{}", event_line);
    }

    Ok(())
}

async fn cmd_spawn(agent: String, role: String) -> Result<()> {
    let req = Envelope::new("cli.spawn", CliSpawn { agent, role })?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let info: SpawnResponsePayload = serde_json::from_value(resp.payload)?;
    println!("Spawned instance: {} (pid {})", info.instance, info.pid);

    Ok(())
}

async fn cmd_kill(instance: String) -> Result<()> {
    let req = Envelope::new("cli.kill", CliKill { instance: instance.clone() })?;
    let resp = send_request(&req).await?;

    if resp.kind == "cli.error" {
        let err: ErrorResponsePayload = serde_json::from_value(resp.payload)?;
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    println!("Killed instance: {}", instance);

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Status => cmd_status().await,
        Commands::Task { action } => match action {
            TaskAction::List => cmd_task_list().await,
            TaskAction::Get { task_id } => cmd_task_get(task_id).await,
            TaskAction::Create { title, description } => cmd_task_create(title, description).await,
            TaskAction::Cancel { task_id } => cmd_task_cancel(task_id).await,
        },
        Commands::Project { action } => match action {
            ProjectAction::List => cmd_project_list().await,
            ProjectAction::Get { name } => cmd_project_get(name).await,
            ProjectAction::Save { name, description, root_dir, stack } => {
                cmd_project_save(name, description, root_dir, stack).await
            }
        },
        Commands::Assign { task_id, agent_id } => cmd_assign(task_id, agent_id).await,
        Commands::Event { action } => match action {
            EventAction::Stream => cmd_event_stream().await,
        },
        Commands::Spawn { agent, role } => cmd_spawn(agent, role).await,
        Commands::Kill { instance } => cmd_kill(instance).await,
    };

    if let Err(e) = result {
        eprintln!("vaelkor: {e:#}");
        std::process::exit(1);
    }
}
