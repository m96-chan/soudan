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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LiveTarget {
    pub id: String,
    pub agent: String,
    pub pid: u32,
    pub start_time: u64,
    pub window_id: u64,
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
    let Some(window) = window else {
        return Ok(None);
    };
    let window_id = std::str::from_utf8(window)?.parse()?;
    let stat = std::fs::read_to_string(path.join("stat"))?;
    let (_, tail) = stat.rsplit_once(')').context("Malformed process stat")?;
    let start_time = tail
        .split_whitespace()
        .nth(19)
        .context("Missing start time")?
        .parse()?;
    Ok(Some(LiveTarget {
        id: format!("kitty:{pid}:{start_time}"),
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
    discover(workspace)?
        .into_iter()
        .find(|t| t.id == id)
        .context("Live target not found in this workspace")
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
/// Codex owns a session API, so terminal automation is a fallback for it, not the path.
/// Choosing the transport up front keeps a failed native send from being retried as keystrokes.
#[cfg(target_os = "linux")]
fn native(workspace: &Path, target: &str) -> Option<(LiveTarget, crate::codex::Session)> {
    let t = resolve(workspace, target).ok()?;
    if t.agent != "codex" {
        return None;
    }
    let session = crate::codex::session(Path::new("/proc"), t.pid).ok()??;
    Some((t, session))
}
#[cfg(not(target_os = "linux"))]
fn native(_: &Path, _: &str) -> Option<(LiveTarget, crate::codex::Session)> {
    None
}

pub async fn read(workspace: &Path, target: &str) -> Result<Value> {
    if let Some((t, session)) = native(workspace, target) {
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
    ensure!(
        std::env::var_os("SOUDAN_CHILD").is_none(),
        "Consultation workers cannot inject messages into live chats"
    );
    validate_message(text)?;
    validate_request_id(request_id)?;
    if let Some((t, session)) = native(workspace, target) {
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

/// Deliver through Codex's queue, recording intent exactly as the terminal path does.
async fn queue_to_codex(
    workspace: &Path,
    target: &LiveTarget,
    session: &crate::codex::Session,
    text: &str,
    request_id: &str,
) -> Result<Value> {
    use rusqlite::{OptionalExtension, params};
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch(DELIVERIES)?;
    let previous: Option<(String, String, String)> = db
        .query_row(
            "SELECT target,text,status FROM live_deliveries WHERE request_id=?1",
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((old_target, old_text, status)) = previous {
        ensure!(
            old_target == target.id && old_text == text,
            "request_id was already used with different arguments"
        );
        return Ok(
            json!({"request_id":request_id,"target":target.id,"status":status,"replayed":true}),
        );
    }
    let before = crate::codex::state(&session.rollout)?.to_string();
    db.execute("INSERT INTO live_deliveries(request_id,target,text,status,before_screen) VALUES(?1,?2,?3,'uncertain',?4)",params![request_id,target.id,text,before])?;
    // Commit the intent before handing the message over, so an interrupted sender
    // leaves an uncertain record instead of silently resending later.
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
        Err(error) => {
            db.execute(
                "UPDATE live_deliveries SET error=?2 WHERE request_id=?1",
                params![request_id, error.to_string()],
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
    db.execute_batch(DELIVERIES)?;
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
    use rusqlite::{OptionalExtension, params};
    match req {
        Request::Ping => Ok(json!({"status":"ready"})),
        Request::Stop => Ok(json!({"status":"stopped"})),
        Request::Read { target } => {
            let t = resolve(workspace, &target)?;
            validate_terminal(workspace, &t)?;
            let screen = kitty(
                &["get-text", "--match", &format!("id:{}", t.window_id)],
                None,
            )
            .await?;
            Ok(json!({"target":t,"screen":screen,"kind":"terminal_snapshot"}))
        }
        Request::Send {
            target,
            text,
            request_id,
        } => {
            validate_message(&text)?;
            validate_request_id(&request_id)?;
            let previous: Option<(String, String, String)> = db
                .query_row(
                    "SELECT target,text,status FROM live_deliveries WHERE request_id=?1",
                    [&request_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if let Some((old_target, old_text, status)) = previous {
                ensure!(
                    old_target == target && old_text == text,
                    "request_id was already used with different arguments"
                );
                return Ok(
                    json!({"request_id":request_id,"target":target,"status":status,"replayed":true}),
                );
            }
            let t = resolve(workspace, &target)?;
            let match_id = format!("id:{}", t.window_id);
            validate_terminal(workspace, &t)?;
            let before = kitty(&["get-text", "--match", &match_id], None).await?;
            ensure_ready(&t.agent, &before)?;
            db.execute("INSERT INTO live_deliveries(request_id,target,text,status,before_screen) VALUES(?1,?2,?3,'uncertain',?4)",params![request_id,target,text,before])?;
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
pub async fn setup(workspace: &Path, via_pid: u32) -> Result<Value> {
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
    configure_shortcut(&config, workspace, &std::env::current_exe()?, kitty_pid)?;
    // SAFETY: signal only the positively identified owning Kitty process.
    unsafe {
        libc::kill(kitty_pid as i32, libc::SIGUSR1);
    }
    Ok(
        json!({"status":"awaiting_shortcut","shortcut":"ctrl+shift+f12","instructions":"Press Ctrl+Shift+F12 once in the existing Kitty window. This starts a restricted Soudan bridge without restarting any chat."}),
    )
}
#[cfg(target_os = "linux")]
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Codex animates braille particles across its input area. They are decoration, not input.
fn undecorate(agent: &str, line: &str) -> String {
    match agent {
        "codex" => line
            .chars()
            .filter(|c| !('\u{2800}'..='\u{28ff}').contains(c))
            .collect(),
        _ => line.to_string(),
    }
}

/// Hint text an agent draws in an empty composer. It is not a draft.
fn is_placeholder(agent: &str, prompt: &str) -> bool {
    matches!(
        (agent, prompt.trim()),
        ("codex", "Ask Codex to do anything")
    )
}

/// The status footer below Codex's composer ends its input area.
fn ends_input_area(agent: &str, text: &str) -> bool {
    text.starts_with("──")
        || text.starts_with("? for shortcuts")
        || (agent == "codex" && text.contains(" · "))
}

/// Fail closed when a terminal is busy or its input box cannot be identified.
pub fn ensure_ready(agent: &str, screen: &str) -> Result<()> {
    let lower = screen.to_lowercase();
    ensure!(
        !lower.contains("esc to interrupt") && !lower.contains("esc to cancel"),
        "Target is busy; wait for the current turn to finish"
    );
    let lines: Vec<_> = screen.lines().map(|line| undecorate(agent, line)).collect();
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
        prompt.trim().is_empty() || is_placeholder(agent, prompt),
        "Target has a draft or an unrecognized prompt; refusing to append or submit it ({agent})"
    );
    for line in &lines[index + 1..] {
        let text = line.trim();
        if ends_input_area(agent, text) {
            break;
        }
        ensure!(
            text.is_empty(),
            "Target has a multiline draft or an unrecognized input area"
        );
    }
    Ok(())
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
pub async fn setup(_: &Path, _: u32) -> Result<Value> {
    anyhow::bail!("Live Kitty setup currently requires Linux")
}

pub fn delivery(workspace: &Path, id: &str) -> Result<Value> {
    let db = rusqlite::Connection::open_with_flags(
        workspace.join(".soudan/state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.query_row("SELECT target,text,status,error FROM live_deliveries WHERE request_id=?1",[id],|r|Ok(json!({"request_id":id,"target":r.get::<_,String>(0)?,"text":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"error":r.get::<_,Option<String>>(3)?}))).context("Live delivery not found")
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
pub fn configure_shortcut(
    config: &Path,
    workspace: &Path,
    executable: &Path,
    kitty_pid: u32,
) -> Result<()> {
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
    ensure!(
        !original.lines().any(|line| {
            let mut words = line.split_whitespace();
            words.next() == Some("map") && words.next() == Some("ctrl+shift+f12")
        }),
        "ctrl+shift+f12 is already mapped in Kitty; choose another shortcut manually"
    );
    let command = format!(
        "launch --type=background --allow-remote-control --remote-control-password='!' --remote-control-password='\"\" get-text send-text send-key' --cwd {} {} --workspace {} live bridge",
        quote(&workspace.to_string_lossy()),
        quote(&executable.to_string_lossy()),
        quote(&workspace.to_string_lossy())
    );
    let addition = format!(
        "\n# Soudan bridge: {}\nmap ctrl+shift+f12 {command}\n",
        workspace.display()
    );
    std::fs::write(config, format!("{original}{addition}"))?;
    std::fs::write(
        record,
        serde_json::to_vec(&json!({"config":config,"addition":addition,"kitty_pid":kitty_pid}))?,
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
