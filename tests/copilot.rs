#![cfg(unix)]
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink, path::Path};
const ID: &str = "885aaf80-fa0b-4c6a-aaf2-d5dbeb130698";
fn process(root: &Path, workspace: &Path, port: u16) {
    fs::create_dir_all(root.join("42/fd")).unwrap();
    fs::create_dir_all(root.join("42/net")).unwrap();
    symlink(workspace, root.join("42/cwd")).unwrap();
    symlink("/opt/copilot", root.join("42/exe")).unwrap();
    symlink("/dev/pts/4", root.join("42/fd/0")).unwrap();
    symlink("socket:[991]", root.join("42/fd/5")).unwrap();
    fs::write(
        root.join("42/stat"),
        format!("42 (copilot) {} 123", vec!["0"; 19].join(" ")),
    )
    .unwrap();
    fs::write(
        root.join("42/net/tcp"),
        format!("header\n0: 0100007F:{port:04X} 0:0 0A 0:0 0:0 0 1000 0 991\n"),
    )
    .unwrap();
    fs::write(
        root.join("42/environ"),
        format!("COPILOT_HOME={}\0", workspace.join("home").display()),
    )
    .unwrap();
    let dir = workspace.join("home/session-state").join(ID);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), format!("{}\n", json!({"type":"session.start","id":"start","data":{"sessionId":ID,"context":{"cwd":workspace}}}))).unwrap();
}
#[test]
fn process_and_endpoint_identity_are_required() {
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    process(&proc, dir.path(), 4096);
    let target = format!("copilot:42:123:4096:{ID}");
    assert!(soudan::copilot::resolve_in(&proc, dir.path(), &target).is_ok());
    assert!(
        soudan::copilot::resolve_in(&proc, dir.path(), &target.replace(":123:", ":124:")).is_err()
    );
    assert!(soudan::copilot::resolve_in(&proc, &dir.path().join("other"), &target).is_err());
    assert!(soudan::copilot::resolve_in(&proc, dir.path(), "copilot:42:123:4096:../bad").is_err());
    fs::remove_file(proc.join("42/fd/5")).unwrap();
    assert!(soudan::copilot::resolve_in(&proc, dir.path(), &target).is_err());
}
#[test]
fn state_reads_structured_replies_and_preserves_running_status() {
    let events = vec![
        json!({"type":"user.message","data":{"content":"not a reply"}}),
        json!({"type":"assistant.message","data":{"content":"answer","messageId":"reply","interactionId":"turn"}}),
        json!({"type":"assistant.turn_end"}),
        json!({"type":"assistant.turn_start"}),
        json!({"type":"system.message","data":{"content":"must not be returned"}}),
        json!({"type":"assistant.message","agentId":"child","data":{"content":"child reply"}}),
        json!({"type":"assistant.turn_end","agentId":"child"}),
    ];
    let state = soudan::copilot::state_from_events(&events);
    assert_eq!(state["status"], "running");
    assert_eq!(state["last_agent_message"], "answer");
    assert_eq!(state["reply_scope"], "session_latest");
}

