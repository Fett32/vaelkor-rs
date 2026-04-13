use anyhow::{bail, Context, Result};
use std::process::Command;

/// Returns true if a tmux session with this name currently exists.
pub fn session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Create a new detached tmux session running `command`.
/// `command` is split on whitespace; use shell quoting yourself if needed.
pub fn create_session(name: &str, command: &str) -> Result<()> {
    let args: Vec<&str> = command.split_whitespace().collect();
    let mut cmd = Command::new("tmux");
    cmd.args(["new-session", "-d", "-s", name]);
    if !args.is_empty() {
        cmd.arg("--");
        cmd.args(&args);
    }
    let out = cmd.output().context("failed to spawn tmux")?;
    if !out.status.success() {
        bail!(
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Attach to an existing session (blocks until detached).
/// In the wrapper main loop we never actually block on this; it's here
/// for completeness and manual use.
pub fn attach_session(name: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["attach-session", "-t", name])
        .status()
        .context("failed to spawn tmux")?;
    if !status.success() {
        bail!("tmux attach-session failed");
    }
    Ok(())
}

/// Inject `text` into the tmux session as if the user typed it, followed by Enter.
pub fn send_keys(name: &str, text: &str) -> Result<()> {
    // Use -l (literal) so tmux doesn't interpret special sequences in the text.
    // Send text first, then Enter separately (Enter is a key name, not literal).
    let out = Command::new("tmux")
        .args(["send-keys", "-t", name, "-l", text])
        .output()
        .context("failed to spawn tmux send-keys (text)")?;
    if !out.status.success() {
        bail!(
            "tmux send-keys (text) failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Now send Enter as a key name (not literal).
    let out = Command::new("tmux")
        .args(["send-keys", "-t", name, "Enter"])
        .output()
        .context("failed to spawn tmux send-keys (Enter)")?;
    if !out.status.success() {
        bail!(
            "tmux send-keys (Enter) failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Capture the last `lines` lines of visible pane output.
/// Returns each line as a separate String.
pub fn capture_pane(name: &str, lines: usize) -> Result<Vec<String>> {
    let start = format!("-{}", lines);
    let out = Command::new("tmux")
        .args([
            "capture-pane",
            "-p",          // print to stdout
            "-t", name,
            "-S", &start,  // start N lines back
        ])
        .output()
        .context("failed to spawn tmux")?;
    if !out.status.success() {
        bail!(
            "tmux capture-pane failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().map(|l| l.to_owned()).collect())
}
