//! Copilot TUI+server JSON-RPC v3 transport. Attach only to the foreground
//! session of the original process; never create sessions or drive a composer.
use crate::{codex::Failure, live::LiveTarget};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const LIMIT: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub pid: u32,
    pub start_time: u64,
    pub port: u16,
    pub id: String,
    pub workspace: PathBuf,
    pub log: PathBuf,
}
impl Session {
    pub fn target(&self) -> String {
        format!(
            "copilot:{}:{}:{}:{}",
            self.pid, self.start_time, self.port, self.id
        )
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub kind: String,
    pub session: Session,
    pub boundary: crate::receipt::Basis,
    pub prompt: String,
    pub message_id: Option<String>,
}
impl Evidence {
    pub fn capture(session: Session, prompt: String) -> Result<Self> {
        Ok(Self {
            kind: "copilot_message_v1".into(),
            boundary: crate::receipt::Basis::capture(&session.log)?,
            session,
            prompt,
            message_id: None,
        })
    }
}
fn identifier(id: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok() && id.len() == 36,
        "Invalid Copilot session/message id"
    );
    Ok(())
}
fn process_matches(proc: &Path, workspace: &Path, pid: u32, start: u64) -> Result<()> {
    ensure!(
        crate::claude::process_start(proc, pid)? == start,
        "Copilot process exited or was replaced"
    );
    ensure!(
        std::fs::read_link(proc.join(format!("{pid}/cwd")))? == workspace,
        "Copilot belongs to another workspace"
    );
    ensure!(
        std::fs::read_link(proc.join(format!("{pid}/exe")))?
            .file_name()
            .and_then(|s| s.to_str())
            == Some("copilot"),
        "Target is not a Copilot process"
    );
    Ok(())
}
fn log_path(proc: &Path, pid: u32, id: &str) -> Result<PathBuf> {
    let env = std::fs::read(proc.join(format!("{pid}/environ")))?;
    let var = |key: &[u8]| {
        env.split(|b| *b == 0)
            .find_map(|e| e.strip_prefix(key))
            .filter(|v| !v.is_empty())
    };
    // Only home paths are read. Authentication must be explicitly supplied to Soudan.
    let home = if let Some(value) = var(b"COPILOT_HOME=") {
        PathBuf::from(std::str::from_utf8(value)?)
    } else {
        PathBuf::from(std::str::from_utf8(
            var(b"HOME=").context("Cannot locate Copilot home")?,
        )?)
        .join(".copilot")
    };
    ensure!(home.is_absolute(), "Copilot home must be absolute");
    let root = home.join("session-state").canonicalize()?;
    let log = root.join(id).join("events.jsonl");
    ensure!(
        log.canonicalize()? == log,
        "Copilot event path contains a symlink"
    );
    Ok(log)
}
pub fn resolve_in(proc: &Path, workspace: &Path, target: &str) -> Result<Session> {
    let p: Vec<_> = target.split(':').collect();
    ensure!(
        p.len() == 5 && p[0] == "copilot",
        "Copilot target must be copilot:<pid>:<start>:<port>:<session>"
    );
    identifier(p[4])?;
    let (pid, start_time, port) = (p[1].parse()?, p[2].parse()?, p[3].parse()?);
    process_matches(proc, workspace, pid, start_time)?;
    ensure!(
        crate::opencode::ports_in(proc, pid)?.contains(&port),
        "Copilot no longer owns this loopback listener; start copilot --ui-server --host 127.0.0.1 --port N and rediscover"
    );
    let session = Session {
        pid,
        start_time,
        port,
        id: p[4].into(),
        workspace: workspace.into(),
        log: log_path(proc, pid, p[4])?,
    };
    events(&session)?;
    Ok(session)
}

