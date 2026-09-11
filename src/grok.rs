//! Grok Build reaches its open sessions through a shared leader process.
//!
//! The leader speaks [ACP](https://agentclientprotocol.com) inside a private
//! envelope: length-prefixed frames carrying a registration handshake and then
//! JSON-RPC as a string. Everything here is that envelope; the conversation
//! itself is plain ACP, so a message reaches the chat a person is looking at
//! without a keystroke and without waiting for it to fall idle.
//!
//! The leader only exists when the target was started with `use_leader = true`,
//! which is a launch-time choice. A session opened without it is discoverable
//! but not reachable, and says so rather than being driven some other way.
use crate::codex::Failure;
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// One open Grok chat, and the files that prove what reached it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub pid: u32,
    pub cwd: PathBuf,
    /// The leader's socket. Absent when this Grok was started without one.
    pub leader: PathBuf,
    /// Where this session's transcripts live. Named by an encoding of the
    /// workspace path, so it is found by session id rather than reconstructed.
    pub dir: PathBuf,
}

impl Session {
    /// The file a receipt is measured against.
    ///
    /// `chat_history.jsonl` exists from the moment a session is created, while
    /// `updates.jsonl` only appears once something streams. Anchoring on the
    /// former means a message sent into a brand-new chat still has a provable
    /// send-time boundary instead of silently becoming unknown forever.
    pub fn evidence(&self) -> PathBuf {
        self.dir.join("chat_history.jsonl")
    }
    /// The structured ACP update stream, once the session has produced one.
    pub fn updates(&self) -> PathBuf {
        self.dir.join("updates.jsonl")
    }
}

#[derive(Deserialize)]
struct Registered {
    session_id: String,
    pid: u32,
    cwd: PathBuf,
}

