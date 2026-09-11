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
    Room {
        room: String,
        after: i64,
    },
    Delivery {
        request_id: String,
    },
    Reply {
        request_id: String,
        room: Option<String>,
        after: i64,
    },
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
            Self::Reply {
                request_id,
                room,
                after,
            } => {
                crate::live::validate_request_id(request_id)?;
                ensure!(*after >= 0, "after must be nonnegative");
                if let Some(room) = room {
                    Self::Room {
                        room: room.clone(),
                        after: *after,
                    }
                    .validate()?;
                }
            }
            Self::Delivery { request_id } => crate::live::validate_request_id(request_id)?,
        }
        Ok(())
    }
    fn snapshot(&self, workspace: &Path, proc: &Path) -> Result<Option<Value>> {
        match self {
            Self::Room { room, after } => {
                let messages = message_snapshot(workspace, Some(room), *after, None)?;
                Ok(messages.last().map(|last|json!({"outcome":"event","kind":"room","room":room,"after":after,"last_id":last["id"],"messages":messages})))
            }
            Self::Reply {
                request_id,
                room,
                after,
            } => {
                let messages =
                    message_snapshot(workspace, room.as_deref(), *after, Some(request_id))?;
                Ok(messages.last().map(|last|json!({"outcome":"event","kind":"reply","request_id":request_id,"room":room,"after":after,"last_id":last["id"],"messages":messages})))
            }
            Self::Delivery { request_id } => {
                let delivery = crate::live::delivery_readonly_in(workspace, request_id, proc)?;
                Ok((delivery["receipt"]["status"]!="waiting").then(||json!({"outcome":"event","kind":"delivery","request_id":request_id,"delivery":delivery})))
            }
        }
    }
}

fn message_snapshot(
    workspace: &Path,
    room: Option<&str>,
    after: i64,
    reply: Option<&str>,
) -> Result<Vec<Value>> {
    match std::fs::metadata(workspace.join(".soudan/state.db")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
        Ok(_) => (),
    }
    let db = open_readonly(workspace)?;
    let columns = {
        let mut q = db.prepare("PRAGMA table_info(messages)")?;
        q.query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let correlated = columns.iter().any(|name| name == "in_reply_to");
    if columns.is_empty() || (reply.is_some() && !correlated) {
        return Ok(vec![]);
    }
    let column = if correlated { "in_reply_to" } else { "NULL" };
    let predicate = if reply.is_some() {
        "in_reply_to=?3 AND (?1 IS NULL OR room=?1)"
    } else {
        "room=?1 AND ?3 IS NULL"
    };
    let sql = format!(
        "SELECT id,room,sender,text,{column} FROM messages WHERE {predicate} AND id>?2 ORDER BY id LIMIT ?4"
    );
    let mut q = db.prepare(&sql)?;
    Ok(q.query_map(params![room,after,reply,if reply.is_some() { 1 } else { 100 }], |r| Ok(json!({
        "id":r.get::<_,i64>(0)?, "room":r.get::<_,String>(1)?, "sender":r.get::<_,String>(2)?,
        "text":r.get::<_,String>(3)?, "in_reply_to":r.get::<_,Option<String>>(4)?
    })))?.collect::<rusqlite::Result<Vec<_>>>()?)
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
