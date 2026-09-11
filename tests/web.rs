use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Server {
    child: Child,
    port: u16,
}
impl Server {
    fn start(workspace: &Path) -> Self {
        Self::start_binary(workspace, Path::new(env!("CARGO_BIN_EXE_soudan")))
    }
    fn start_binary(workspace: &Path, binary: &Path) -> Self {
        let mut child = Command::new(binary)
            .arg("--workspace")
            .arg(workspace)
            .args(["web", "--port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let ready: Value = serde_json::from_str(&line).expect("JSON listening notification");
        Self {
            child,
            port: ready["port"].as_u64().unwrap() as u16,
        }
    }
    fn request(&self, path: &str) -> String {
        self.raw(&format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            self.port
        ))
    }
    fn raw(&self, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        out
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn body(response: &str) -> Value {
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}
#[test]
fn web_requires_explicit_port_and_has_no_external_bind_option() {
    for args in [vec!["web"], vec!["web", "--port", "0", "--host", "0.0.0.0"]] {
        assert!(
            !Command::new(env!("CARGO_BIN_EXE_soudan"))
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}
#[test]
fn serves_fixed_assets_and_rejects_cross_origin_and_file_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let server = Server::start(tmp.path());
    let home = server.request("/");
    assert!(home.starts_with("HTTP/1.1 200"));
    assert!(home.contains("Content-Security-Policy:"));
    assert!(server.request("/app.js").contains("textContent"));
    assert!(
        server
            .request("/../../etc/passwd")
            .starts_with("HTTP/1.1 404")
    );
    assert!(
        server
            .request("/api/overview?path=/etc/passwd")
            .starts_with("HTTP/1.1 400")
    );
    assert!(
        server
            .raw("GET /api/overview HTTP/1.1\r\nHost: evil.example\r\n\r\n")
            .starts_with("HTTP/1.1 403")
    );
    assert!(
        server
            .raw(&format!(
                "POST / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                server.port
            ))
            .starts_with("HTTP/1.1 405")
    );
    assert!(server.raw(&format!("GET /api/overview HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: https://evil.example\r\n\r\n",server.port)).starts_with("HTTP/1.1 403"));
    assert!(
        server
            .raw(&format!(
                "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nHost: 127.0.0.1:{}\r\n\r\n",
                server.port, server.port
            ))
            .starts_with("HTTP/1.1 403")
    );
    assert!(
        server
            .request("/api/messages?room=%GG")
            .starts_with("HTTP/1.1 400")
    );
    assert!(
        server
            .request("/api/overview?offset=0&offset=1")
            .starts_with("HTTP/1.1 400")
    );
    assert!(
        body(&server.request("/api/overview"))["deliveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!tmp.path().join(".soudan").exists());
}
#[test]
fn exposes_records_and_legacy_receipts_without_mutating_database() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let path = tmp.path().join(".soudan/state.db");
    let store = soudan::Store::open(&path).unwrap();
    store
        .post("review & room", "claude", "<script>alert(1)</script>")
        .unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE live_deliveries(request_id TEXT PRIMARY KEY,target TEXT,text TEXT,status TEXT,before_screen TEXT,error TEXT); INSERT INTO live_deliveries VALUES('legacy','codex:42:123','hello','queued','private screen',NULL);").unwrap();
    drop(db);
    let before = fs::read(&path).unwrap();
    let server = Server::start(tmp.path());
    let overview = body(&server.request("/api/overview"));
    assert_eq!(overview["deliveries"][0]["status"], "queued");
    assert!(!overview.to_string().contains("private screen"));
    let messages = body(&server.request("/api/messages?room=review%20%26%20room"));
    assert_eq!(messages["messages"][0]["text"], "<script>alert(1)</script>");
    let receipt = body(&server.request("/api/receipt?id=legacy"));
    assert_eq!(receipt["status"], "unknown");
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn a_blocked_receipt_times_out_without_blocking_other_requests() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let log = tmp.path().join("log");
    fs::write(&log, "before").unwrap();
    let basis = serde_json::to_string(&soudan::receipt::Basis::capture(&log).unwrap()).unwrap();
    fs::remove_file(&log).unwrap();
    let path = std::ffi::CString::new(log.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated path points inside this test's tempdir.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let db = rusqlite::Connection::open(tmp.path().join(".soudan/state.db")).unwrap();
    db.execute_batch("CREATE TABLE live_deliveries(request_id TEXT PRIMARY KEY,target TEXT,text TEXT,status TEXT,before_screen TEXT,error TEXT,receipt_basis TEXT);").unwrap();
    db.execute(
        "INSERT INTO live_deliveries VALUES('blocked','codex:42:123','hello','queued','',NULL,?1)",
        [basis],
    )
    .unwrap();
    let server = Server::start(tmp.path());
    let start = Instant::now();
    std::thread::scope(|scope| {
        let request = scope.spawn(|| server.request("/api/receipt?id=blocked"));
        std::thread::sleep(Duration::from_millis(200));
        let quick = Instant::now();
        assert!(server.request("/").starts_with("HTTP/1.1 200"));
        assert_eq!(
            body(&server.request("/api/overview"))["deliveries"][0]["status"],
            "queued"
        );
        assert!(quick.elapsed() < Duration::from_secs(1));
        let result = body(&request.join().unwrap());
        assert_eq!(result["status"], "unknown");
        assert!(result["reason"].as_str().unwrap().contains("timed out"));
    });
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn external_state_symlinks_are_not_served() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let store = soudan::Store::open(&outside.path().join("other.db")).unwrap();
    store.post("secret", "user", "outside content").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("other.db"),
        tmp.path().join(".soudan/state.db"),
    )
    .unwrap();
    let server = Server::start(tmp.path());
    let response = server.request("/api/overview");
    assert!(response.starts_with("HTTP/1.1 503"));
    assert!(!response.contains("outside content"));
}

#[test]
fn room_pagination_and_receipts_include_positive_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let dbpath = tmp.path().join(".soudan/state.db");
    let store = soudan::Store::open(&dbpath).unwrap();
    for n in 0..102 {
        store.post("r", "codex", &format!("message {n}")).unwrap();
    }
    let log = tmp.path().join("log");
    fs::write(&log, "before").unwrap();
    let basis = serde_json::to_string(&soudan::receipt::Basis::capture(&log).unwrap()).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"[Soudan delivered]")
        .unwrap();
    let db = rusqlite::Connection::open(dbpath).unwrap();
    db.execute_batch("CREATE TABLE live_deliveries(request_id TEXT PRIMARY KEY,target TEXT,text TEXT,status TEXT,before_screen TEXT,error TEXT,receipt_basis TEXT); INSERT INTO jobs VALUES('job-1','r','codex','prompt',30,123,'completed','done',NULL);").unwrap();
    db.execute("INSERT INTO live_deliveries VALUES('delivered','codex:42:123','hello','submitted','',NULL,?1)",[basis]).unwrap();
    let server = Server::start(tmp.path());
    let latest = body(&server.request("/api/messages?room=r"));
    assert_eq!(latest["messages"].as_array().unwrap().len(), 100);
    assert_eq!(latest["has_older"], true);
    let oldest = latest["messages"][0]["id"].as_i64().unwrap();
    let older = body(&server.request(&format!("/api/messages?room=r&before={oldest}")));
    assert_eq!(older["messages"].as_array().unwrap().len(), 2);
    assert_eq!(older["has_older"], false);
    assert_eq!(
        body(&server.request("/api/receipt?id=delivered"))["status"],
        "taken"
    );
    assert_eq!(
        body(&server.request("/api/overview"))["jobs"][0]["status"],
        "completed"
    );
}

#[cfg(unix)]
#[test]
fn replacing_the_server_binary_does_not_break_observer_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let binary = tmp.path().join("soudan");
    fs::copy(env!("CARGO_BIN_EXE_soudan"), &binary).unwrap();
    let server = Server::start_binary(tmp.path(), &binary);
    let replacement = tmp.path().join("replacement");
    fs::copy(env!("CARGO_BIN_EXE_soudan"), &replacement).unwrap();
    fs::rename(replacement, &binary).unwrap();
    assert!(server.request("/api/overview").starts_with("HTTP/1.1 200"));
}
