//! Codex exposes a first-class session API. Prefer it over terminal automation.
//!
//! `codex queue` inserts into Codex's own queue database and the running session
//! drains it, so delivery needs no terminal, no keystrokes, and no idle composer.
use anyhow::{Result, bail, ensure};
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

/// A failed delivery attempt, split by whether anything could have been delivered.
///
/// Only `NotAttempted` is safe to retry under the same request ID.
#[derive(Debug)]
pub enum Failure {
    NotAttempted(anyhow::Error),
    Uncertain(anyhow::Error),
}

impl Failure {
    pub fn into_error(self) -> anyhow::Error {
        match self {
            Self::NotAttempted(error) | Self::Uncertain(error) => error,
        }
    }
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
/// A running Codex holds its thread write lock and that thread's rollout open.
/// Reading them works for sessions that were not started with `codex resume`,
/// whose thread id never appears in the command line.
///
/// Both files are matched by exact name and an ambiguous process is refused, so a
/// message is never addressed to one thread using another thread's state.
pub fn session(proc: &Path, pid: u32) -> Result<Option<Session>> {
    let Ok(entries) = std::fs::read_dir(proc.join(pid.to_string()).join("fd")) else {
        return Ok(None);
    };
    let mut threads: Vec<String> = vec![];
    let mut rollouts: Vec<PathBuf> = vec![];
    for entry in entries.flatten() {
        let Ok(path) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        let in_locks = path
            .parent()
            .is_some_and(|parent| parent.ends_with("thread-writer-locks"));
        let lock = name.strip_suffix(".lock").filter(|id| is_thread_id(id));
        match lock {
            Some(id) if in_locks => {
                if !threads.iter().any(|held| held == id) {
                    threads.push(id.to_string());
                }
            }
            _ if name.starts_with("rollout-")
                && name.ends_with(".jsonl")
                && !rollouts.contains(&path) =>
            {
                rollouts.push(path);
            }
            _ => {}
        }
    }
    ensure!(
        threads.len() <= 1,
        "Process {pid} holds {} Codex thread locks; refusing to guess which session to address",
        threads.len()
    );
    let Some(thread) = threads.pop() else {
        return Ok(None);
    };
    // The thread id is the rollout's final name component, never a parent directory.
    let suffix = format!("-{thread}.jsonl");
    rollouts.retain(|path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(&suffix))
    });
    match rollouts.len() {
        0 => Ok(None),
        1 => Ok(Some(Session {
            thread,
            rollout: rollouts.remove(0),
        })),
        count => bail!("Process {pid} holds {count} rollouts for thread {thread}"),
    }
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
/// A turn whose records outgrow the window read here reports `unknown` rather than
/// inheriting a default, so a working session is never reported as finished.
///
/// A turn can end by being interrupted as well as by finishing. Reporting that as
/// `aborted` rather than `idle` matters, because Codex does not drain its queue
/// after an interruption: a message delivered to that thread waits for the person
/// at the keyboard.
pub fn state(rollout: &Path) -> Result<Value> {
    let mut status = "unknown";
    let mut last_agent_message = None;
    for line in tail(rollout, 262144)?.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["type"] != "event_msg" {
            continue;
        }
        match record["payload"]["type"].as_str() {
            Some("task_started") => status = "running",
            Some("turn_aborted") => status = "aborted",
            Some("task_complete") => {
                status = "idle";
                if let Some(text) = record["payload"]["last_agent_message"].as_str() {
                    last_agent_message = Some(text.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(json!({
        "status": status,
        "last_agent_message": last_agent_message,
    }))
}

/// Hand a message to Codex for an existing thread. A busy session queues it.
pub async fn queue(thread: &str, text: &str) -> std::result::Result<(), Failure> {
    let started = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("codex")
            .args(["queue", "--thread", thread, "--message", text])
            .kill_on_drop(true)
            .output(),
    );
    // A command that never started cannot have delivered anything.
    let output = match started.await {
        Err(elapsed) => {
            return Err(Failure::Uncertain(
                anyhow::Error::new(elapsed).context("codex queue timed out"),
            ));
        }
        Ok(Err(error)) => {
            return Err(Failure::NotAttempted(anyhow::Error::new(error).context(
                "Cannot run codex; it must be on PATH to reach an open session",
            )));
        }
        Ok(Ok(output)) => output,
    };
    // An exit code does not say how far Codex got, so this stays uncertain.
    if !output.status.success() {
        return Err(Failure::Uncertain(anyhow::anyhow!(
            "codex queue: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}
