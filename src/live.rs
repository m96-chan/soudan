//! Delivery to existing terminal chats. Kitty is a transport, not an agent API.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Delivery records outlive any one transport, so both of them use this table.
const DELIVERIES: &str = "CREATE TABLE IF NOT EXISTS live_deliveries(request_id TEXT PRIMARY KEY,target TEXT NOT NULL,text TEXT NOT NULL,status TEXT NOT NULL,before_screen TEXT NOT NULL,error TEXT);";

/// Migrate existing databases atomically, including concurrent bridge/server opens.
fn delivery_schema(db: &rusqlite::Connection) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(DELIVERIES)?;
    let exists = {
        let mut query = tx.prepare("PRAGMA table_info(live_deliveries)")?;
        let names = query
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        names.iter().any(|n| n == "receipt_basis")
    };
    if !exists {
        tx.execute_batch("ALTER TABLE live_deliveries ADD COLUMN receipt_basis TEXT")?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LiveTarget {
    pub id: String,
    pub agent: String,
    pub pid: u32,
    pub start_time: u64,
    pub window_id: Option<u64>,
    pub tty: PathBuf,
}

pub fn discover_in(proc: &Path, workspace: &Path) -> Result<Vec<LiveTarget>> {
    let mut targets = vec![];
    for entry in std::fs::read_dir(proc)? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Ok(Some(t)) = inspect(proc, workspace, pid) {
            targets.push(t);
        }
    }
    targets.sort_by_key(|t| t.pid);
    Ok(targets)
}
fn inspect(proc: &Path, workspace: &Path, pid: u32) -> Result<Option<LiveTarget>> {
    let path = proc.join(pid.to_string());
    if std::fs::read_link(path.join("cwd"))? != workspace {
        return Ok(None);
    }
    let exe = std::fs::read_link(path.join("exe"))?;
    let exe = exe.to_string_lossy();
    let agent = if exe.contains("/claude/") || exe.ends_with("/claude") {
        "claude-code"
    } else if exe.contains("/cursor-agent/") {
        "cursor"
    } else if exe.ends_with("/codex") {
        "codex"
    } else {
        return Ok(None);
    };
    let tty = std::fs::read_link(path.join("fd/0"))?;
    if !tty.starts_with("/dev/pts") {
        return Ok(None);
    }
    let env = std::fs::read(path.join("environ"))?;
    let window = env
        .split(|b| *b == 0)
        .find_map(|e| e.strip_prefix(b"KITTY_WINDOW_ID="));
    let window_id = match window {
        Some(window) => Some(std::str::from_utf8(window)?.parse()?),
        // Only agents delivered to through their terminal need a Kitty window.
        None if agent == "cursor" => return Ok(None),
        None => None,
    };
    let stat = std::fs::read_to_string(path.join("stat"))?;
    let (_, tail) = stat.rsplit_once(')').context("Malformed process stat")?;
    let start_time = tail
        .split_whitespace()
        .nth(19)
        .context("Missing start time")?
        .parse()?;
    // The id names the transport, because each agent has exactly one.
    let transport = match agent {
        "codex" => "codex",
        "claude-code" => "claude",
        _ => "kitty",
    };
    Ok(Some(LiveTarget {
        id: format!("{transport}:{pid}:{start_time}"),
        agent: agent.into(),
        pid,
        start_time,
        window_id,
        tty,
    }))
}
pub fn validate_target(proc: &Path, workspace: &Path, target: &LiveTarget) -> Result<()> {
    ensure!(
        inspect(proc, workspace, target.pid)?.as_ref() == Some(target),
        "Target exited, changed window, or left this workspace; discover it again"
    );
    Ok(())
}
pub fn validate_message(text: &str) -> Result<()> {
    ensure!(
        !text.trim().is_empty() && text.len() <= 8192,
        "Live message must contain 1–8192 bytes"
    );
    ensure!(
        !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "Live messages cannot contain terminal control characters"
    );
    Ok(())
}
pub fn discover(workspace: &Path) -> Result<Vec<LiveTarget>> {
    #[cfg(target_os = "linux")]
    {
        discover_in(Path::new("/proc"), workspace)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = workspace;
        Ok(vec![])
    }
}
fn resolve(workspace: &Path, id: &str) -> Result<LiveTarget> {
    #[cfg(target_os = "linux")]
    {
        resolve_in(Path::new("/proc"), workspace, id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        discover(workspace)?
            .into_iter()
            .find(|t| t.id == id)
            .context("Live target not found in this workspace")
    }
}

/// Resolve an id, explaining a near miss rather than only reporting absence.
///
/// Discovery compares working directories exactly, so a session opened in a
/// subdirectory of the project belongs to a different workspace and vanishes
/// from the list without a word. Naming the directory it actually runs in is
/// the difference between a one-line fix and a hunt.
pub fn resolve_in(proc: &Path, workspace: &Path, id: &str) -> Result<LiveTarget> {
    if let Some(t) = discover_in(proc, workspace)?
        .into_iter()
        .find(|t| t.id == id)
    {
        return Ok(t);
    }
    anyhow::bail!("{}", miss(proc, workspace, id))
}

/// Describe why an id did not resolve, using only what the process still shows.
fn miss(proc: &Path, workspace: &Path, id: &str) -> String {
    const GENERIC: &str = "Live target not found in this workspace";
    let mut parts = id.split(':');
    let (Some(_), Some(pid), Some(start)) = (parts.next(), parts.next(), parts.next()) else {
        return format!("{GENERIC}; ids look like <transport>:<pid>:<start_time>");
    };
    let Ok(pid) = pid.parse::<u32>() else {
        return GENERIC.into();
    };
    let cwd = match std::fs::read_link(proc.join(pid.to_string()).join("cwd")) {
        Ok(cwd) => cwd,
        Err(_) => {
            return format!("{GENERIC}; process {pid} is gone, so rediscover the target");
        }
    };
    if cwd != workspace {
        return format!(
            "{GENERIC}; process {pid} runs in {} and discovery matches the working directory exactly. Run Soudan with --workspace {} to reach it, or reopen that chat in {}.",
            cwd.display(),
            cwd.display(),
            workspace.display()
        );
    }
    match crate::claude::process_start(proc, pid) {
        Ok(actual) if start.parse::<u64>().is_ok_and(|s| s != actual) => {
            format!(
                "{GENERIC}; process {pid} was replaced since this id was issued, so rediscover the target"
            )
        }
        _ => format!(
            "{GENERIC}; process {pid} is in this workspace but is not a supported agent terminal"
        ),
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Read {
        target: String,
    },
    Send {
        target: String,
        text: String,
        request_id: String,
    },
    Ping,
    Stop,
}

#[cfg(unix)]
async fn request(workspace: &Path, request: &Request) -> Result<Value> {
    let work = async {
        let mut stream = tokio::net::UnixStream::connect(workspace.join(".soudan/kitty.sock"))
            .await
            .context("Kitty bridge is offline; run soudan live setup --via-pid PID")?;
        stream
            .write_all(format!("{}\n", serde_json::to_string(request)?).as_bytes())
            .await?;
        let mut data = String::new();
        BufReader::new(stream)
            .take(262145)
            .read_to_string(&mut data)
            .await?;
        ensure!(data.len() <= 262144, "Bridge response too large");
        let value: Value = serde_json::from_str(&data)?;
        if let Some(error) = value.get("error") {
            anyhow::bail!("{error}");
        }
        Ok(value)
    };
    tokio::time::timeout(Duration::from_secs(20), work)
        .await
        .context("Kitty bridge request timed out; do not blindly retry a send")?
}
/// Resolve a Codex target to its session. Codex is never driven through a terminal.
///
/// Failing here is an error rather than a fallback: keystroke automation cannot tell
/// an empty Codex composer from one holding a draft, so silently downgrading to it
/// would risk submitting someone's unfinished message.
#[cfg(target_os = "linux")]
fn native(workspace: &Path, target: &str) -> Result<Option<(LiveTarget, crate::codex::Session)>> {
    // An unknown id belongs to the bridge, which reports its own reason.
    let Ok(t) = resolve(workspace, target) else {
        return Ok(None);
    };
    if t.agent != "codex" {
        return Ok(None);
    }
    let session = crate::codex::session(Path::new("/proc"), t.pid)?.context(
        "Codex session exposes no readable thread; Soudan will not fall back to its terminal",
    )?;
    Ok(Some((t, session)))
}
#[cfg(not(target_os = "linux"))]
fn native(_: &Path, _: &str) -> Result<Option<(LiveTarget, crate::codex::Session)>> {
    Ok(None)
}

pub async fn read(workspace: &Path, target: &str) -> Result<Value> {
    if target.starts_with("claude:") {
        let t = resolve(workspace, target)?;
        let session = crate::claude::for_target(workspace, &t)?;
        let mut state = crate::claude::state(&session)?;
        state["target"] = serde_json::to_value(t)?;
        return Ok(state);
    }
    if let Some((t, session)) = native(workspace, target)? {
        let mut state = crate::codex::state(&session.rollout)?;
        state["kind"] = "codex_thread".into();
        state["thread"] = session.thread.into();
        state["target"] = serde_json::to_value(&t)?;
        return Ok(state);
    }
    request(
        workspace,
        &Request::Read {
            target: target.into(),
        },
    )
    .await
}
pub async fn send(workspace: &Path, target: &str, text: &str, request_id: &str) -> Result<Value> {
    let mut result = send_inner(workspace, target, text, request_id).await?;
    let receipt = delivery(workspace, request_id)
        .map(|v| v["receipt"].clone())
        .unwrap_or_else(|e| crate::receipt::unknown(&format!("{e:#}")));
    if receipt["status"] == "blocked" {
        result["next"] = receipt["reason"].clone();
    }
    result["receipt"] = receipt;
    Ok(result)
}
async fn send_inner(workspace: &Path, target: &str, text: &str, request_id: &str) -> Result<Value> {
    ensure!(
        std::env::var_os("SOUDAN_CHILD").is_none(),
        "Consultation workers cannot inject messages into live chats"
    );
    validate_message(text)?;
    validate_request_id(request_id)?;
    if let Some(status) = settled(workspace, request_id, target, text)? {
        return Ok(
            json!({"request_id":request_id,"target":target,"status":status,"replayed":true}),
        );
    }
    if target.starts_with("claude:") {
        let t = resolve(workspace, target)?;
        let session = crate::claude::for_target(workspace, &t)?;
        return send_to_claude(workspace, &t, &session, text, request_id).await;
    }
    if let Some((t, session)) = native(workspace, target)? {
        return queue_to_codex(workspace, &t, &session, text, request_id).await;
    }
    request(
        workspace,
        &Request::Send {
            target: target.into(),
            text: text.into(),
            request_id: request_id.into(),
        },
    )
    .await
}

/// The outcome of claiming a request id.
enum Claim {
    /// This caller reserved the id and must attempt the delivery.
    Reserved,
    /// An earlier attempt already recorded this outcome; report it, do not resend.
    Recorded(String),
}

/// The decided outcome of a request id, or `None` if the id is still usable.
///
/// An id is usable when it has never been seen, or when its record proves the
/// message was never handed over. Every other status is final: reporting it is
/// the only correct answer, because a resend could duplicate a live message.
fn recorded(
    db: &rusqlite::Connection,
    request_id: &str,
    target: &str,
    text: &str,
) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let previous: Option<(String, String, String)> = db
        .query_row(
            "SELECT target,text,status FROM live_deliveries WHERE request_id=?1",
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((old_target, old_text, status)) = previous else {
        return Ok(None);
    };
    ensure!(
        old_target == target && old_text == text,
        "request_id was already used with different arguments"
    );
    Ok((status != "not_delivered").then_some(status))
}

/// Answer a settled delivery before its target is touched at all.
///
/// Resolving a target, reading its screen and checking that it is idle each fail
/// for reasons that have nothing to do with this request: the chat has ended, or
/// it is mid-turn. A sender that never saw the first reply still has to be able to
/// learn what became of it, so the stored verdict is returned first and only a
/// reusable id goes on to inspect the target.
pub fn settled(
    workspace: &Path,
    request_id: &str,
    target: &str,
    text: &str,
) -> Result<Option<String>> {
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    delivery_schema(&db)?;
    recorded(&db, request_id, target, text)
}

/// Claim a request id, or report what an earlier attempt recorded for it.
///
/// The lookup and the reservation share one immediate transaction, so two senders
/// racing on the same id cannot both believe they are the first. Without it the
/// loser fails on the primary key instead of replaying the winner's result.
///
/// The transaction is held by value so that every exit which is not a successful
/// commit rolls back, a commit that itself fails included. A caller that keeps its
/// connection open — the bridge does — must never inherit a half-open transaction.
fn claim(
    db: &rusqlite::Connection,
    request_id: &str,
    target: &str,
    text: &str,
    before: &str,
    basis: Option<&str>,
) -> Result<Claim> {
    use rusqlite::{TransactionBehavior, params};
    let tx = rusqlite::Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    if let Some(status) = recorded(&tx, request_id, target, text)? {
        // Nothing was written, so letting this roll back on the way out is right.
        return Ok(Claim::Recorded(status));
    }
    // The row may already exist as a proven non-delivery, which recorded() cleared
    // for reuse; either way the claim resets it to this attempt.
    tx.execute(
        "INSERT INTO live_deliveries(request_id,target,text,status,before_screen,receipt_basis) VALUES(?1,?2,?3,'uncertain',?4,?5) ON CONFLICT(request_id) DO UPDATE SET status='uncertain',error=NULL,before_screen=excluded.before_screen,receipt_basis=excluded.receipt_basis",
        params![request_id, target, text, before, basis],
    )?;
    tx.commit()?;
    Ok(Claim::Reserved)
}

/// Deliver through Codex's queue, recording intent exactly as the terminal path does.
async fn queue_to_codex(
    workspace: &Path,
    target: &LiveTarget,
    session: &crate::codex::Session,
    text: &str,
    request_id: &str,
) -> Result<Value> {
    use rusqlite::params;
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    delivery_schema(&db)?;
    let basis = crate::receipt::Basis::capture(&session.rollout)
        .ok()
        .and_then(|b| serde_json::to_string(&b).ok());
    let before = crate::codex::state(&session.rollout)?.to_string();
    // Commit the intent before handing the message over, so an interrupted sender
    // leaves an uncertain record instead of silently resending later.
    if let Claim::Recorded(status) =
        claim(&db, request_id, &target.id, text, &before, basis.as_deref())?
    {
        return Ok(
            json!({"request_id":request_id,"target":target.id,"status":status,"replayed":true}),
        );
    }
    match crate::codex::queue(&session.thread, &format!("[Soudan {request_id}] {text}")).await {
        Ok(()) => {
            db.execute(
                "UPDATE live_deliveries SET status='queued' WHERE request_id=?1",
                [request_id],
            )?;
            Ok(
                json!({"request_id":request_id,"target":target.id,"thread":session.thread,"status":"queued","transport":"codex_queue","next":"Read the target to verify the reply; queued means Codex accepted the message, not that it answered."}),
            )
        }
        Err(failure) => {
            // A command that never ran delivered nothing, so the id stays retryable.
            let status = match failure {
                crate::codex::Failure::NotAttempted(_) => "not_delivered",
                crate::codex::Failure::Uncertain(_) => "uncertain",
            };
            let error = failure.into_error();
            db.execute(
                "UPDATE live_deliveries SET status=?2,error=?3 WHERE request_id=?1",
                params![request_id, status, error.to_string()],
            )?;
            Err(error)
        }
    }
}

async fn send_to_claude(
    workspace: &Path,
    target: &LiveTarget,
    session: &crate::claude::Session,
    text: &str,
    request_id: &str,
) -> Result<Value> {
    use rusqlite::params;
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    delivery_schema(&db)?;
    let basis = crate::receipt::Basis::capture(&session.transcript)
        .ok()
        .and_then(|b| serde_json::to_string(&b).ok());
    let before = json!({"session":session.id,"status":session.status}).to_string();
    if let Claim::Recorded(status) =
        claim(&db, request_id, &target.id, text, &before, basis.as_deref())?
    {
        return Ok(
            json!({"request_id":request_id,"target":target.id,"status":status,"replayed":true}),
        );
    }
    let result = async {
        let current = crate::claude::for_target(workspace, target)
            .map_err(crate::codex::Failure::NotAttempted)?;
        if current.id != session.id || current.socket != session.socket {
            return Err(crate::codex::Failure::NotAttempted(anyhow::anyhow!(
                "Claude session changed before delivery"
            )));
        }
        crate::claude::send(&current, text, request_id).await
    }
    .await;
    match result {
        Ok(()) => {
            db.execute(
                "UPDATE live_deliveries SET status='submitted' WHERE request_id=?1",
                [request_id],
            )?;
            Ok(
                json!({"request_id":request_id,"target":target.id,"session":session.id,
                "status":"submitted","transport":"claude_socket",
                "next":"Written to Claude's inbox, not an acknowledgement. Its inbound policy may hold or refuse the message. Read the session to verify the reply."}),
            )
        }
        Err(failure) => {
            let status = match failure {
                crate::codex::Failure::NotAttempted(_) => "not_delivered",
                _ => "uncertain",
            };
            let error = failure.into_error();
            db.execute(
                "UPDATE live_deliveries SET status=?2,error=?3 WHERE request_id=?1",
                params![request_id, status, error.to_string()],
            )?;
            Err(error)
        }
    }
}

async fn kitty(args: &[&str], input: Option<&str>) -> Result<String> {
    use std::process::Stdio;
    let mut child = tokio::process::Command::new("kitty")
        .arg("@")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().context("Missing stdin")?;
    if let Some(input) = input {
        stdin.write_all(input.as_bytes()).await?;
    }
    drop(stdin);
    let result = tokio::time::timeout(Duration::from_secs(8), child.wait_with_output())
        .await
        .context("Kitty command timed out")??;
    ensure!(
        result.status.success(),
        "Kitty: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    ensure!(
        result.stdout.len() <= 131072,
        "Terminal snapshot exceeds 128 KiB"
    );
    Ok(String::from_utf8(result.stdout)?)
}

#[cfg(unix)]
pub async fn bridge(workspace: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        std::env::var("KITTY_LISTEN_ON").is_ok(),
        "Bridge must be launched by Kitty with --allow-remote-control"
    );
    let path = workspace.join(".soudan/kitty.sock");
    if path.exists() {
        ensure!(
            tokio::net::UnixStream::connect(&path).await.is_err(),
            "Bridge already running"
        );
        std::fs::remove_file(&path)?;
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    delivery_schema(&db)?;
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut stop = false;
        let result = async {
            let mut line = String::new();
            tokio::time::timeout(
                Duration::from_secs(3),
                BufReader::new(&mut stream).take(16385).read_line(&mut line),
            )
            .await??;
            ensure!(line.len() <= 16384, "Bridge request too large");
            let req: Request = serde_json::from_str(&line)?;
            stop = matches!(&req, Request::Stop);
            handle(workspace, &db, req).await
        }
        .await;
        let value = match result {
            Ok(value) => value,
            Err(error) => json!({"error":format!("{error:#}")}),
        };
        let response = format!("{value}\n");
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            stream.write_all(response.as_bytes()),
        )
        .await;
        let _ = stream.shutdown().await;
        if stop {
            break;
        }
    }
    std::fs::remove_file(&path)?;
    Ok(())
}
async fn handle(workspace: &Path, db: &rusqlite::Connection, req: Request) -> Result<Value> {
    use rusqlite::params;
    match req {
        Request::Ping => Ok(json!({"status":"ready"})),
        Request::Stop => Ok(json!({"status":"stopped"})),
        Request::Read { target } => {
            let t = resolve(workspace, &target)?;
            validate_terminal(workspace, &t)?;
            let screen = kitty(&["get-text", "--match", &window_match(&t)?], None).await?;
            Ok(json!({"target":t,"screen":screen,"kind":"terminal_snapshot"}))
        }
        Request::Send {
            target,
            text,
            request_id,
        } => {
            validate_message(&text)?;
            validate_request_id(&request_id)?;
            // A settled delivery is reported before the target is touched; a direct
            // bridge request gets the same answer as one routed through send().
            if let Some(status) = recorded(db, &request_id, &target, &text)? {
                return Ok(
                    json!({"request_id":request_id,"target":target,"status":status,"replayed":true}),
                );
            }
            let t = resolve(workspace, &target)?;
            let match_id = window_match(&t)?;
            validate_terminal(workspace, &t)?;
            let before = kitty(&["get-text", "--match", &match_id], None).await?;
            ensure_ready(&t.agent, &before)?;
            if let Claim::Recorded(status) = claim(db, &request_id, &target, &text, &before, None)?
            {
                return Ok(
                    json!({"request_id":request_id,"target":target,"status":status,"replayed":true}),
                );
            }
            // Commit the intent before writing any keystrokes. A crashed sender
            // must never replay an uncertain delivery automatically.
            let attempt = async {
                validate_terminal(workspace, &t)?;
                kitty(
                    &[
                        "send-text",
                        "--match",
                        &match_id,
                        "--bracketed-paste",
                        "enable",
                        "--stdin",
                    ],
                    Some(&format!("[Soudan {request_id}] {text}")),
                )
                .await?;
                tokio::time::sleep(Duration::from_millis(150)).await;
                validate_terminal(workspace, &t)?;
                kitty(&["send-key", "--match", &match_id, "Return"], None).await?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            match attempt {
                Ok(()) => {
                    db.execute(
                        "UPDATE live_deliveries SET status='submitted' WHERE request_id=?1",
                        [&request_id],
                    )?;
                    Ok(
                        json!({"request_id":request_id,"target":target,"status":"submitted","next":"Read the target terminal to verify receipt and its response; submitted is not an acknowledgement."}),
                    )
                }
                Err(error) => {
                    db.execute(
                        "UPDATE live_deliveries SET error=?2 WHERE request_id=?1",
                        params![request_id, error.to_string()],
                    )?;
                    Err(error)
                }
            }
        }
    }
}

/// Install a Kitty shortcut that grants a dedicated bridge connection.
/// Kitty's global remote-control flag cannot be enabled by config reload.
#[cfg(target_os = "linux")]
pub async fn setup(workspace: &Path, via_pid: u32, shortcut: Option<&str>) -> Result<Value> {
    if let Ok(value) = request(workspace, &Request::Ping).await {
        return Ok(value);
    }
    inspect(Path::new("/proc"), workspace, via_pid)?
        .context("via-pid is not a Kitty agent in this workspace")?;
    let kitty_pid = owning_kitty(via_pid)?;
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join(".config")
            .into()
    }))
    .join("kitty/kitty.conf");
    // An explicit key wins, then the one this workspace already uses, then the default.
    let shortcut = shortcut
        .map(str::to_owned)
        .or_else(|| recorded_shortcut(workspace))
        .unwrap_or_else(|| DEFAULT_SHORTCUT.to_owned());
    configure_shortcut(
        &config,
        workspace,
        &std::env::current_exe()?,
        kitty_pid,
        &shortcut,
    )?;
    // SAFETY: signal only the positively identified owning Kitty process.
    unsafe {
        libc::kill(kitty_pid as i32, libc::SIGUSR1);
    }
    Ok(
        json!({"status":"awaiting_shortcut","shortcut":shortcut,"instructions":format!("Press {shortcut} once in the existing Kitty window. This starts a restricted Soudan bridge without restarting any chat.")}),
    )
}
#[cfg(target_os = "linux")]
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Fail closed when a terminal is busy or its input box cannot be identified.
pub fn ensure_ready(agent: &str, screen: &str) -> Result<()> {
    let lower = screen.to_lowercase();
    ensure!(
        !lower.contains("esc to interrupt") && !lower.contains("esc to cancel"),
        "Target is busy; wait for the current turn to finish"
    );
    let lines: Vec<_> = screen.lines().collect();
    let (index, prompt) = lines
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, line)| {
            let line = line.trim_start();
            line.strip_prefix('❯')
                .or_else(|| line.strip_prefix('›'))
                .or_else(|| line.strip_prefix("> "))
                .map(|text| (index, text))
        })
        .context(
            "Cannot identify an idle input prompt; inspect the target screen before sending",
        )?;
    ensure!(
        prompt.trim().is_empty(),
        "Target has a draft or an unrecognized prompt; refusing to append or submit it ({agent})"
    );
    for line in &lines[index + 1..] {
        let text = line.trim();
        if text.starts_with("──") || text.starts_with("? for shortcuts") {
            break;
        }
        ensure!(
            text.is_empty(),
            "Target has a multiline draft or an unrecognized input area"
        );
    }
    Ok(())
}
/// Kitty addresses a window, so a target without one cannot be driven this way.
///
/// Codex is refused here and not only in native(), which hands an unresolvable id
/// to the bridge. Discovery can succeed in the bridge while failing in the process
/// that asked it, and a Codex chat must never be driven by keystrokes: its composer
/// cannot be told apart from one holding somebody's unsent draft.
pub fn window_match(target: &LiveTarget) -> Result<String> {
    ensure!(
        target.agent == "cursor",
        "Only Cursor uses terminal delivery; Claude Code and Codex use native inboxes"
    );
    let window = target
        .window_id
        .context("Target has no Kitty window; it cannot be reached through the terminal")?;
    Ok(format!("id:{window}"))
}

