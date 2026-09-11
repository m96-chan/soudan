//! Claude Code peer-protocol v1: deliver to the running session's own inbox.
//! No terminal input, resumed process, recipient token, or permission assertion.
use crate::codex::Failure;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub pid: u32,
    pub start_time: u64,
    pub socket: PathBuf,
    pub status: String,
    pub transcript: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    pid: u32,
    session_id: String,
    cwd: PathBuf,
    proc_start: String,
    peer_protocol: u32,
    messaging_socket_path: PathBuf,
    #[serde(default)]
    status: String,
}

pub fn process_start(proc: &Path, pid: u32) -> Result<u64> {
    let stat = std::fs::read_to_string(proc.join(pid.to_string()).join("stat"))?;
    stat.rsplit_once(')')
        .context("Invalid process stat")?
        .1
        .split_whitespace()
        .nth(19)
        .context("Missing process start time")?
        .parse()
        .context("Invalid process start time")
}

/// Resolve only the record belonging to the selected PID and process lifetime.
pub fn session(config: &Path, workspace: &Path, pid: u32, start: u64) -> Result<Session> {
    use std::io::Read;
    let file = config.join(format!("sessions/{pid}.json"));
    ensure!(
        !std::fs::symlink_metadata(&file)?.file_type().is_symlink(),
        "Claude session record is a symlink"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(file)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "Claude session record is too large");
    let record: Record = serde_json::from_slice(&bytes)
        .context("Unreadable Claude session registry; rediscover the target")?;
    ensure!(
        record.pid == pid && record.proc_start.parse::<u64>()? == start,
        "Claude process identity changed"
    );
    ensure!(
        record.cwd == workspace,
        "Claude session belongs to another workspace"
    );
    ensure!(
        record.peer_protocol == 1,
        "Unsupported Claude peer protocol; expected v1"
    );
    let id = uuid::Uuid::parse_str(&record.session_id)?;
    ensure!(
        id.hyphenated().to_string() == record.session_id,
        "Invalid canonical Claude session ID"
    );
    ensure!(
        record.messaging_socket_path.is_absolute(),
        "Claude inbox path must be absolute"
    );
    // Claude's project transcript layout. A missing transcript does not prevent sending.
    let project: String = workspace
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Ok(Session {
        transcript: config
            .join("projects")
            .join(project)
            .join(format!("{id}.jsonl")),
        id: record.session_id,
        pid,
        start_time: start,
        socket: record.messaging_socket_path,
        status: match record.status.as_str() {
            "busy" => "running",
            "idle" => "idle",
            _ => "unknown",
        }
        .into(),
    })
}

/// Use the target process's config directory, not another client's configuration.
#[cfg(target_os = "linux")]
pub fn for_target(workspace: &Path, target: &crate::live::LiveTarget) -> Result<Session> {
    crate::live::validate_target(Path::new("/proc"), workspace, target)?;
    let env = std::fs::read(format!("/proc/{}/environ", target.pid))?;
    let get = |key: &[u8]| -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        env.split(|b| *b == 0)
            .find_map(|e| e.strip_prefix(key))
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(std::ffi::OsStr::from_bytes(v)))
    };
    let config = get(b"CLAUDE_CONFIG_DIR=")
        .or_else(|| get(b"HOME=").map(|h| h.join(".claude")))
        .context("Cannot locate target Claude configuration")?;
    session(&config, workspace, target.pid, target.start_time)
}
#[cfg(not(target_os = "linux"))]
pub fn for_target(_: &Path, _: &crate::live::LiveTarget) -> Result<Session> {
    anyhow::bail!("Claude native discovery currently requires Linux")
}

