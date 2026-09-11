//! Explicitly started, loopback-only, read-only dashboard.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

const DEADLINE: Duration = Duration::from_secs(3);
const PAGE: i64 = 100;
#[derive(Serialize, Deserialize)]
pub enum Query {
    Overview { offset: i64 },
    Messages { room: String, before: i64 },
    Receipt { id: String },
}
fn exists(db: &Connection, table: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}
/// Called only in a supervised child; no initialization or write transactions.
pub fn snapshot(workspace: &Path, query: Query) -> Result<Value> {
    let workspace = workspace.canonicalize()?;
    let path = workspace.join(".soudan/state.db");
    match path.canonicalize() {
        Ok(real) => ensure!(
            real.starts_with(&workspace),
            "State database is outside the workspace"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return match query {
                Query::Overview { .. } => Ok(
                    json!({"deliveries":[],"jobs":[],"rooms":[],"delivery_count":0,"job_count":0,"page_size":PAGE}),
                ),
                Query::Messages { .. } => Ok(json!({"messages":[],"has_older":false})),
                Query::Receipt { .. } => anyhow::bail!("Delivery not found"),
            };
        }
        Err(e) => return Err(e.into()),
    }
    if let Query::Receipt { id } = query {
        return Ok(crate::live::delivery_readonly(&workspace, &id)?["receipt"].clone());
    }
    let db = crate::wait::open_readonly(&workspace)?;
    match query {
        Query::Overview { offset } => {
            ensure!(offset >= 0, "Invalid offset");
            let mut deliveries = vec![];
            let mut jobs = vec![];
            let mut rooms = vec![];
            let mut delivery_count = 0i64;
            let mut job_count = 0i64;
            if exists(&db, "live_deliveries")? {
                delivery_count =
                    db.query_row("SELECT count(*) FROM live_deliveries", [], |r| r.get(0))?;
                let mut q=db.prepare("SELECT request_id,target,status FROM live_deliveries ORDER BY rowid DESC LIMIT ?1 OFFSET ?2")?;
                deliveries=q.query_map(params![PAGE,offset],|r|Ok(json!({"request_id":r.get::<_,String>(0)?,"target":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            }
            if exists(&db, "jobs")? {
                job_count = db.query_row("SELECT count(*) FROM jobs", [], |r| r.get(0))?;
                let mut q=db.prepare("SELECT id,room,agent,status,error FROM jobs ORDER BY rowid DESC LIMIT ?1 OFFSET ?2")?;
                jobs=q.query_map(params![PAGE,offset],|r|Ok(json!({"id":r.get::<_,String>(0)?,"room":r.get::<_,String>(1)?,"agent":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"error":r.get::<_,Option<String>>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            }
            if exists(&db, "messages")? {
                let mut q=db.prepare("SELECT room,count(*),max(id) FROM messages GROUP BY room ORDER BY max(id) DESC")?;
                rooms=q.query_map([],|r|Ok(json!({"room":r.get::<_,String>(0)?,"count":r.get::<_,i64>(1)?,"last_id":r.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            }
            Ok(
                json!({"deliveries":deliveries,"jobs":jobs,"rooms":rooms,"delivery_count":delivery_count,"job_count":job_count,"offset":offset,"page_size":PAGE}),
            )
        }
        Query::Messages { room, before } => {
            ensure!(
                !room.trim().is_empty() && room.len() <= 128 && before >= 0,
                "Invalid room cursor"
            );
            if !exists(&db, "messages")? {
                return Ok(json!({"messages":[],"has_older":false}));
            }
            let mut q=db.prepare("SELECT id,sender,text FROM messages WHERE room=?1 AND (?2=0 OR id<?2) ORDER BY id DESC LIMIT ?3")?;
            let mut rows=q.query_map(params![room,before,PAGE+1],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"sender":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let has_older = rows.len() > PAGE as usize;
            rows.truncate(PAGE as usize);
            rows.reverse();
            Ok(json!({"messages":rows,"has_older":has_older}))
        }
        Query::Receipt { .. } => unreachable!(),
    }
}

async fn observe(workspace: &Path, query: &Query, executable: &Path) -> Result<Value> {
    let mut child = tokio::process::Command::new(executable)
        .arg("--workspace")
        .arg(workspace)
        .arg("web-observer")
        .arg(serde_json::to_string(query)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().context("Missing observer output")?;
    let read = async {
        let mut bytes = Vec::new();
        let mut out = stdout.take(64 * 1024 * 1024 + 1);
        let (read, status) = tokio::join!(out.read_to_end(&mut bytes), child.wait());
        read?;
        ensure!(status?.success(), "Observer exited unexpectedly");
        ensure!(
            bytes.len() <= 64 * 1024 * 1024,
            "Snapshot exceeds response limit"
        );
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value.get("observer_error").is_none(),
            "{}",
            value["observer_error"]
        );
        Ok::<_, anyhow::Error>(value)
    };
    match tokio::time::timeout(DEADLINE, read).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_millis(250), child.wait()).await;
            anyhow::bail!("Observation timed out after 3 seconds")
        }
    }
}

fn decode(text: &str) -> Result<String> {
    let mut out = vec![];
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'+' => out.push(b' '),
            b'%' => {
                let a = bytes.next().context("Invalid escape")?;
                let b = bytes.next().context("Invalid escape")?;
                out.push(u8::from_str_radix(std::str::from_utf8(&[a, b])?, 16)?);
            }
            _ => out.push(b),
        }
    }
    Ok(String::from_utf8(out)?)
}
fn query(route: &str) -> Result<Option<Query>> {
    let (path, args) = route.split_once('?').unwrap_or((route, ""));
    if !matches!(path, "/api/overview" | "/api/messages" | "/api/receipt") {
        return Ok(None);
    }
    let mut fields = BTreeMap::new();
    if !args.is_empty() {
        for pair in args.split('&') {
            let (k, v) = pair.split_once('=').context("Expected query key=value")?;
            ensure!(
                fields.insert(decode(k)?, decode(v)?).is_none(),
                "Duplicate query parameter"
            );
        }
    }
    let number = |fields: &mut BTreeMap<String, String>, key: &str| -> Result<i64> {
        let n = fields.remove(key).unwrap_or_else(|| "0".into()).parse()?;
        ensure!(n >= 0, "Invalid cursor");
        Ok(n)
    };
    let q = match path {
        "/api/overview" => Query::Overview {
            offset: number(&mut fields, "offset")?,
        },
        "/api/messages" => {
            let room = fields.remove("room").context("Missing room")?;
            ensure!(!room.trim().is_empty() && room.len() <= 128, "Invalid room");
            Query::Messages {
                room,
                before: number(&mut fields, "before")?,
            }
        }
        _ => {
            let id = fields.remove("id").context("Missing id")?;
            crate::live::validate_request_id(&id)?;
            Query::Receipt { id }
        }
    };
    ensure!(fields.is_empty(), "Unsupported query parameter");
    Ok(Some(q))
}
struct Response {
    code: u16,
    kind: &'static str,
    body: Vec<u8>,
}
impl Response {
    fn json(code: u16, value: Value) -> Self {
        Self {
            code,
            kind: "application/json; charset=utf-8",
            body: value.to_string().into_bytes(),
        }
    }
    fn error(code: u16, message: &str) -> Self {
        Self::json(code, json!({"error":message}))
    }
}
async fn route(
    header: &str,
    port: u16,
    workspace: &Path,
    observers: Arc<Semaphore>,
    executable: &Path,
) -> Response {
    let mut lines = header.split("\r\n");
    let mut request = lines.next().unwrap_or("").split(' ');
    let method = request.next().unwrap_or("");
    let path = request.next().unwrap_or("");
    let version = request.next().unwrap_or("");
    if request.next().is_some() || version != "HTTP/1.1" || !path.starts_with('/') {
        return Response::error(400, "Invalid HTTP/1.1 request");
    }
    if method != "GET" {
        return Response::error(405, "Only GET is supported");
    }
    let authority = format!("127.0.0.1:{port}");
    let mut host = false;
    for line in lines.filter(|l| !l.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Response::error(400, "Invalid header");
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "host" => {
                if host || value != authority {
                    return Response::error(403, "Invalid local Host");
                };
                host = true;
            }
            "origin" => {
                if value != format!("http://{authority}") {
                    return Response::error(403, "Cross-origin requests are forbidden");
                }
            }
            "sec-fetch-site" => {
                if !matches!(value, "same-origin" | "none") {
                    return Response::error(403, "Cross-site requests are forbidden");
                }
            }
            "transfer-encoding" => return Response::error(400, "Request bodies are unsupported"),
            "content-length" if value != "0" => {
                return Response::error(400, "Request bodies are unsupported");
            }
            _ => (),
        }
    }
    if !host {
        return Response::error(400, "Host is required");
    }
    let asset = match path {
        "/" => Some(("text/html; charset=utf-8", include_str!("web/index.html"))),
        "/app.js" => Some((
            "application/javascript; charset=utf-8",
            include_str!("web/app.js"),
        )),
        "/style.css" => Some(("text/css; charset=utf-8", include_str!("web/style.css"))),
        _ => None,
    };
    if let Some((kind, body)) = asset {
        return Response {
            code: 200,
            kind,
            body: body.as_bytes().to_vec(),
        };
    }
    let q = match query(path) {
        Ok(Some(q)) => q,
        Ok(None) => return Response::error(404, "Not found"),
        Err(_) => return Response::error(400, "Invalid query"),
    };
    let Ok(_permit) = observers.try_acquire_owned() else {
        return Response::error(503, "Observation capacity busy; retry shortly");
    };
    match observe(workspace, &q, executable).await {
        Ok(value) => Response::json(200, value),
        Err(e) => match q {
            Query::Receipt { .. } => {
                Response::json(200, crate::receipt::unknown(&format!("{e:#}")))
            }
            _ => Response::error(
                503,
                "Snapshot unavailable or observation deadline exceeded; retry shortly",
            ),
        },
    }
}
async fn connection(
    mut stream: TcpStream,
    port: u16,
    workspace: Arc<PathBuf>,
    observers: Arc<Semaphore>,
    executable: Arc<PathBuf>,
) {
    let read = async {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 1024];
            let n = stream.read(&mut chunk).await?;
            ensure!(n > 0, "Incomplete header");
            bytes.extend_from_slice(&chunk[..n]);
            ensure!(bytes.len() <= 8192, "Header too large");
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                return Ok::<_, anyhow::Error>(String::from_utf8(bytes[..end].to_vec())?);
            }
        }
    };
    let response = match tokio::time::timeout(Duration::from_secs(2), read).await {
        Ok(Ok(header)) => route(&header, port, &workspace, observers, &executable).await,
        _ => Response::error(400, "Invalid, oversized or timed-out request header"),
    };
    let status = match response.code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Service Unavailable",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n",
        response.code,
        status,
        response.kind,
        response.body.len()
    );
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(&response.body).await?;
        stream.shutdown().await
    })
    .await;
}
pub async fn serve(workspace: &Path, port: u16) -> Result<()> {
    let workspace = Arc::new(workspace.canonicalize()?);
    // Resolve once: current_exe can acquire a " (deleted)" suffix after upgrades.
    let executable = Arc::new(std::env::current_exe()?);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let port = listener.local_addr()?.port();
    println!(
        "{}",
        json!({"url":format!("http://127.0.0.1:{port}"),"port":port,"read_only":true,"authentication":false})
    );
    let connections = Arc::new(Semaphore::new(32));
    let observers = Arc::new(Semaphore::new(6));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let workspace = workspace.clone();
        let observers = observers.clone();
        let executable = executable.clone();
        tokio::spawn(async move {
            let _permit = permit;
            connection(stream, port, workspace, observers, executable).await;
        });
    }
}