#[cfg(target_os = "linux")]
fn validate_terminal(workspace: &Path, t: &LiveTarget) -> Result<()> {
    validate_target(Path::new("/proc"), workspace, t)?;
    ensure!(
        owning_kitty(t.pid)? == owning_kitty(std::process::id())?,
        "Target belongs to another Kitty instance"
    );
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", t.pid))?;
    let tail = stat.rsplit_once(')').context("Invalid process stat")?.1;
    let fields: Vec<_> = tail.split_whitespace().collect();
    let group: i32 = fields.get(2).context("Missing process group")?.parse()?;
    let foreground: i32 = fields
        .get(5)
        .context("Missing terminal foreground group")?
        .parse()?;
    ensure!(
        foreground > 0 && foreground == group,
        "Target agent is not the foreground terminal process"
    );
    Ok(())
}
#[cfg(target_os = "linux")]
fn owning_kitty(mut pid: u32) -> Result<u32> {
    for _ in 0..64 {
        let path = PathBuf::from(format!("/proc/{pid}"));
        if std::fs::read_link(path.join("exe"))?
            .file_name()
            .is_some_and(|n| n == "kitty")
        {
            return Ok(pid);
        }
        let status = std::fs::read_to_string(path.join("status"))?;
        pid = status
            .lines()
            .find_map(|s| s.strip_prefix("PPid:"))
            .context("Missing parent")?
            .trim()
            .parse()?;
        ensure!(pid > 1, "Process is no longer attached to Kitty");
    }
    anyhow::bail!("Process ancestry exceeds limit")
}
#[cfg(not(target_os = "linux"))]
fn validate_terminal(_: &Path, _: &LiveTarget) -> Result<()> {
    anyhow::bail!("Live terminal discovery currently requires Linux")
}
#[cfg(not(target_os = "linux"))]
pub async fn setup(_: &Path, _: u32, _: Option<&str>) -> Result<Value> {
    anyhow::bail!("Live Kitty setup currently requires Linux")
}