pub fn message(session_id: &str, text: &str, request_id: &str) -> Result<String> {
    message_as(session_id, text, request_id, None)
}
pub fn message_as(
    session_id: &str,
    text: &str,
    request_id: &str,
    sender: Option<&str>,
) -> Result<String> {
    uuid::Uuid::parse_str(session_id)?;
    crate::live::validate_message(text)?;
    crate::live::validate_request_id(request_id)?;
    let text = crate::live::prompt(request_id, text, sender)?
        .replace('&', "&amp;")
        .replace('<', "&lt;");
    let content =
        format!("<cross-session-message from-name=\"Soudan\">\n{text}\n</cross-session-message>");
    Ok(format!(
        "{}\n",
        json!({"msgV":1,"msg_id":uuid::Uuid::new_v4().to_string(),"session_id":session_id,
        "type":"user","message":{"role":"user","content":content},"priority":"next"})
    ))
}

/// A completed write is submitted, never an assertion of receipt or policy acceptance.
#[cfg(target_os = "linux")]
pub async fn send_as(
    target: &Session,
    text: &str,
    request_id: &str,
    sender: Option<&str>,
) -> Result<(), Failure> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    let wire = message_as(&target.id, text, request_id, sender).map_err(Failure::NotAttempted)?;
    let connect = async {
        let meta = std::fs::symlink_metadata(&target.socket)?;
        ensure!(
            meta.file_type().is_socket(),
            "Claude inbox is not a socket (symlinks are refused)"
        );
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        ensure!(meta.uid() == uid, "Claude inbox belongs to another user");
        let parent = target.socket.parent().context("Missing inbox directory")?;
        let dir = std::fs::symlink_metadata(parent)?;
        ensure!(
            dir.is_dir() && dir.uid() == uid && dir.mode() & 0o022 == 0,
            "Unsafe Claude inbox directory"
        );
        let stream = tokio::net::UnixStream::connect(&target.socket).await?;
        let peer = stream.peer_cred()?;
        ensure!(
            peer.pid() == Some(target.pid as i32) && peer.uid() == uid,
            "Claude inbox peer identity mismatch"
        );
        ensure!(
            process_start(Path::new("/proc"), target.pid)? == target.start_time,
            "Claude process was replaced"
        );
        Ok::<_, anyhow::Error>(stream)
    };
    let mut stream = tokio::time::timeout(Duration::from_secs(5), connect)
        .await
        .map_err(|e| Failure::NotAttempted(e.into()))?
        .map_err(Failure::NotAttempted)?;
    // Once writing starts, any error can follow partial or complete delivery.
    tokio::time::timeout(Duration::from_secs(5), async {
        stream.write_all(wire.as_bytes()).await?;
        stream.shutdown().await
    })
    .await
    .map_err(|e| Failure::Uncertain(e.into()))?
    .map_err(|e| Failure::Uncertain(e.into()))
}
#[cfg(not(target_os = "linux"))]
pub async fn send_as(_: &Session, _: &str, _: &str, _: Option<&str>) -> Result<(), Failure> {
    Err(Failure::NotAttempted(anyhow::anyhow!(
        "Claude native delivery currently requires Linux"
    )))
}

pub fn state(target: &Session) -> Result<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let mut last = None;
    let mut transcript_available = false;
    match std::fs::File::open(&target.transcript) {
        Ok(mut file) => {
            let start = file.metadata()?.len().saturating_sub(262144);
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            file.take(262144).read_to_end(&mut bytes)?;
            let text = String::from_utf8_lossy(&bytes);
            for line in text.lines().skip(usize::from(start > 0)) {
                let Ok(record) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if record["sessionId"] != target.id || record["type"] != "assistant" {
                    continue;
                }
                if let Some(blocks) = record["message"]["content"].as_array() {
                    let text = blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        last = Some(text);
                    }
                }
            }
            transcript_available = true;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(
        json!({"kind":"claude_session","session":target.id,"status":target.status,
        "last_agent_message":last,"transcript_available":transcript_available}),
    )
}

pub async fn send(target: &Session, text: &str, request_id: &str) -> Result<(), Failure> {
    send_as(target, text, request_id, None).await
}