/// Content-Length framed JSON-RPC, not ACP's newline framing. A single absolute
/// deadline and byte budget include notifications and unsolicited requests.
struct Rpc {
    reader: BufReader<TcpStream>,
    deadline: Instant,
    remaining: usize,
    id: u64,
}
impl Rpc {
    fn connect(port: u16) -> Result<Self> {
        let deadline = Instant::now() + TIMEOUT;
        let stream = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Duration::from_secs(2),
        )?;
        let mut rpc = Self {
            reader: BufReader::new(stream),
            deadline,
            remaining: LIMIT,
            id: 0,
        };
        let mut params = json!({});
        if let Ok(token) = std::env::var("COPILOT_CONNECTION_TOKEN")
            && !token.is_empty()
        {
            params["connectionToken"] = token.into();
        }
        let result = rpc.call("connect", params)?;
        ensure!(
            result["ok"] == true && result["protocolVersion"] == 3,
            "Unsupported Copilot JSON-RPC handshake; expected protocol 3"
        );
        Ok(rpc)
    }
    fn arm(&self) -> Result<()> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .context("Copilot API deadline exceeded")?;
        self.reader.get_ref().set_read_timeout(Some(remaining))?;
        self.reader.get_ref().set_write_timeout(Some(remaining))?;
        Ok(())
    }
    fn write(&mut self, value: Value) -> Result<()> {
        self.arm()?;
        let body = serde_json::to_vec(&value)?;
        write!(
            self.reader.get_mut(),
            "Content-Length: {}\r\n\r\n",
            body.len()
        )?;
        self.arm()?;
        self.reader.get_mut().write_all(&body)?;
        Ok(())
    }
    fn read(&mut self) -> Result<Value> {
        let mut header = Vec::new();
        loop {
            self.arm()?;
            let mut byte = [0];
            self.reader.read_exact(&mut byte)?;
            header.push(byte[0]);
            ensure!(header.len() <= 4096, "Copilot RPC header too large");
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let mut length = None;
        for line in std::str::from_utf8(&header)?
            .split("\r\n")
            .filter(|s| !s.is_empty())
        {
            let (key, value) = line
                .split_once(':')
                .context("Malformed Copilot RPC header")?;
            if key.eq_ignore_ascii_case("content-length") {
                ensure!(length.is_none(), "Duplicate Content-Length");
                length = Some(value.trim().parse::<usize>()?);
            }
        }
        let length = length.context("Missing Copilot RPC Content-Length")?;
        ensure!(
            length <= self.remaining.saturating_sub(header.len()),
            "Copilot RPC exceeded observation budget"
        );
        self.remaining -= length + header.len();
        let mut bytes = vec![0; length];
        let mut offset = 0;
        while offset < length {
            self.arm()?;
            let n = self.reader.read(&mut bytes[offset..])?;
            ensure!(n > 0, "Copilot closed an incomplete frame");
            offset += n;
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        self.write(json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params}))?;
        loop {
            let msg = self.read()?;
            ensure!(msg["jsonrpc"] == "2.0", "Invalid Copilot JSON-RPC envelope");
            if msg.get("method").is_some() {
                if let Some(id) = msg.get("id") {
                    // Never become a permission, tool, or user-input handler.
                    self.write(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Soudan does not handle client requests"}}))?;
                }
                continue;
            }
            ensure!(msg["id"] == self.id, "Unexpected Copilot RPC response id");
            // Do not leak remote error bodies or connection secrets into receipts.
            ensure!(
                msg.get("error").is_none(),
                "Copilot rejected {method} (RPC error code {})",
                msg["error"]["code"]
            );
            return msg
                .get("result")
                .cloned()
                .context("Missing Copilot RPC result");
        }
    }
    fn foreground(&mut self) -> Result<String> {
        let value = self.call("session.getForeground", json!({}))?;
        let id = value["sessionId"]
            .as_str()
            .context("Copilot has no foreground TUI session; use --ui-server")?;
        identifier(id)?;
        Ok(id.into())
    }
}

pub fn discover_in(proc: &Path, workspace: &Path, target: &LiveTarget) -> Result<Vec<LiveTarget>> {
    process_matches(proc, workspace, target.pid, target.start_time)?;
    let mut targets = vec![];
    // Most CLIs expose one listener. Cap probes so a process cannot cause an unbounded scan.
    for port in crate::opencode::ports_in(proc, target.pid)?
        .into_iter()
        .take(4)
    {
        let found = (|| -> Result<LiveTarget> {
            let mut rpc = Rpc::connect(port)?;
            let id = rpc.foreground()?;
            let id = format!("copilot:{}:{}:{port}:{id}", target.pid, target.start_time);
            resolve_in(proc, workspace, &id)?;
            Ok(LiveTarget {
                id,
                ..target.clone()
            })
        })();
        if let Ok(found) = found {
            targets.push(found);
        }
    }
    Ok(targets)
}