pub fn delivery(workspace: &Path, id: &str) -> Result<Value> {
    delivery_in(workspace, id, Path::new("/proc"))
}

/// Inspect a saved delivery using an injectable process directory.
pub fn delivery_in(workspace: &Path, id: &str, proc: &Path) -> Result<Value> {
    validate_request_id(id)?;
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    delivery_schema(&db)?;
    delivery_row(&db, id, proc)
}

/// Read receipt evidence without schema initialization or migration.
pub fn delivery_readonly(workspace: &Path, id: &str) -> Result<Value> {
    delivery_readonly_in(workspace, id, Path::new("/proc"))
}

/// Read-only receipt observation with a fixture process directory.
pub fn delivery_readonly_in(workspace: &Path, id: &str, proc: &Path) -> Result<Value> {
    validate_request_id(id)?;
    let db = crate::wait::open_readonly(workspace)?;
    delivery_row(&db, id, proc)
}

fn delivery_row(db: &rusqlite::Connection, id: &str, proc: &Path) -> Result<Value> {
    let has_basis = {
        let mut q = db.prepare("PRAGMA table_info(live_deliveries)")?;
        q.query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|name| name == "receipt_basis")
    };
    let sql = if has_basis {
        "SELECT target,text,status,error,receipt_basis FROM live_deliveries WHERE request_id=?1"
    } else {
        "SELECT target,text,status,error,NULL FROM live_deliveries WHERE request_id=?1"
    };
    let (mut row, basis) = db.query_row(sql, [id], |r| Ok((json!({"request_id":id,"target":r.get::<_,String>(0)?,"text":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"error":r.get::<_,Option<String>>(3)?}), r.get::<_,Option<String>>(4)?))).context("Live delivery not found")?;
    let basis = basis.and_then(|s| serde_json::from_str::<crate::receipt::Basis>(&s).ok());
    row["receipt"] = crate::receipt::observe(
        basis.as_ref(),
        proc,
        row["target"].as_str().unwrap_or(""),
        id,
    );
    Ok(row)
}
#[cfg(unix)]
pub async fn disconnect(workspace: &Path) -> Result<Value> {
    let socket = workspace.join(".soudan/kitty.sock");
    if socket.exists() {
        match tokio::net::UnixStream::connect(&socket).await {
            Ok(probe) => {
                drop(probe);
                request(workspace, &Request::Stop).await?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                if socket.exists() {
                    std::fs::remove_file(&socket)?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    let record = workspace.join(".soudan/kitty-shortcut.json");
    if record.exists() {
        let value: Value = serde_json::from_slice(&std::fs::read(&record)?)?;
        let config = PathBuf::from(
            value["config"]
                .as_str()
                .context("Invalid shortcut record")?,
        );
        let addition = value["addition"]
            .as_str()
            .context("Invalid shortcut record")?;
        let current = std::fs::read_to_string(&config)?;
        ensure!(
            current.matches(addition).count() == 1,
            "Shortcut config was edited; remove the Soudan mapping manually"
        );
        std::fs::write(&config, current.replacen(addition, "", 1))?;
        #[cfg(target_os = "linux")]
        if let Some(pid) = value["kitty_pid"].as_u64()
            && std::fs::read_link(format!("/proc/{pid}/exe"))
                .is_ok_and(|p| p.file_name().is_some_and(|n| n == "kitty"))
        {
            // SAFETY: reload only the recorded and revalidated Kitty process.
            unsafe {
                libc::kill(pid as i32, libc::SIGUSR1);
            }
        }
        std::fs::remove_file(record)?;
    }
    Ok(json!({"status":"disconnected","history_retained":true}))
}

#[cfg(target_os = "linux")]
/// The shortcut Kitty maps when the caller expresses no preference.
pub const DEFAULT_SHORTCUT: &str = "ctrl+shift+f12";

/// Reject anything that could carry a second directive into kitty.conf.
///
/// The key is written verbatim into a `map <key> launch ...` line, so a value
/// holding whitespace or a newline would append configuration of its own.
pub fn validate_shortcut(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() <= 64
            && key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+_-".contains(&b))
            && !key.starts_with('+')
            && !key.ends_with('+'),
        "Kitty shortcut must be lowercase key names joined by '+', such as ctrl+shift+backslash"
    );
    Ok(())
}

/// The shortcut this workspace already installed, if it has one.
///
/// Re-running setup must not silently move a workspace back to the default: a
/// hand-picked key is the whole reason the option exists, and the recorded
/// `addition` is what `disconnect` matches to remove the block cleanly.
pub fn recorded_shortcut(workspace: &Path) -> Option<String> {
    let record = std::fs::read(workspace.join(".soudan/kitty-shortcut.json")).ok()?;
    let value: Value = serde_json::from_slice(&record).ok()?;
    if let Some(shortcut) = value["shortcut"].as_str() {
        return Some(shortcut.to_owned());
    }
    // Records written before the key was stored — including the hand-edited ones
    // this option exists to replace — carry only the block. Reading the key back
    // out of it carries a chosen shortcut through the upgrade, instead of quietly
    // resetting the one installation that already needed a different key.
    value["addition"]
        .as_str()?
        .lines()
        .find_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some("map"))
                .then(|| words.next())
                .flatten()
        })
        .filter(|key| validate_shortcut(key).is_ok())
        .map(str::to_owned)
}

