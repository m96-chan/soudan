//! OpenCode's published v2 HTTP API, discovered through a process-owned loopback socket.
//! No composer operations, process restart, permission override, or transcript scraping.
use crate::{codex::Failure, live::LiveTarget};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub pid: u32,
    pub start_time: u64,
    pub port: u16,
    pub id: String,
    pub workspace: PathBuf,
}
impl Session {
    pub fn target(&self) -> String {
        format!(
            "opencode:{}:{}:{}:{}",
            self.pid, self.start_time, self.port, self.id
        )
    }
    fn path(&self) -> String {
        format!("/api/session/{}", self.id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub kind: String,
    pub session: Session,
    pub message_id: String,
}
impl Evidence {
    pub fn new(session: Session) -> Self {
        Self {
            kind: "opencode_message_v1".into(),
            session,
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
        }
    }
}

fn identifier(id: &str, prefix: &str) -> Result<()> {
    ensure!(
        id.starts_with(prefix)
            && id.len() > prefix.len()
            && id.len() <= 128
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "Invalid OpenCode identifier"
    );
    Ok(())
}

/// Socket inode ownership, not merely a port found in a process's network namespace.
pub fn ports_in(proc: &Path, pid: u32) -> Result<Vec<u16>> {
    let mut owned = BTreeSet::new();
    for fd in std::fs::read_dir(proc.join(format!("{pid}/fd")))? {
        let Ok(path) = std::fs::read_link(fd?.path()) else {
            continue;
        };
        let path = path.to_string_lossy();
        if let Some(inode) = path
            .strip_prefix("socket:[")
            .and_then(|s| s.strip_suffix(']'))
        {
            owned.insert(inode.to_owned());
        }
    }
    let tcp = std::fs::read_to_string(proc.join(format!("{pid}/net/tcp")))?;
    let mut ports = BTreeSet::new();
    for line in tcp.lines().skip(1) {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() < 10 || parts[3] != "0A" || !owned.contains(parts[9]) {
            continue;
        }
        let Some(port) = parts[1].strip_prefix("0100007F:") else {
            continue;
        };
        let port = u16::from_str_radix(port, 16)?;
        if port != 0 {
            ports.insert(port);
        }
    }
    Ok(ports.into_iter().collect())
}

fn process_matches(proc: &Path, workspace: &Path, pid: u32, start: u64) -> Result<()> {
    ensure!(
        crate::claude::process_start(proc, pid)? == start,
        "OpenCode process exited or was replaced"
    );
    ensure!(
        std::fs::read_link(proc.join(format!("{pid}/cwd")))? == workspace,
        "OpenCode belongs to another workspace"
    );
    let exe = std::fs::read_link(proc.join(format!("{pid}/exe")))?;
    ensure!(
        matches!(
            exe.file_name().and_then(|s| s.to_str()),
            Some("opencode" | "opencode.exe")
        ),
        "Target is not an OpenCode process"
    );
    Ok(())
}

pub fn resolve_in(proc: &Path, workspace: &Path, target: &str) -> Result<Session> {
    let parts: Vec<_> = target.split(':').collect();
    ensure!(
        parts.len() == 5 && parts[0] == "opencode",
        "OpenCode targets have form opencode:<pid>:<start>:<port>:<session>"
    );
    identifier(parts[4], "ses")?;
    let session = Session {
        pid: parts[1].parse()?,
        start_time: parts[2].parse()?,
        port: parts[3].parse()?,
        id: parts[4].into(),
        workspace: workspace.into(),
    };
    process_matches(proc, workspace, session.pid, session.start_time)?;
    ensure!(
        ports_in(proc, session.pid)?.contains(&session.port),
        "OpenCode no longer owns this loopback listener; start it with --port N --hostname 127.0.0.1 and rediscover"
    );
    Ok(session)
}

/// Credentials are explicitly supplied to Soudan, never harvested from another process.
struct Api {
    agent: ureq::Agent,
    port: u16,
    authorization: Option<String>,
    deadline: Instant,
}
impl Api {
    fn new(port: u16) -> Self {
        let authorization = std::env::var("OPENCODE_SERVER_PASSWORD")
            .ok()
            .filter(|p| !p.is_empty())
            .map(|password| {
                let username =
                    std::env::var("OPENCODE_SERVER_USERNAME").unwrap_or_else(|_| "opencode".into());
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{username}:{password}"))
                )
            });
        Self::with_auth(port, authorization)
    }
    fn with_auth(port: u16, authorization: Option<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(2)))
            .max_idle_connections(0)
            .build();
        Self {
            agent: config.into(),
            port,
            authorization,
            deadline: Instant::now() + Duration::from_secs(8),
        }
    }
    fn request(&self, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        ensure!(
            Instant::now() < self.deadline,
            "OpenCode observation deadline exceeded"
        );
        let url = format!("http://127.0.0.1:{}{path}", self.port);
        let mut response = if let Some(body) = body {
            let mut request = self
                .agent
                .post(&url)
                .header("Content-Type", "application/json");
            if let Some(auth) = &self.authorization {
                request = request.header("Authorization", auth);
            }
            request
                .send(serde_json::to_vec(body)?)
                .context("OpenCode HTTP request failed")?
        } else {
            let mut request = self.agent.get(&url);
            if let Some(auth) = &self.authorization {
                request = request.header("Authorization", auth);
            }
            request.call().context("OpenCode HTTP request failed")?
        };
        let status = response.status().as_u16();
        // Never reflect error bodies: they may contain secrets or unrelated session data.
        if status != 200 {
            return Ok((status, Value::Null));
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_to_vec()?;
        Ok((
            status,
            serde_json::from_slice(&bytes).context("OpenCode returned invalid JSON")?,
        ))
    }
    fn get(&self, path: &str) -> Result<Value> {
        let (status, body) = self.request(path, None)?;
        ensure!(
            status == 200,
            "OpenCode HTTP {status}; check server authentication and API compatibility"
        );
        Ok(body)
    }
    fn validate_session(&self, session: &Session) -> Result<()> {
        let body = self.get(&session.path())?;
        ensure!(
            body["data"]["id"] == session.id
                && body["data"]["location"]["directory"]
                    .as_str()
                    .is_some_and(|p| Path::new(p) == session.workspace),
            "OpenCode API session does not belong to this workspace"
        );
        ensure!(
            body["data"]["location"].get("workspaceID").is_none(),
            "Remote OpenCode workspaces are not supported"
        );
        Ok(())
    }
}

