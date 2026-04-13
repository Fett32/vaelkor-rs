mod client;
mod detector;
mod protocol;
mod tmux;

use anyhow::{bail, Context, Result};
use detector::{AgentKind, IdleDetector};
use protocol::{
    AgentState, DaemonShutdown, Envelope, StatusRequest, StatusResponse, TaskAccept, TaskAssign,
    TaskComplete, WrapperError, WrapperRegister, MSG_REGISTER, MSG_SHUTDOWN, MSG_STATUS_REQUEST,
    MSG_STATUS_RESPONSE, MSG_TASK_ACCEPT, MSG_TASK_ASSIGN, MSG_TASK_COMPLETE, MSG_ERROR,
};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

// ---- configuration constants ------------------------------------------------

const DAEMON_SOCK: &str = "/tmp/vaelkor/daemon.sock";
const CAPTURE_LINES: usize = 50;
const POLL_INTERVAL_MS: u64 = 500;
/// Number of trailing lines to check for the idle pattern.
const IDLE_TAIL: usize = 5;
/// Idle pattern must be stable for this many seconds before marking complete.
const IDLE_STABLE_SECS: f64 = 3.0;

// ---- CLI args ---------------------------------------------------------------

fn parse_args() -> Result<String> {
    let mut args = std::env::args().skip(1);
    match args.next() {
        Some(name) if !name.is_empty() => Ok(name),
        _ => bail!("Usage: vaelkor-wrapper <agent-name>  e.g. vaelkor-wrapper claude"),
    }
}

// ---- helpers ----------------------------------------------------------------

fn session_name(agent: &str) -> String {
    format!("vaelkor-{}", agent)
}

/// Start the tmux session if it doesn't already exist, running the agent CLI.
fn ensure_session(session: &str, kind: &AgentKind) -> Result<()> {
    if tmux::session_exists(session) {
        info!(session, "reusing existing tmux session");
        return Ok(());
    }
    let command = match kind {
        AgentKind::ClaudeCode => "claude",
        AgentKind::Codex => "codex",
        _ => "bash",
    };
    info!(session, command, "creating new tmux session");
    tmux::create_session(session, command)
        .with_context(|| format!("could not create tmux session {session}"))?;
    Ok(())
}

// ---- main loop --------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("vaelkor_wrapper=info".parse().unwrap()),
        )
        .init();

    let agent = parse_args()?;
    let session = session_name(&agent);
    let kind = AgentKind::from_name(&agent);
    let detector = IdleDetector::new(&kind);

    info!(agent, "vaelkor-wrapper starting");

    // Connect to the daemon socket.
    let mut client = client::DaemonClient::connect(DAEMON_SOCK)
        .await
        .context("connecting to daemon")?;

    // Register with the daemon.
    let register_env = Envelope::new(MSG_REGISTER, WrapperRegister { agent_id: agent.clone() })?;
    client.send(&register_env).await?;

    // Ensure the tmux session exists.
    if let Err(e) = ensure_session(&session, &kind) {
        let msg = format!("{e:#}");
        error!(%msg, "failed to create tmux session");
        let err_env = Envelope::new(
            MSG_ERROR,
            WrapperError { agent_id: agent.clone(), message: msg },
        )?;
        client.send(&err_env).await?;
        return Err(e);
    }

    // Runtime state
    let mut state = AgentState::Idle;
    /// Tracks when we first saw the idle pattern (for stability window).
    let mut idle_since: Option<std::time::Instant> = None;

    // Give the agent a moment to render its first prompt before polling.
    sleep(Duration::from_millis(1500)).await;

    loop {
        // ---- race: daemon message OR poll interval ----
        tokio::select! {
            result = client.recv() => {
                match result {
                    Ok(Some(envelope)) => {
                        let keep_running = handle_envelope(
                            envelope,
                            &agent,
                            &session,
                            &mut state,
                            &mut client,
                        )
                        .await?;
                        // Reset idle timer on any daemon message (new task, etc).
                        idle_since = None;
                        if !keep_running {
                            break;
                        }
                    }
                    Ok(None) => {
                        warn!("daemon closed connection, exiting");
                        break;
                    }
                    Err(e) => {
                        error!("error reading from daemon: {e:#}");
                        break;
                    }
                }
            }
            _ = sleep(Duration::from_millis(POLL_INTERVAL_MS)) => {
                // Timer fired — fall through to tmux poll below.
            }
        }

        // ---- poll tmux for idle pattern when a task is running ----
        if let AgentState::Running { task_id } = &state.clone() {
            let task_id = *task_id;
            match tmux::capture_pane(&session, CAPTURE_LINES) {
                Ok(lines) => {
                    if detector.is_idle_tail(&lines, IDLE_TAIL) {
                        let now = std::time::Instant::now();
                        let first_seen = *idle_since.get_or_insert(now);
                        let elapsed = now.duration_since(first_seen).as_secs_f64();

                        if elapsed >= IDLE_STABLE_SECS {
                            info!(
                                stable_for = format!("{elapsed:.1}s"),
                                "idle pattern stable, task complete"
                            );
                            state = AgentState::Idle;
                            idle_since = None;
                            let complete_env = Envelope::new(
                                MSG_TASK_COMPLETE,
                                TaskComplete {
                                    task_id,
                                    summary: None,
                                    output: None,
                                },
                            )?;
                            client.send(&complete_env).await?;
                        }
                    } else {
                        // Output changed — reset the stability timer.
                        idle_since = None;
                    }
                }
                Err(e) => {
                    warn!("capture_pane error: {e:#}");
                }
            }
        }
    }

    info!("vaelkor-wrapper exiting");
    Ok(())
}