pub fn configure_shortcut(
    config: &Path,
    workspace: &Path,
    executable: &Path,
    kitty_pid: u32,
    shortcut: &str,
) -> Result<()> {
    validate_shortcut(shortcut)?;
    let mut original =
        std::fs::read_to_string(config).context("Cannot read Kitty configuration")?;
    let record = workspace.join(".soudan/kitty-shortcut.json");
    if record.exists() {
        let value: Value = serde_json::from_slice(&std::fs::read(&record)?)?;
        ensure!(
            value["config"].as_str() == config.to_str(),
            "Shortcut record belongs to another config file"
        );
        let previous = value["addition"]
            .as_str()
            .context("Invalid shortcut record")?;
        ensure!(
            original.matches(previous).count() == 1,
            "Soudan shortcut was edited; update it manually"
        );
        original = original.replacen(previous, "", 1);
    }
    // One kitty.conf serves every workspace, so the key is the contended resource.
    ensure!(
        !original.lines().any(|line| {
            let mut words = line.split_whitespace();
            words.next() == Some("map") && words.next() == Some(shortcut)
        }),
        "{shortcut} is already mapped in Kitty, possibly by another workspace's bridge; pass --shortcut with a free key"
    );
    let command = format!(
        "launch --type=background --allow-remote-control --remote-control-password='!' --remote-control-password='\"\" get-text send-text send-key' --cwd {} {} --workspace {} live bridge",
        quote(&workspace.to_string_lossy()),
        quote(&executable.to_string_lossy()),
        quote(&workspace.to_string_lossy())
    );
    let addition = format!(
        "\n# Soudan bridge: {}\nmap {shortcut} {command}\n",
        workspace.display()
    );
    std::fs::write(config, format!("{original}{addition}"))?;
    std::fs::write(
        record,
        serde_json::to_vec(
            &json!({"config":config,"addition":addition,"kitty_pid":kitty_pid,"shortcut":shortcut}),
        )?,
    )?;
    Ok(())
}

