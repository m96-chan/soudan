//! Bounded, non-consuming waits. The supervisor isolates blocking filesystem reads.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};

pub const DEFAULT_TIMEOUT: u64 = 300;
pub const MAX_TIMEOUT: u64 = 3600;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Watch {
    Room { room: String, after: i64 },
    Delivery { request_id: String },
}
impl Watch {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Room { room, after } => {
                ensure!(
                    !room.trim().is_empty() && room.len() <= 128,
                    "Room must contain 1–128 bytes"
                );
                ensure!(*after >= 0, "after must be nonnegative");
            }
            Self::Delivery { request_id } => crate::live::validate_request_id(request_id)?,
        }
        Ok(())
    }
    fn snapshot(&self, workspace: &Path, proc: &Path) -> Result<Option<Value>> {
        match self {
            Self::Room { room, after } => {
                match std::fs::metadata(workspace.join(".soudan/state.db")) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e.into()),
                    Ok(_) => (),
                }
                let db = open_readonly(workspace)?;
                let exists: bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages')",[],|r|r.get(0))?;
                if !exists {
                    return Ok(None);
                }
                let mut q=db.prepare("SELECT id,room,sender,text FROM messages WHERE room=?1 AND id>?2 ORDER BY id LIMIT 100")?;
                let messages=q.query_map(params![room,after],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"room":r.get::<_,String>(1)?,"sender":r.get::<_,String>(2)?,"text":r.get::<_,String>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(messages.last().map(|last|json!({"outcome":"event","kind":"room","room":room,"after":after,"last_id":last["id"],"messages":messages})))
            }
            Self::Delivery { request_id } => {
                let delivery = crate::live::delivery_readonly_in(workspace, request_id, proc)?;
                Ok((delivery["receipt"]["status"]!="waiting").then(||json!({"outcome":"event","kind":"delivery","request_id":request_id,"delivery":delivery})))
            }
        }
    }
}

pub(crate) fn open_readonly(workspace: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(
        workspace.join(".soudan/state.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.busy_timeout(Duration::from_millis(100))?;
    Ok(db)
}
fn locked(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<rusqlite::Error>(),Some(rusqlite::Error::SqliteFailure(e,_)) if matches!(e.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked))
}
/// Runs only inside the supervised observer. No DB connection survives a poll.
pub async fn observe(workspace: &Path, watch: &Watch, timeout: u64) -> Result<Value> {
    observe_in(workspace, watch, timeout, Path::new("/proc")).await
}

/// Injectable process directory for receipt wait tests.
pub async fn observe_in(
    workspace: &Path,
    watch: &Watch,
    timeout: u64,
    proc: &Path,
) -> Result<Value> {
    ensure!(
        (1..=MAX_TIMEOUT).contains(&timeout),
        "timeout must be 1–{MAX_TIMEOUT} seconds"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    watch.validate()?;
    loop {
        match watch.snapshot(workspace, proc) {
            Ok(Some(event)) => return Ok(event),
            Ok(None) => (),
            Err(e) if locked(&e) => (),
            Err(e) => return Err(e),
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(json!({"outcome":"timeout","timeout_seconds":timeout,"watch":watch}));
        }
        tokio::time::sleep_until((now + Duration::from_secs(1)).min(deadline)).await;
    }
}

pub fn error(error: impl std::fmt::Display) -> Value {
    json!({"outcome":"error","error":error.to_string()})
}
/// A separate observer can be killed at the deadline even during a long log scan.
/// Its stdout is forwarded unchanged only after a complete event/error response.
pub async fn supervise(workspace: &Path, watch: &Watch, timeout: u64) -> Value {
    let run = async {
        ensure!(
            (1..=MAX_TIMEOUT).contains(&timeout),
            "timeout must be 1–{MAX_TIMEOUT} seconds"
        );
        watch.validate()?;
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .arg("--workspace")
            .arg(workspace)
            .arg("wait-observer")
            .arg(serde_json::to_string(watch)?)
            .arg(timeout.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("Cannot start wait observer")?;
        // Retain the Child so timeout can explicitly terminate and reap it.
        let stdout = child.stdout.take().context("Missing observer stdout")?;
        let stderr = child.stderr.take().context("Missing observer stderr")?;
        let read = async {
            use tokio::io::AsyncReadExt;
            let mut bytes = Vec::new();
            let mut errors = Vec::new();
            let mut stdout = stdout.take(64 * 1024 * 1024);
            let mut stderr = stderr.take(65536);
            let (out, err, status) = tokio::join!(
                stdout.read_to_end(&mut bytes),
                stderr.read_to_end(&mut errors),
                child.wait()
            );
            out?;
            err?;
            let status = status?;
            ensure!(
                status.success(),
                "Wait observer failed: {}",
                String::from_utf8_lossy(&errors)
            );
            Ok::<Value, anyhow::Error>(serde_json::from_slice(&bytes)?)
        };
        match tokio::time::timeout(Duration::from_secs(timeout), read).await {
            Ok(result) => result,
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_millis(250), child.wait()).await;
                Ok(json!({"outcome":"timeout","timeout_seconds":timeout,"watch":watch}))
            }
        }
    };
    run.await.unwrap_or_else(|e| error(format!("{e:#}")))
}
pub fn exit_code(value: &Value) -> i32 {
    match value["outcome"].as_str() {
        Some("event") => 0,
        Some("timeout") => 124,
        _ => 1,
    }
}