/// Handle one incoming envelope from the daemon.
/// Returns `false` to signal the main loop should exit.
async fn handle_envelope(
    env: Envelope,
    agent: &str,
    session: &str,
    state: &mut AgentState,
    client: &mut client::DaemonClient,
) -> Result<bool> {
    match env.kind.as_str() {
        MSG_TASK_ASSIGN => {
            let task: TaskAssign = env
                .decode_payload()
                .context("decode task.assign payload")?;
            info!(task_id = %task.task_id, title = %task.title, "received task");

            // Inject the prompt into the tmux session FIRST.
            let prompt = format!("{}\n{}", task.title, task.description);
            match tmux::send_keys(session, &prompt) {
                Ok(()) => {
                    // Only send accept AFTER successful injection.
                    let accept_env = Envelope::new(
                        MSG_TASK_ACCEPT,
                        TaskAccept { task_id: task.task_id },
                    )?;
                    client.send(&accept_env).await?;
                    *state = AgentState::Running { task_id: task.task_id };
                    info!(task_id = %task.task_id, "task accepted and injected");
                }
                Err(e) => {
                    // Injection failed - send error, don't send accept.
                    let message = format!("send_keys failed: {e:#}");
                    error!(%message);
                    let err_env = Envelope::new(
                        MSG_ERROR,
                        WrapperError { agent_id: agent.to_owned(), message },
                    )?;
                    client.send(&err_env).await?;
                }
            }
        }

        MSG_STATUS_REQUEST => {
            let req: StatusRequest = env
                .decode_payload()
                .context("decode status.request payload")?;
            let (alive, task_id, details) = match state {
                AgentState::Idle => (true, req.task_id, Some("idle".to_owned())),
                AgentState::Running { task_id } => {
                    (true, Some(*task_id), Some("running".to_owned()))
                }
                AgentState::Uninitialized => {
                    (false, None, Some("uninitialized".to_owned()))
                }
            };
            let resp_env = Envelope::new(
                MSG_STATUS_RESPONSE,
                StatusResponse {
                    agent_id: agent.to_owned(),
                    task_id,
                    alive,
                    details,
                },
            )?;
            // Preserve correlation_id so daemon can match request to response.
            let mut resp_env = resp_env;
            resp_env.correlation_id = env.correlation_id;
            client.send(&resp_env).await?;
        }

        MSG_SHUTDOWN => {
            let _: DaemonShutdown = env
                .decode_payload()
                .unwrap_or(DaemonShutdown {});
            info!("received shutdown from daemon");
            return Ok(false);
        }

        other => {
            warn!(kind = other, "unrecognised message type from daemon, ignoring");
        }
    }
    Ok(true)
}
