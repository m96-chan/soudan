//! Delivery to existing agent chats through each agent's own session API.
//!
//! Every supported agent is reached natively: Codex through its queue, Claude
//! Code through its registered inbox socket. Nothing here drives a terminal, so
//! no keystrokes are synthesised and no chat has to be idle to receive a message.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

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
    let has_sender: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('live_deliveries') WHERE name='sender')",
        [],
        |r| r.get(0),
    )?;
    if !has_sender {
        tx.execute_batch("ALTER TABLE live_deliveries ADD COLUMN sender TEXT")?;
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
    } else if exe.ends_with("/codex") {
        "codex"
    } else if exe.contains("/.grok/") || exe.ends_with("/grok") {
        "grok"
    } else {
        return Ok(None);
    };
    let tty = std::fs::read_link(path.join("fd/0"))?;
    if !tty.starts_with("/dev/pts") {
        return Ok(None);
    }
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
        "grok" => "grok",
        _ => "claude",
    };
    Ok(Some(LiveTarget {
        id: format!("{transport}:{pid}:{start_time}"),
        agent: agent.into(),
        pid,
        start_time,
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
    if target.starts_with("grok:") {
        let t = resolve(workspace, target)?;
        let session = grok_session(workspace, &t)?;
        let mut state = crate::grok::state(&session)?;
        state["target"] = serde_json::to_value(t)?;
        return Ok(state);
    }
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
    unsupported(target)
}

/// Every id names its transport, so one we do not serve cannot be delivered.
///
/// Cursor was reached by typing into its terminal through a Kitty bridge. That
/// path is gone: it needed a keypress to arm, could not tell an empty composer
/// from someone's unsent draft, and served one agent. Cursor comes back when it
/// exposes a session API of its own.
fn unsupported(target: &str) -> Result<Value> {
    let transport = target.split(':').next().unwrap_or(target);
    anyhow::bail!(
        "No live transport for {transport:?} targets; Soudan delivers to Claude Code and Codex through their own session APIs"
    )
}
pub async fn send(workspace: &Path, target: &str, text: &str, request_id: &str) -> Result<Value> {
    send_as(workspace, target, text, request_id, None).await
}

pub fn prompt(request_id: &str, text: &str, sender: Option<&str>) -> Result<String> {
    validate_message(text)?;
    validate_request_id(request_id)?;
    if let Some(sender) = sender {
        validate_request_id(sender)?;
    }
    Ok(format!(
        "[Soudan {request_id}] Declared sender: {} (unverified)\n{text}",
        sender.unwrap_or("unspecified")
    ))
}

pub async fn send_as(
    workspace: &Path,
    target: &str,
    text: &str,
    request_id: &str,
    sender: Option<&str>,
) -> Result<Value> {
    let mut result = send_inner(workspace, target, text, request_id, sender).await?;
    let receipt = delivery(workspace, request_id)
        .map(|v| v["receipt"].clone())
        .unwrap_or_else(|e| crate::receipt::unknown(&format!("{e:#}")));
    if receipt["status"] == "blocked" {
        result["next"] = receipt["reason"].clone();
    }
    result["receipt"] = receipt;
    Ok(result)
}
async fn send_inner(
    workspace: &Path,
    target: &str,
    text: &str,
    request_id: &str,
    sender: Option<&str>,
) -> Result<Value> {
    ensure!(
        std::env::var_os("SOUDAN_CHILD").is_none(),
        "Consultation workers cannot inject messages into live chats"
    );
    prompt(request_id, text, sender)?;
    if let Some(status) = settled_as(workspace, request_id, target, text, sender)? {
        return Ok(
            json!({"request_id":request_id,"target":target,"status":status,"replayed":true}),
        );
    }
    if target.starts_with("grok:") {
        let t = resolve(workspace, target)?;
        let session = grok_session(workspace, &t)?;
        return send_to_grok(workspace, &t, &session, text, request_id, sender).await;
    }
    if target.starts_with("claude:") {
        let t = resolve(workspace, target)?;
        let session = crate::claude::for_target(workspace, &t)?;
        return send_to_claude(workspace, &t, &session, text, request_id, sender).await;
    }
    if let Some((t, session)) = native(workspace, target)? {
        return queue_to_codex(workspace, &t, &session, text, request_id, sender).await;
    }
    unsupported(target)
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
    sender: Option<&str>,
) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let previous: Option<(String, String, String, Option<String>)> = db
        .query_row(
            "SELECT target,text,status,sender FROM live_deliveries WHERE request_id=?1",
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((old_target, old_text, status, old_sender)) = previous else {
        return Ok(None);
    };
    ensure!(
        old_target == target && old_text == text && old_sender.as_deref() == sender,
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
    settled_as(workspace, request_id, target, text, None)
}
fn settled_as(
    workspace: &Path,
    request_id: &str,
    target: &str,
    text: &str,
    sender: Option<&str>,
) -> Result<Option<String>> {
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    delivery_schema(&db)?;
    recorded(&db, request_id, target, text, sender)
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
    provenance: (&str, Option<&str>),
    basis: Option<&str>,
) -> Result<Claim> {
    let (before, sender) = provenance;
    use rusqlite::{TransactionBehavior, params};
    let tx = rusqlite::Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    if let Some(status) = recorded(&tx, request_id, target, text, sender)? {
        // Nothing was written, so letting this roll back on the way out is right.
        return Ok(Claim::Recorded(status));
    }
    // The row may already exist as a proven non-delivery, which recorded() cleared
    // for reuse; either way the claim resets it to this attempt.
    tx.execute(
        "INSERT INTO live_deliveries(request_id,target,text,status,before_screen,receipt_basis,sender) VALUES(?1,?2,?3,'uncertain',?4,?5,?6) ON CONFLICT(request_id) DO UPDATE SET status='uncertain',error=NULL,before_screen=excluded.before_screen,receipt_basis=excluded.receipt_basis",
        params![request_id, target, text, before, basis, sender],
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
    sender: Option<&str>,
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
    if let Claim::Recorded(status) = claim(
        &db,
        request_id,
        &target.id,
        text,
        (&before, sender),
        basis.as_deref(),
    )? {
        return Ok(
            json!({"request_id":request_id,"target":target.id,"status":status,"replayed":true}),
        );
    }
    match crate::codex::queue(&session.thread, &prompt(request_id, text, sender)?).await {
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

/// Resolve a validated target to its Grok session, using that process's own home.
#[cfg(target_os = "linux")]
fn grok_session(workspace: &Path, target: &LiveTarget) -> Result<crate::grok::Session> {
    validate_target(Path::new("/proc"), workspace, target)?;
    let env = std::fs::read(format!("/proc/{}/environ", target.pid))?;
    let home = {
        use std::os::unix::ffi::OsStrExt;
        env.split(|b| *b == 0)
            .find_map(|e| e.strip_prefix(b"HOME=".as_slice()))
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(std::ffi::OsStr::from_bytes(v)).join(".grok"))
            .context("Cannot locate the target's Grok home")?
    };
    crate::grok::session(&home, workspace, target.pid)?
        .context("Grok session is not in its registry; rediscover the target")
}
#[cfg(not(target_os = "linux"))]
fn grok_session(_: &Path, _: &LiveTarget) -> Result<crate::grok::Session> {
    anyhow::bail!("Grok discovery currently requires Linux")
}

/// Deliver through Grok's leader, recording intent exactly as the others do.
async fn send_to_grok(
    workspace: &Path,
    target: &LiveTarget,
    session: &crate::grok::Session,
    text: &str,
    request_id: &str,
    sender: Option<&str>,
) -> Result<Value> {
    use rusqlite::params;
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    delivery_schema(&db)?;
    let basis = crate::receipt::Basis::capture(&session.evidence())
        .ok()
        .and_then(|b| serde_json::to_string(&b).ok());
    let before = crate::grok::state(session)?.to_string();
    if let Claim::Recorded(status) = claim(
        &db,
        request_id,
        &target.id,
        text,
        (&before, sender),
        basis.as_deref(),
    )? {
        return Ok(
            json!({"request_id":request_id,"target":target.id,"status":status,"replayed":true}),
        );
    }
    match crate::grok::send_as(session, text, request_id, sender).await {
        Ok(()) => {
            db.execute(
                "UPDATE live_deliveries SET status='submitted' WHERE request_id=?1",
                [request_id],
            )?;
            Ok(
                json!({"request_id":request_id,"target":target.id,"session":session.id,
                "status":"submitted","transport":"grok_leader",
                "next":"Handed to Grok's leader, which drives the turn after this connection closes. This is not an acknowledgement; read the target or its receipt."}),
            )
        }
        Err(failure) => {
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
    sender: Option<&str>,
) -> Result<Value> {
    use rusqlite::params;
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    delivery_schema(&db)?;
    let basis = crate::receipt::Basis::capture(&session.transcript)
        .ok()
        .and_then(|b| serde_json::to_string(&b).ok());
    let before = json!({"session":session.id,"status":session.status}).to_string();
    if let Claim::Recorded(status) = claim(
        &db,
        request_id,
        &target.id,
        text,
        (&before, sender),
        basis.as_deref(),
    )? {
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
        crate::claude::send_as(&current, text, request_id, sender).await
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
    let has_sender: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('live_deliveries') WHERE name='sender')",
        [],
        |r| r.get(0),
    )?;
    row["sender"] = if has_sender {
        db.query_row(
            "SELECT sender FROM live_deliveries WHERE request_id=?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )?
        .into()
    } else {
        Value::Null
    };
    row["sender_verification"] = "unverified_declaration".into();
    row["receipt"] = crate::receipt::observe(
        basis.as_ref(),
        proc,
        row["target"].as_str().unwrap_or(""),
        id,
    );
    Ok(row)
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
            claim(
                &db,
                "id",
                "codex:42:123",
                "hello",
                ("aborted", None),
                Some("first")
            )
            .unwrap(),
            Claim::Reserved
        ));
        assert!(
            claim(
                &db,
                "id",
                "codex:42:123",
                "hello",
                ("", Some("other")),
                None
            )
            .is_err()
        );
        assert!(db.is_autocommit());
        assert!(matches!(
            claim(
                &db,
                "id",
                "codex:42:123",
                "hello",
                ("idle", None),
                Some("second")
            )
            .unwrap(),
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
            claim(
                &db,
                "id",
                "codex:42:123",
                "hello",
                ("idle", None),
                Some("second")
            )
            .unwrap(),
            Claim::Reserved
        ));
        assert_eq!(basis(), "second");
        assert!(claim(&db, "id", "codex:42:123", "different", ("", None), None).is_err());
        assert!(db.is_autocommit());
    }
}