/// Read persistent events without resuming the session or writing to Copilot.
fn events(session: &Session) -> Result<Vec<Value>> {
    let file = File::open(&session.log)?;
    ensure!(
        file.metadata()?.is_file(),
        "Copilot log is not a regular file"
    );
    let mut bytes = vec![];
    file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= LIMIT,
        "Copilot event log exceeds 16 MiB observation limit"
    );
    let end = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let mut events = vec![];
    let mut id = None;
    let mut cwd = None;
    for line in bytes[..end]
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
    {
        let event: Value = serde_json::from_slice(line)?;
        if !event["agentId"].is_null() {
            continue;
        }
        if event["type"] == "session.context_changed" {
            cwd = event["data"]["cwd"].as_str().map(PathBuf::from);
        }
        if event["type"] == "session.start" {
            id = event["data"]["sessionId"].as_str().map(String::from);
        }
        if matches!(
            event["type"].as_str(),
            Some("session.start" | "session.resume")
        ) && let Some(path) = event["data"]["context"]["cwd"].as_str()
        {
            cwd = Some(PathBuf::from(path));
        }
        events.push(event);
    }
    ensure!(
        id.as_deref() == Some(session.id.as_str())
            && cwd.as_deref() == Some(session.workspace.as_path()),
        "Copilot event session/workspace does not match target"
    );
    Ok(events)
}
pub fn state_from_events(events: &[Value]) -> Value {
    let mut state = json!({"kind":"copilot_session","status":"unknown","last_agent_message":null,"reply_scope":"session_latest"});
    for event in events {
        if !event["agentId"].is_null() {
            continue;
        }
        match event["type"].as_str() {
            Some("user.message" | "assistant.turn_start") => state["status"] = "running".into(),
            Some("assistant.turn_end" | "session.idle") => state["status"] = "idle".into(),
            Some("session.error") => state["status"] = "error".into(),
            Some("abort") => state["status"] = "aborted".into(),
            Some("assistant.message") if event["data"]["parentToolCallId"].is_null() => {
                if let Some(text) = event["data"]["content"].as_str() {
                    state["last_agent_message"] =
                        text.chars().take(65536).collect::<String>().into();
                    state["message_id"] = event["data"]["messageId"].clone();
                    state["interaction_id"] = event["data"]["interactionId"].clone();
                }
            }
            _ => {}
        }
    }
    state
}
pub fn state(proc: &Path, workspace: &Path, target: &str) -> Result<Value> {
    let session = resolve_in(proc, workspace, target)?;
    let mut state = state_from_events(&events(&session)?);
    state["session"] = session.id.into();
    state["target"] =
        json!({"id":target,"agent":"copilot","pid":session.pid,"start_time":session.start_time});
    Ok(state)
}

pub fn send(proc: &Path, evidence: &Evidence) -> std::result::Result<String, Failure> {
    let before = || -> Result<Rpc> {
        let session = resolve_in(
            proc,
            &evidence.session.workspace,
            &evidence.session.target(),
        )?;
        ensure!(
            session.log == evidence.session.log,
            "Copilot log path changed"
        );
        let mut rpc = Rpc::connect(session.port)?;
        ensure!(
            rpc.foreground()? == session.id,
            "Copilot foreground changed; rediscover before sending"
        );
        // Binding this connection to the already-foreground session is required by
        // the SDK. Omit model/tools/permissions so the TUI remains the owner.
        let resumed = rpc.call("session.resume", json!({"sessionId":session.id}))?;
        ensure!(
            resumed["sessionId"] == session.id && resumed["isRemote"] == false,
            "Copilot did not attach to the expected local session"
        );
        ensure!(
            rpc.foreground()? == session.id,
            "Copilot foreground changed during attach"
        );
        process_matches(proc, &session.workspace, session.pid, session.start_time)?;
        ensure!(
            crate::opencode::ports_in(proc, session.pid)?.contains(&session.port),
            "Copilot listener changed before send"
        );
        Ok(rpc)
    };
    let mut rpc = before().map_err(Failure::NotAttempted)?;
    let result = rpc
        .call(
            "session.send",
            json!({"sessionId":evidence.session.id,"prompt":evidence.prompt,"mode":"enqueue"}),
        )
        .map_err(Failure::Uncertain)?;
    let id = result["messageId"]
        .as_str()
        .context("Copilot returned no message ID")
        .map_err(Failure::Uncertain)?;
    identifier(id).map_err(Failure::Uncertain)?;
    // Drop only this socket. session.destroy/abort would affect the user's chat.
    Ok(id.into())
}

pub fn observe(basis: Option<&str>, proc: &Path, workspace: &Path, target: &str) -> Value {
    let result = || -> Result<Value> {
        let evidence: Evidence =
            serde_json::from_str(basis.context("No Copilot delivery evidence")?)?;
        ensure!(
            evidence.kind == "copilot_message_v1"
                && evidence.session.target() == target
                && evidence.session.workspace == workspace,
            "Copilot evidence target mismatch"
        );
        let id = evidence.message_id.as_deref().context("No acknowledged message ID; delivery is uncertain and must not be retried automatically")?;
        identifier(id)?;
        let events = evidence.boundary.events_since(&evidence.session.log)?;
        if events.iter().any(|event| {
            event["type"] == "user.message"
                && event["agentId"].is_null()
                && event["data"]["messageId"] == id
                && event["data"]["content"] == evidence.prompt
        }) {
            return Ok(
                json!({"status":"taken","reason":"Exact acknowledged Copilot user message ID and content observed after the send boundary; not a reply acknowledgement","message_id":id}),
            );
        }
        let alive = crate::claude::process_start(proc, evidence.session.pid)?
            == evidence.session.start_time;
        Ok(
            json!({"status":if alive {"waiting"} else {"lost"},"reason":"Acknowledged user message is not yet present in the covered event log"}),
        )
    };
    result().unwrap_or_else(|e| crate::receipt::unknown(&format!("{e:#}")))
}