fn is_session_id(text: &str) -> bool {
    text.len() == 36
        && text.chars().enumerate().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Resolve the session a process is running, from Grok's own registry.
///
/// The registry is keyed by PID, so the caller's already-validated process
/// identity carries over; a stale entry for a dead PID resolves to nothing
/// rather than to whatever reused that number.
pub fn session(home: &Path, workspace: &Path, pid: u32) -> Result<Option<Session>> {
    let Ok(bytes) = std::fs::read(home.join("active_sessions.json")) else {
        return Ok(None);
    };
    ensure!(
        bytes.len() <= 1_048_576,
        "Grok session registry is too large"
    );
    let registered: Vec<Registered> =
        serde_json::from_slice(&bytes).context("Unreadable Grok session registry")?;
    let Some(entry) = registered.into_iter().find(|r| r.pid == pid) else {
        return Ok(None);
    };
    if entry.cwd != workspace {
        return Ok(None);
    }
    ensure!(
        is_session_id(&entry.session_id),
        "Grok registry holds an unusable session id"
    );
    // The transcript directory is named by an encoding of the workspace path.
    // Finding it by session id instead keeps this working if that encoding
    // changes, and a UUID is unambiguous across workspaces anyway.
    let dir = find_dir(&home.join("sessions"), &entry.session_id)
        .context("Grok session has no transcript directory; rediscover the target")?;
    Ok(Some(Session {
        id: entry.session_id,
        pid,
        cwd: entry.cwd,
        leader: home.join("leader.sock"),
        dir,
    }))
}

fn find_dir(sessions: &Path, id: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(sessions).ok()? {
        let candidate = entry.ok()?.path().join(id);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Report what the session last said, and whether it is mid-turn.
///
/// A turn that has not ended must not look answered, so an unfinished stream
/// reports `running` while still carrying the previous reply.
pub fn state(session: &Session) -> Result<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let mut status = "unknown";
    let mut last: Option<String> = None;
    if let Ok(mut file) = std::fs::File::open(session.dir.join("events.jsonl")) {
        let length = file.metadata()?.len();
        let start = length.saturating_sub(262144);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(262144).read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines().skip(usize::from(start > 0)) {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match event["type"].as_str() {
                Some("turn_started") => status = "running",
                Some("turn_ended") => {
                    status = match event["outcome"].as_str() {
                        // An interrupted turn is over, but it did not answer.
                        Some("completed") => "idle",
                        _ => "aborted",
                    }
                }
                _ => {}
            }
        }
    }
    // Replies arrive as a run of chunks, so the last one alone is a fragment.
    // Collect backwards to the turn that produced them and reassemble in order.
    if let Ok(text) = std::fs::read_to_string(session.updates()) {
        let mut chunks: Vec<String> = vec![];
        for line in text.lines().rev().take(20000) {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let update = &record["params"]["update"];
            match update["sessionUpdate"].as_str() {
                Some("agent_message_chunk") => {
                    if let Some(chunk) = update["content"]["text"].as_str() {
                        chunks.push(chunk.to_owned());
                    }
                }
                // Anything the user or a previous turn contributed ends the reply.
                Some("user_message_chunk") if !chunks.is_empty() => break,
                _ => {}
            }
        }
        if !chunks.is_empty() {
            chunks.reverse();
            last = Some(chunks.concat());
        }
    }
    Ok(json!({
        "kind": "grok_session",
        "session": session.id,
        "status": status,
        "last_agent_message": last,
    }))
}

/// A leader connection, framed the way the leader expects.
struct Leader {
    stream: tokio::net::UnixStream,
    buffer: Vec<u8>,
    id: i64,
}

const MAX_FRAME: usize = 1_048_576;

impl Leader {
    async fn connect(path: &Path) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .context("Grok leader is not running; start Grok with use_leader = true")?;
        Ok(Self {
            stream,
            buffer: vec![],
            id: 0,
        })
    }
    async fn write(&mut self, frame: &Value) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let body = serde_json::to_vec(frame)?;
        ensure!(body.len() <= MAX_FRAME, "Grok leader frame is too large");
        let length = u32::try_from(body.len())?.to_be_bytes();
        self.stream.write_all(&length).await?;
        self.stream.write_all(&body).await?;
        Ok(())
    }
    /// The leader carries JSON-RPC as a string, not as a nested object.
    async fn call(&mut self, method: &str, params: Value) -> Result<i64> {
        self.id += 1;
        let payload = json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        self.write(&json!({"type":"acp","payload":payload.to_string()}))
            .await?;
        Ok(self.id)
    }
    async fn read(&mut self) -> Result<Value> {
        use tokio::io::AsyncReadExt;
        loop {
            if self.buffer.len() >= 4 {
                let length = u32::from_be_bytes(self.buffer[..4].try_into()?) as usize;
                ensure!(length <= MAX_FRAME, "Grok leader frame is too large");
                if self.buffer.len() >= 4 + length {
                    let body: Value = serde_json::from_slice(&self.buffer[4..4 + length])?;
                    self.buffer.drain(..4 + length);
                    return Ok(body);
                }
            }
            let mut chunk = [0u8; 65536];
            let read = self.stream.read(&mut chunk).await?;
            ensure!(read > 0, "Grok leader closed the connection");
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
    /// Read frames until the reply to `id` arrives, discarding notifications.
    async fn reply(&mut self, id: i64) -> Result<Value> {
        loop {
            let frame = self.read().await?;
            if frame["type"] != "acp" {
                continue;
            }
            let Some(text) = frame["payload"].as_str() else {
                continue;
            };
            let message: Value = serde_json::from_str(text)?;
            if message["id"].as_i64() == Some(id) {
                if let Some(error) = message.get("error").filter(|e| !e.is_null()) {
                    bail!("Grok: {error}");
                }
                return Ok(message["result"].clone());
            }
        }
    }
}

/// Hand a message to an open Grok chat through its leader.
///
/// The prompt is written and the connection closed without waiting for the
/// turn: the leader keeps driving the session after a client disconnects, and
/// a turn can run for minutes. Delivery is therefore reported as handed over,
/// never as answered, exactly as the other transports do.
///
/// Everything before the prompt leaves this process is a failure that provably
/// delivered nothing. Once it is written, no error can prove otherwise.
pub async fn send(session: &Session, text: &str, request_id: &str) -> Result<(), Failure> {
    let work = async {
        let mut leader = Leader::connect(&session.leader).await?;
        leader
            .write(&json!({
                "type": "register",
                "client_type": "soudan",
                "mode": "stdio",
                "capabilities": {
                    "yolo_mode": false, "auto_mode": false, "default_model": null,
                    "client_version": null, "code_nav_enabled": false, "terminal": false,
                    "fs_read": false, "fs_write": false, "status_line": false,
                    "user_message_echo": false
                }
            }))
            .await?;
        let registered = leader.read().await?;
        ensure!(
            registered["type"] == "registered",
            "Grok leader refused the connection: {registered}"
        );
        ensure!(
            registered["leader_protocol_version"] == 1,
            "Unsupported Grok leader protocol; expected version 1"
        );
        let id = leader
            .call(
                "initialize",
                json!({"protocolVersion":1,
                       "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false}}}),
            )
            .await?;
        let initialized = leader.reply(id).await?;
        ensure!(
            initialized["agentCapabilities"]["sessionCapabilities"]["resume"].is_object(),
            "This Grok cannot resume an existing session"
        );
        let id = leader
            .call(
                "session/resume",
                json!({"sessionId":session.id,"cwd":session.cwd,"mcpServers":[]}),
            )
            .await?;
        leader.reply(id).await?;
        Ok::<_, anyhow::Error>(leader)
    };
    // Reaching the session is bounded; the turn it starts is not.
    let mut leader = match tokio::time::timeout(Duration::from_secs(30), work).await {
        Err(elapsed) => {
            return Err(Failure::NotAttempted(
                anyhow::Error::new(elapsed).context("Grok leader did not respond"),
            ));
        }
        Ok(Err(error)) => return Err(Failure::NotAttempted(error)),
        Ok(Ok(leader)) => leader,
    };
    let prompt = json!({
        "sessionId": session.id,
        "prompt": [{"type":"text","text":format!("[Soudan {request_id}] {text}")}],
    });
    match tokio::time::timeout(
        Duration::from_secs(30),
        leader.call("session/prompt", prompt),
    )
    .await
    {
        Err(elapsed) => Err(Failure::Uncertain(
            anyhow::Error::new(elapsed).context("Grok prompt may have been written"),
        )),
        Ok(Err(error)) => Err(Failure::Uncertain(
            error.context("Grok prompt write failed"),
        )),
        Ok(Ok(_)) => Ok(()),
    }
}
