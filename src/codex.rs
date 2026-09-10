//! Codex exposes a first-class session API. Prefer it over terminal automation.
//!
//! `codex queue` inserts into Codex's own queue database and the running session
//! drains it, so delivery needs no terminal, no keystrokes, and no idle composer.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// An interactive Codex session, identified by the files it holds open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub thread: String,
    pub rollout: PathBuf,
}

fn is_thread_id(text: &str) -> bool {
    text.len() == 36
        && text.chars().enumerate().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Recover a session's thread identity from its open files.
///
/// A running Codex holds its thread write lock and its rollout open. Reading them
/// works for sessions that were not started with `codex resume`, whose thread id
/// never appears in the command line.
pub fn session(proc: &Path, pid: u32) -> Result<Option<Session>> {
    let Ok(entries) = std::fs::read_dir(proc.join(pid.to_string()).join("fd")) else {
        return Ok(None);
    };
    let mut thread = None;
    let mut rollouts = vec![];
    for entry in entries.flatten() {
        let Ok(path) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let text = path.to_string_lossy().into_owned();
        let lock = text
            .split("/thread-writer-locks/")
            .nth(1)
            .and_then(|name| name.strip_suffix(".lock"))
            .filter(|id| is_thread_id(id));
        if let Some(id) = lock {
            thread = Some(id.to_string());
        } else if text.contains("/sessions/") && text.ends_with(".jsonl") {
            rollouts.push(path);
        }
    }
    let Some(thread) = thread else {
        return Ok(None);
    };
    let rollout = rollouts
        .into_iter()
        .find(|path| path.to_string_lossy().contains(&thread));
    Ok(rollout.map(|rollout| Session { thread, rollout }))
}

/// Read the end of a rollout. Transcripts grow without bound; only the tail is needed.
fn tail(path: &Path, limit: u64) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(limit);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![];
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // A truncated first line is not a record.
    match text.find('\n') {
        Some(end) if start > 0 => Ok(text[end + 1..].to_string()),
        _ => Ok(text),
    }
}

/// Report whether a turn is in flight and what the agent last said.
///
/// This is a structured answer from Codex's own transcript, not a screen scrape.
pub fn state(rollout: &Path) -> Result<Value> {
    let mut running = false;
    let mut last_agent_message = None;
    for line in tail(rollout, 262144)?.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["type"] != "event_msg" {
            continue;
        }
        match record["payload"]["type"].as_str() {
            Some("task_started") => running = true,
            Some("task_complete") => {
                running = false;
                if let Some(text) = record["payload"]["last_agent_message"].as_str() {
                    last_agent_message = Some(text.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(json!({
        "status": if running { "running" } else { "idle" },
        "last_agent_message": last_agent_message,
    }))
}

/// Hand a message to Codex for an existing thread. A busy session queues it.
pub async fn queue(thread: &str, text: &str) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("codex")
            .args(["queue", "--thread", thread, "--message", text])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("codex queue timed out")?
    .context("Cannot run codex; it must be on PATH to reach an open session")?;
    ensure!(
        output.status.success(),
        "codex queue: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}