mod rpc {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
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
    fn frame(stream: &mut TcpStream, body: Value) {
        let body = body.to_string();
        let _ = write!(stream, "Content-Length: {}\r\n\r\n{body}", body.len());
    }
    fn server(handler: impl Fn(Value) -> Option<Value> + Send + 'static) -> Server {
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
                    .set_read_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if !matches!(reader.read_line(&mut line), Ok(n) if n>0) {
                        break;
                    }
                    let n: usize = line
                        .trim()
                        .strip_prefix("Content-Length: ")
                        .unwrap()
                        .parse()
                        .unwrap();
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    let mut bytes = vec![0; n];
                    reader.read_exact(&mut bytes).unwrap();
                    let request: Value = serde_json::from_slice(&bytes).unwrap();
                    if request.get("method").is_none() {
                        continue;
                    }
                    let Some(result) = handler(request.clone()) else {
                        break;
                    };
                    // Interleaved notifications must not be mistaken for replies.
                    frame(
                        &mut stream,
                        json!({"jsonrpc":"2.0","method":"session.event","params":{}}),
                    );
                    frame(
                        &mut stream,
                        json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                    );
                }
            }
        });
        Server {
            port,
            stop,
            thread: Some(thread),
        }
    }
    fn standard(request: &Value) -> Value {
        match request["method"].as_str().unwrap() {
            "connect" => json!({"ok":true,"protocolVersion":3}),
            "session.getForeground" => json!({"sessionId":ID}),
            "session.resume" => {
                assert_eq!(request["params"], json!({"sessionId":ID}));
                json!({"sessionId":ID,"isRemote":false})
            }
            other => panic!("Unexpected method {other}"),
        }
    }
    fn append(path: &Path, event: Value) {
        writeln!(
            fs::OpenOptions::new().append(true).open(path).unwrap(),
            "{event}"
        )
        .unwrap();
    }
    #[tokio::test]
    async fn admission_commits_intent_and_receipt_requires_exact_user_message() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".soudan")).unwrap();
        let workspace = dir.path().to_owned();
        let workspace2 = workspace.clone();
        let calls = Arc::new(Mutex::new(0));
        let count = calls.clone();
        let server = server(move |request| {
            if request["method"] == "session.send" {
                *count.lock().unwrap() += 1;
                assert_eq!(request["params"]["mode"], "enqueue");
                assert_eq!(request["params"].as_object().unwrap().len(), 3);
                let db = rusqlite::Connection::open(workspace2.join(".soudan/state.db")).unwrap();
                let (status, basis): (String, String) = db
                    .query_row(
                        "SELECT status,receipt_basis FROM live_deliveries WHERE request_id='test'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(status, "uncertain");
                assert!(serde_json::from_str::<Value>(&basis).unwrap()["message_id"].is_null());
                let log = workspace2
                    .join("home/session-state")
                    .join(ID)
                    .join("events.jsonl");
                // An assistant quoting the prompt is not a receipt.
                append(
                    &log,
                    json!({"type":"assistant.message","data":{"content":request["params"]["prompt"],"messageId":ID}}),
                );
                return Some(json!({"messageId":ID}));
            }
            Some(standard(&request))
        });
        let proc = workspace.join("proc");
        process(&proc, &workspace, server.port);
        let target = format!("copilot:42:123:{}:{ID}", server.port);
        let sent = soudan::live::send_copilot_in(
            &workspace,
            &target,
            "hello",
            "test",
            Some("codex"),
            &proc,
        )
        .await
        .unwrap();
        assert_eq!(sent["status"], "queued");
        let discovered = soudan::live::discover_in(&proc, &workspace).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].id, target);

        let before = fs::read(workspace.join(".soudan/state.db")).unwrap();
        assert_eq!(
            soudan::live::delivery_readonly_in(&workspace, "test", &proc).unwrap()["receipt"]["status"],
            "waiting"
        );
        assert_eq!(
            fs::read(workspace.join(".soudan/state.db")).unwrap(),
            before
        );
        let log = workspace
            .join("home/session-state")
            .join(ID)
            .join("events.jsonl");
        append(
            &log,
            json!({"type":"user.message","data":{"content":soudan::live::prompt("test","hello",Some("codex")).unwrap(),"messageId":ID}}),
        );
        assert_eq!(
            soudan::live::delivery_in(&workspace, "test", &proc).unwrap()["receipt"]["status"],
            "taken"
        );
        assert_eq!(
            soudan::live::send_copilot_in(
                &workspace,
                &target,
                "hello",
                "test",
                Some("codex"),
                &proc
            )
            .await
            .unwrap()["replayed"],
            true
        );
        assert!(
            soudan::live::send_copilot_in(
                &workspace,
                &target,
                "changed",
                "test",
                Some("codex"),
                &proc
            )
            .await
            .is_err()
        );
        assert_eq!(*calls.lock().unwrap(), 1);
        fs::remove_file(proc.join("42/stat")).unwrap();
        assert_eq!(
            soudan::live::delivery_in(&workspace, "test", &proc).unwrap()["receipt"]["status"],
            "taken"
        );
        fs::write(log, "replaced\n").unwrap();
        assert_eq!(
            soudan::live::delivery_in(&workspace, "test", &proc).unwrap()["receipt"]["status"],
            "unknown"
        );
    }
    #[tokio::test]
    async fn lost_ack_is_uncertain_and_retry_never_sends_again() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".soudan")).unwrap();
        let count = Arc::new(Mutex::new(0));
        let count2 = count.clone();
        let server = server(move |r| {
            if r["method"] == "session.send" {
                *count2.lock().unwrap() += 1;
                None
            } else {
                Some(standard(&r))
            }
        });
        let proc = dir.path().join("proc");
        process(&proc, dir.path(), server.port);
        let target = format!("copilot:42:123:{}:{ID}", server.port);
        assert!(
            soudan::live::send_copilot_in(dir.path(), &target, "hello", "lost", None, &proc)
                .await
                .is_err()
        );
        let row = soudan::live::delivery_in(dir.path(), "lost", &proc).unwrap();
        assert_eq!(row["status"], "uncertain", "delivery: {row}");
        assert_eq!(row["receipt"]["status"], "unknown");
        assert_eq!(
            soudan::live::send_copilot_in(dir.path(), &target, "hello", "lost", None, &proc)
                .await
                .unwrap()["replayed"],
            true
        );
        assert_eq!(*count.lock().unwrap(), 1);
    }
    #[tokio::test]
    async fn changed_foreground_is_proven_non_delivery() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".soudan")).unwrap();
        let server = server(|r| {
            if r["method"] == "session.getForeground" {
                Some(json!({"sessionId":"11111111-1111-4111-8111-111111111111"}))
            } else {
                Some(standard(&r))
            }
        });
        let proc = dir.path().join("proc");
        process(&proc, dir.path(), server.port);
        let target = format!("copilot:42:123:{}:{ID}", server.port);
        assert!(
            soudan::live::send_copilot_in(dir.path(), &target, "hello", "changed", None, &proc)
                .await
                .is_err()
        );
        assert_eq!(
            soudan::live::delivery_in(dir.path(), "changed", &proc).unwrap()["status"],
            "not_delivered"
        );
    }
    #[test]
    fn incompatible_protocol_is_not_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(|r| {
            assert_eq!(r["method"], "connect");
            Some(json!({"ok":true,"protocolVersion":4}))
        });
        let proc = dir.path().join("proc");
        process(&proc, dir.path(), server.port);
        assert!(
            soudan::live::discover_in(&proc, dir.path())
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn context_changes_and_symlinked_logs_cannot_cross_workspaces() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    process(&proc, dir.path(), 4096);
    let target = format!("copilot:42:123:4096:{ID}");
    let log = dir
        .path()
        .join("home/session-state")
        .join(ID)
        .join("events.jsonl");
    writeln!(
        fs::OpenOptions::new().append(true).open(&log).unwrap(),
        "{}",
        json!({"type":"session.context_changed","data":{"cwd":"/another-workspace"}})
    )
    .unwrap();
    assert!(soudan::copilot::resolve_in(&proc, dir.path(), &target).is_err());
    fs::rename(&log, log.with_extension("saved")).unwrap();
    symlink(log.with_extension("saved"), &log).unwrap();
    assert!(soudan::copilot::resolve_in(&proc, dir.path(), &target).is_err());
}