pub fn validate_contract(doc: &Value) -> Result<()> {
    ensure!(
        doc["openapi"]
            .as_str()
            .is_some_and(|v| v.starts_with("3.1.")),
        "OpenCode must expose an OpenAPI 3.1 document"
    );
    for (path, method, operation) in [
        ("/api/session", "get", "v2.session.list"),
        ("/api/session/{sessionID}", "get", "v2.session.get"),
        (
            "/api/session/{sessionID}/prompt",
            "post",
            "v2.session.prompt",
        ),
        (
            "/api/session/{sessionID}/message",
            "get",
            "v2.session.messages",
        ),
        (
            "/api/session/{sessionID}/message/{messageID}",
            "get",
            "v2.session.message",
        ),
        ("/api/session/active", "get", "v2.session.active"),
    ] {
        ensure!(
            doc["paths"][path][method]["operationId"] == operation,
            "Unsupported OpenCode API contract: {operation}"
        );
    }
    ensure!(
        doc["paths"]["/api/session/{sessionID}/prompt"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"]["properties"]["id"]["pattern"]
            == "^msg_",
        "OpenCode must accept a client-supplied message ID"
    );
    Ok(())
}

pub fn discover_in(proc: &Path, workspace: &Path, target: &LiveTarget) -> Result<Vec<LiveTarget>> {
    let mut result = vec![];
    for port in ports_in(proc, target.pid)?.into_iter().take(8) {
        process_matches(proc, workspace, target.pid, target.start_time)?;
        let api = Api::new(port);
        let Ok(doc) = api.get("/doc") else { continue };
        if validate_contract(&doc).is_err() {
            continue;
        }
        let mut cursor: Option<String> = None;
        let mut seen = BTreeSet::new();
        // Explicit pagination; an incomplete/looping list is an error, never a guessed target.
        loop {
            let mut path = format!(
                "/api/session?directory={}&limit=100",
                encode(&workspace.to_string_lossy())
            );
            if let Some(c) = &cursor {
                path.push_str(&format!("&cursor={}", encode(c)));
            }
            let body = api.get(&path)?;
            let rows = body["data"]
                .as_array()
                .context("Invalid OpenCode session list")?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                if row["location"]["directory"]
                    .as_str()
                    .is_none_or(|p| Path::new(p) != workspace)
                    || row["location"].get("workspaceID").is_some()
                    || row["time"].get("archived").is_some()
                {
                    continue;
                }
                let id = row["id"].as_str().context("Missing OpenCode session ID")?;
                identifier(id, "ses")?;
                let session = Session {
                    pid: target.pid,
                    start_time: target.start_time,
                    port,
                    id: id.into(),
                    workspace: workspace.into(),
                };
                let mut t = target.clone();
                t.id = session.target();
                result.push(t);
            }
            cursor = body["cursor"]["next"].as_str().map(String::from);
            let Some(c) = &cursor else { break };
            ensure!(
                seen.insert(c.clone()) && seen.len() <= 32,
                "OpenCode session pagination exceeds its bound"
            );
        }
        process_matches(proc, workspace, target.pid, target.start_time)?;
    }
    Ok(result)
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Preflight errors prove no POST was attempted. After POST starts, failures stay uncertain.
pub fn send(
    proc: &Path,
    evidence: &Evidence,
    text: &str,
    sender: Option<&str>,
    request_id: &str,
) -> Result<(), Failure> {
    let session = &evidence.session;
    let api = Api::new(session.port);
    let preflight = || -> Result<Value> {
        identifier(&evidence.message_id, "msg_")?;
        let prompt = crate::live::prompt(request_id, text, sender)?;
        resolve_in(proc, &session.workspace, &session.target())?;
        validate_contract(&api.get("/doc")?)?;
        api.validate_session(session)?;
        // Recheck process and socket ownership immediately before the write.
        resolve_in(proc, &session.workspace, &session.target())?;
        Ok(
            json!({"id":evidence.message_id,"prompt":{"text":prompt},"delivery":"queue","resume":true}),
        )
    };
    let body = preflight().map_err(Failure::NotAttempted)?;
    let result = || -> Result<()> {
        let (status, response) = api.request(&format!("{}/prompt", session.path()), Some(&body))?;
        ensure!(
            status == 200,
            "OpenCode prompt returned HTTP {status}; delivery is uncertain"
        );
        ensure!(
            response["data"]["id"] == evidence.message_id
                && response["data"]["sessionID"] == session.id,
            "OpenCode admission returned a different message or session ID"
        );
        Ok(())
    };
    result().map_err(Failure::Uncertain)
}

/// Structural positive evidence. Failure to observe it is never inferred as loss.
pub fn receipt(proc: &Path, evidence: &Evidence, target: &str) -> Value {
    let observe = || -> Result<Value> {
        ensure!(
            evidence.kind == "opencode_message_v1" && evidence.session.target() == target,
            "OpenCode evidence does not match its target"
        );
        identifier(&evidence.message_id, "msg_")?;
        let session = &evidence.session;
        resolve_in(proc, &session.workspace, target)?;
        let api = Api::new(session.port);
        api.validate_session(session)?;
        let (status, response) = api.request(
            &format!("{}/message/{}", session.path(), evidence.message_id),
            None,
        )?;
        resolve_in(proc, &session.workspace, target)?;
        if status == 404 {
            return Ok(
                json!({"status":"waiting","reason":"The original OpenCode server is alive but the admitted message is not yet projected; this is not proof of loss"}),
            );
        }
        ensure!(
            status == 200,
            "OpenCode receipt HTTP {status}; evidence unavailable"
        );
        ensure!(
            response["data"]["id"] == evidence.message_id && response["data"]["type"] == "user",
            "OpenCode returned a different message identity or type"
        );
        Ok(
            json!({"status":"taken","reason":"Exact user message ID observed in the recipient session API; this is not a reply acknowledgement","message_id":evidence.message_id}),
        )
    };
    observe().unwrap_or_else(|e| crate::receipt::unknown(&format!("{e:#}")))
}

/// Read actual assistant content via the detail endpoint, not a session-list preview.
pub fn state(proc: &Path, workspace: &Path, target: &str) -> Result<Value> {
    let session = resolve_in(proc, workspace, target)?;
    let api = Api::new(session.port);
    api.validate_session(&session)?;
    let active = api.get("/api/session/active")?;
    let statuses = active["data"]
        .as_object()
        .context("Invalid OpenCode active sessions")?;
    let status = match statuses.get(&session.id) {
        None => "idle",
        Some(v) if v["type"] == "running" => "running",
        _ => "unknown",
    };
    let mut cursor: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut reply = Value::Null;
    loop {
        let path = match &cursor {
            None => format!("{}/message?limit=100&order=desc", session.path()),
            Some(c) => format!("{}/message?limit=100&cursor={}", session.path(), encode(c)),
        };
        let page = api.get(&path)?;
        let rows = page["data"]
            .as_array()
            .context("Invalid OpenCode message list")?;
        if let Some(row) = rows.iter().find(|r| r["type"] == "assistant") {
            let id = row["id"].as_str().context("Missing assistant ID")?;
            identifier(id, "msg_")?;
            reply = api.get(&format!("{}/message/{id}", session.path()))?["data"].clone();
            ensure!(
                reply["id"] == id && reply["type"] == "assistant",
                "OpenCode returned a different assistant message"
            );
            break;
        }
        if rows.is_empty() {
            break;
        }
        cursor = page["cursor"]["next"].as_str().map(String::from);
        let Some(c) = &cursor else { break };
        ensure!(
            seen.insert(c.clone()) && seen.len() <= 32,
            "OpenCode message pagination exceeds its bound"
        );
    }
    resolve_in(proc, workspace, target)?;
    let text = reply["content"].as_array().map(|parts| {
        parts
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("")
    });
    Ok(
        json!({"kind":"opencode_session","session":session.id,"status":status,"last_agent_message":text,"reply":reply,"reply_scope":"latest_session_assistant_not_request_ack","tui_visibility":"external_messages_not_rendered"}),
    )
}

pub fn notify(proc: &Path, workspace: &Path, target: &str, text: &str) -> Result<Value> {
    crate::live::validate_message(text)?;
    let session = resolve_in(proc, workspace, target)?;
    let api = Api::new(session.port);
    api.validate_session(&session)?;
    resolve_in(proc, workspace, target)?;
    let (status, result) = api.request(
        &format!(
            "/tui/show-toast?directory={}",
            encode(&workspace.to_string_lossy())
        ),
        Some(&json!({"title":"Soudan","message":text,"variant":"info"})),
    )?;
    ensure!(
        status == 200 && result == true,
        "OpenCode did not accept the toast notification"
    );
    Ok(
        json!({"status":"submitted","kind":"toast","target":target,"next":"Toast accepted by the API, not proof that a human saw it. No conversation message was sent."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic_auth_is_explicit_and_redirects_do_not_forward_it() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(
                headers
                    .to_lowercase()
                    .contains("authorization: basic dgvzddp0b2tlbg==")
            );
            write!(stream,"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("test:token")
        );
        assert_eq!(
            Api::with_auth(port, Some(auth))
                .request("/doc", None)
                .unwrap()
                .0,
            302
        );
        server.join().unwrap();
    }
}