pub fn validate_request_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b)),
        "Live request_id must contain 1–128 ASCII letters, digits, hyphens, underscores, periods, or colons"
    );
    Ok(())
}

#[cfg(test)]
mod receipt_claim_tests {
    use super::*;
    #[test]
    fn claim_commits_evidence_and_only_replaces_it_for_proven_non_delivery() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        delivery_schema(&db).unwrap();
        assert!(matches!(
            claim(&db, "id", "codex:42:123", "hello", "aborted", Some("first")).unwrap(),
            Claim::Reserved
        ));
        assert!(db.is_autocommit());
        assert!(matches!(
            claim(&db, "id", "codex:42:123", "hello", "idle", Some("second")).unwrap(),
            Claim::Recorded(_)
        ));
        assert!(db.is_autocommit());
        let basis = || {
            db.query_row("SELECT receipt_basis FROM live_deliveries", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
        };
        assert_eq!(basis(), "first");
        db.execute("UPDATE live_deliveries SET status='not_delivered'", [])
            .unwrap();
        assert!(matches!(
            claim(&db, "id", "codex:42:123", "hello", "idle", Some("second")).unwrap(),
            Claim::Reserved
        ));
        assert_eq!(basis(), "second");
        assert!(claim(&db, "id", "codex:42:123", "different", "", None).is_err());
        assert!(db.is_autocommit());
    }
}
