use serde_json::json;
use std::{fs, path::Path};

#[cfg(unix)]
fn process(root: &Path, workspace: &Path, port: u16) {
    use std::os::unix::fs::symlink;
    fs::create_dir_all(root.join("42/fd")).unwrap();
    fs::create_dir_all(root.join("42/net")).unwrap();
    symlink(workspace, root.join("42/cwd")).unwrap();
    symlink("/opt/opencode/bin/opencode.exe", root.join("42/exe")).unwrap();
    symlink("/dev/pts/7", root.join("42/fd/0")).unwrap();
    symlink("socket:[991]", root.join("42/fd/5")).unwrap();
    fs::write(
        root.join("42/stat"),
        format!("42 (opencode) {} 123", vec!["0"; 19].join(" ")),
    )
    .unwrap();
    fs::write(
        root.join("42/net/tcp"),
        format!("header\n0: 0100007F:{port:04X} 00000000:0000 0A 0:0 0:0 0 1000 0 991\n"),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn endpoint_is_bound_to_the_original_process_workspace_and_socket_inode() {
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    process(&proc, dir.path(), 4096);
    let target = "opencode:42:123:4096:ses_test";
    let session = soudan::opencode::resolve_in(&proc, dir.path(), target).unwrap();
    assert_eq!(session.id, "ses_test");
    assert!(soudan::opencode::resolve_in(&proc, &dir.path().join("other"), target).is_err());
    assert!(
        soudan::opencode::resolve_in(&proc, dir.path(), "opencode:42:124:4096:ses_test").is_err()
    );
    assert!(
        soudan::opencode::resolve_in(&proc, dir.path(), "opencode:42:123:4096:ses_../bad").is_err()
    );
    fs::remove_file(proc.join("42/fd/5")).unwrap();
    assert!(soudan::opencode::resolve_in(&proc, dir.path(), target).is_err());
}

#[test]
fn protocol_guard_requires_the_documented_v2_admission_contract() {
    assert!(soudan::opencode::validate_contract(&json!({"openapi":"3.1.0","paths":{}})).is_err());
}

#[cfg(unix)]
mod http {
    use super::*;
    use serde_json::Value;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };
    struct Server {
        port: u16,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.thread.take().unwrap().join().unwrap();
        }
    }
    fn server(handler: impl Fn(&str, &str, Value) -> (u16, Value) + Send + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let thread = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let fields: Vec<_> = first.split_whitespace().collect();
                let (status, body) = handler(
                    fields[0],
                    fields[1],
                    serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                );
                let body = body.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Server {
            port,
            stop,
            thread: Some(thread),
        }
    }
    fn contract() -> Value {
        let mut doc = json!({"openapi":"3.1.0","paths":{}});
        for (path, method, op) in [
            ("/api/session", "get", "list"),
            ("/api/session/{sessionID}", "get", "get"),
            ("/api/session/{sessionID}/prompt", "post", "prompt"),
            ("/api/session/{sessionID}/message", "get", "messages"),
            (
                "/api/session/{sessionID}/message/{messageID}",
                "get",
                "message",
            ),
            ("/api/session/active", "get", "active"),
        ] {
            doc["paths"][path] = json!({method:{"operationId":format!("v2.session.{op}")}});
        }
        doc["paths"]["/api/session/{sessionID}/prompt"]["post"]["requestBody"] = json!({"content":{"application/json":{"schema":{"properties":{"id":{"pattern":"^msg_"}}}}}});
        doc
    }
    fn fixture(dir: &Path, port: u16) -> (std::path::PathBuf, String) {
        let proc = dir.join("proc");
        process(&proc, dir, port);
        (proc, format!("opencode:42:123:{port}:ses_test"))
    }
    #[tokio::test]
    async fn admission_commits_intent_then_receipt_matches_identity_and_retry_never_resends() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".soudan")).unwrap();
        let workspace = dir.path().to_owned();
        let mid = Arc::new(Mutex::new(String::new()));
        let seen = mid.clone();
        let posts = Arc::new(Mutex::new(0));
        let sent = posts.clone();
        let server = server(move |method, path, body| {
            if path == "/doc" {
                return (200, contract());
            }
            if path == "/api/session/ses_test" {
                return (
                    200,
                    json!({"data":{"id":"ses_test","location":{"directory":workspace}}}),
                );
            }
            if method == "POST" {
                assert_eq!(path, "/api/session/ses_test/prompt");
                assert!(
                    body["prompt"]["text"]
                        .as_str()
                        .unwrap()
                        .contains("[Soudan r1] Declared sender: codex (unverified)")
                );
                let db = rusqlite::Connection::open(workspace.join(".soudan/state.db")).unwrap();
                let (status, basis): (String, String) = db
                    .query_row(
                        "SELECT status,receipt_basis FROM live_deliveries WHERE request_id='r1'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(status, "uncertain");
                assert_eq!(
                    serde_json::from_str::<Value>(&basis).unwrap()["message_id"],
                    body["id"]
                );
                *seen.lock().unwrap() = body["id"].as_str().unwrap().into();
                *sent.lock().unwrap() += 1;
                return (
                    200,
                    json!({"data":{"id":body["id"],"sessionID":"ses_test"}}),
                );
            }
            assert_eq!(
                path,
                format!("/api/session/ses_test/message/{}", seen.lock().unwrap())
            );
            (
                200,
                json!({"data":{"id":*seen.lock().unwrap(),"type":"user","text":"irrelevant"}}),
            )
        });
        let (proc, target) = fixture(dir.path(), server.port);
        let first =
            soudan::live::send_opencode_in(dir.path(), &target, "hi", "r1", Some("codex"), &proc)
                .await
                .unwrap();
        assert_eq!(first["status"], "submitted");
        let delivery = soudan::live::delivery_in(dir.path(), "r1", &proc).unwrap();
        assert_eq!(delivery["receipt"]["status"], "taken");
        assert_eq!(delivery["sender"], "codex");
        assert_eq!(
            soudan::live::send_opencode_in(dir.path(), &target, "hi", "r1", Some("codex"), &proc)
                .await
                .unwrap()["replayed"],
            true
        );
        assert!(
            soudan::live::send_opencode_in(dir.path(), &target, "hi", "r1", Some("claude"), &proc)
                .await
                .is_err()
        );
        assert_eq!(*posts.lock().unwrap(), 1);
    }
    #[test]
    fn absent_or_wrong_id_is_not_taken_and_reused_process_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_owned();
        let mode = Arc::new(Mutex::new(0));
        let flag = mode.clone();
        let server = server(move |_, path, _| {
            if path == "/api/session/ses_test" {
                return (
                    200,
                    json!({"data":{"id":"ses_test","location":{"directory":workspace}}}),
                );
            }
            match *flag.lock().unwrap() {
                0 => (404, Value::Null),
                1 => (
                    200,
                    json!({"data":{"id":"msg_other","type":"user","text":"[Soudan request]"}}),
                ),
                _ => (401, Value::Null),
            }
        });
        let (proc, target) = fixture(dir.path(), server.port);
        let evidence = soudan::opencode::Evidence::new(
            soudan::opencode::resolve_in(&proc, dir.path(), &target).unwrap(),
        );
        assert_eq!(
            soudan::opencode::receipt(&proc, &evidence, &target)["status"],
            "waiting"
        );
        *mode.lock().unwrap() = 1;
        assert_eq!(
            soudan::opencode::receipt(&proc, &evidence, &target)["status"],
            "unknown"
        );
        *mode.lock().unwrap() = 2;
        assert_eq!(
            soudan::opencode::receipt(&proc, &evidence, &target)["status"],
            "unknown"
        );
        fs::write(
            proc.join("42/stat"),
            format!("42 (opencode) {} 124", vec!["0"; 19].join(" ")),
        )
        .unwrap();
        assert_eq!(
            soudan::opencode::receipt(&proc, &evidence, &target)["status"],
            "unknown"
        );
    }
    #[tokio::test]
    async fn auth_preflight_is_retryable_but_failed_post_is_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".soudan")).unwrap();
        let workspace = dir.path().to_owned();
        let allowed = Arc::new(AtomicBool::new(false));
        let flag = allowed.clone();
        let server = server(move |method, path, _| {
            if !flag.load(Ordering::SeqCst) {
                assert_eq!(method, "GET");
                return (401, Value::Null);
            }
            if path == "/doc" {
                return (200, contract());
            }
            if path == "/api/session/ses_test" {
                return (
                    200,
                    json!({"data":{"id":"ses_test","location":{"directory":workspace}}}),
                );
            }
            if method == "GET" {
                return (404, Value::Null);
            }
            assert_eq!(method, "POST");
            (503, Value::Null)
        });
        let (proc, target) = fixture(dir.path(), server.port);
        assert!(
            soudan::live::send_opencode_in(dir.path(), &target, "hi", "r", None, &proc)
                .await
                .is_err()
        );
        assert_eq!(
            soudan::live::delivery_in(dir.path(), "r", &proc).unwrap()["status"],
            "not_delivered"
        );
        allowed.store(true, Ordering::SeqCst);
        assert!(
            soudan::live::send_opencode_in(dir.path(), &target, "hi", "r", None, &proc)
                .await
                .is_err()
        );
        assert_eq!(
            soudan::live::delivery_in(dir.path(), "r", &proc).unwrap()["status"],
            "uncertain"
        );
        assert_eq!(
            soudan::live::send_opencode_in(dir.path(), &target, "hi", "r", None, &proc)
                .await
                .unwrap()["replayed"],
            true
        );
    }
    #[test]
    fn discovery_filters_workspace_and_read_fetches_assistant_details_not_preview() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_owned();
        let server = server(move |_, path, _| {
            if path == "/doc" {
                return (200, contract());
            }
            if path.starts_with("/api/session?") {
                assert!(path.contains("directory=%2F"));
                return (
                    200,
                    json!({"data":[{"id":"ses_test","location":{"directory":workspace}},{"id":"ses_other","location":{"directory":"/elsewhere"}}],"cursor":{}}),
                );
            }
            if path == "/api/session/ses_test" {
                return (
                    200,
                    json!({"data":{"id":"ses_test","location":{"directory":workspace}}}),
                );
            }
            if path == "/api/session/active" {
                return (200, json!({"data":{"ses_test":{"type":"running"}}}));
            }
            if path.contains("/message?") {
                return (
                    200,
                    json!({"data":[{"id":"msg_reply","type":"assistant"}],"cursor":{}}),
                );
            }
            assert_eq!(path, "/api/session/ses_test/message/msg_reply");
            (
                200,
                json!({"data":{"id":"msg_reply","type":"assistant","content":[{"type":"text","text":"actual reply"}],"finish":"stop"}}),
            )
        });
        let (proc, target) = fixture(dir.path(), server.port);
        let found = soudan::live::discover_in(&proc, dir.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, target);
        let state = soudan::opencode::state(&proc, dir.path(), &target).unwrap();
        assert_eq!(state["status"], "running");
        assert_eq!(state["last_agent_message"], "actual reply");
        assert_eq!(state["reply"]["finish"], "stop");
    }
    #[test]
    fn receipt_timeout_is_unknown_and_toast_never_touches_the_composer() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_owned();
        let server = server(move |method, path, body| {
            if path == "/api/session/ses_test" {
                return (
                    200,
                    json!({"data":{"id":"ses_test","location":{"directory":workspace}}}),
                );
            }
            if path.starts_with("/tui/show-toast?") {
                assert_eq!(method, "POST");
                assert_eq!(body["message"], "Done");
                return (200, json!(true));
            }
            assert!(path.starts_with("/api/session/ses_test/message/"));
            thread::sleep(Duration::from_millis(2300));
            (200, json!({"data":{"id":"msg_wrong","type":"user"}}))
        });
        let (proc, target) = fixture(dir.path(), server.port);
        assert_eq!(
            soudan::opencode::notify(&proc, dir.path(), &target, "Done").unwrap()["kind"],
            "toast"
        );
        let evidence = soudan::opencode::Evidence::new(
            soudan::opencode::resolve_in(&proc, dir.path(), &target).unwrap(),
        );
        let start = std::time::Instant::now();
        assert_eq!(
            soudan::opencode::receipt(&proc, &evidence, &target)["status"],
            "unknown"
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn changed_workspace_and_nonloopback_listener_cannot_receive_a_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(|method, path, _| {
            assert_eq!(method, "GET");
            if path == "/doc" {
                return (200, contract());
            }
            (
                200,
                json!({"data":{"id":"ses_test","location":{"directory":"/other-project"}}}),
            )
        });
        let (proc, target) = fixture(dir.path(), server.port);
        let evidence = soudan::opencode::Evidence::new(
            soudan::opencode::resolve_in(&proc, dir.path(), &target).unwrap(),
        );
        assert!(matches!(
            soudan::opencode::send(&proc, &evidence, "hi", None, "r"),
            Err(soudan::codex::Failure::NotAttempted(_))
        ));
        let tcp = proc.join("42/net/tcp");
        fs::write(
            &tcp,
            fs::read_to_string(&tcp)
                .unwrap()
                .replace("0100007F", "00000000"),
        )
        .unwrap();
        assert!(soudan::opencode::resolve_in(&proc, dir.path(), &target).is_err());
    }
}
