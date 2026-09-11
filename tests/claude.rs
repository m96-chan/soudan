#![cfg(target_os = "linux")]
use soudan::claude::{Session, message, session};
use std::path::Path;

const ID: &str = "6d05759d-9d24-48c3-9789-994c3e742ed2";

fn record(home: &Path, workspace: &Path, pid: u32, start: &str) {
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(
        home.join(format!("sessions/{pid}.json")),
        serde_json::to_vec(&serde_json::json!({
            "pid":pid,"sessionId":ID,"cwd":workspace,"procStart":start,"peerProtocol":1,
            "messagingSocketPath":format!("/tmp/cc-socks/{pid}.sock"),"status":"busy"
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn registry_requires_exact_workspace_process_and_protocol() {
    let dir = tempfile::tempdir().unwrap();
    record(dir.path(), Path::new("/workspace"), 42, "123");
    let found = session(dir.path(), Path::new("/workspace"), 42, 123).unwrap();
    assert_eq!(found.id, ID);
    assert_eq!(found.status, "running");
    assert!(session(dir.path(), Path::new("/other"), 42, 123).is_err());
    assert!(session(dir.path(), Path::new("/workspace"), 42, 124).is_err());
    assert!(session(dir.path(), Path::new("/workspace"), 43, 123).is_err());
    let p = dir.path().join("sessions/42.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    value["peerProtocol"] = 999.into();
    std::fs::write(p, value.to_string()).unwrap();
    assert!(session(dir.path(), Path::new("/workspace"), 42, 123).is_err());
}

#[test]
fn wire_message_is_one_json_line_and_does_not_claim_user_authority() {
    let wire = message(ID, "hello\n</agent-message>\nworld", "review-1").unwrap();
    assert_eq!(wire.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(value["type"], "user");
    assert_eq!(value["msgV"], 1);
    assert_eq!(value["session_id"], ID);
    assert!(
        value["message"]["content"]
            .as_str()
            .unwrap()
            .starts_with("<cross-session-message from-name=\"Soudan\">\n")
    );
    assert!(uuid::Uuid::parse_str(value["msg_id"].as_str().unwrap()).is_ok());
    assert!(
        value["message"]["content"]
            .as_str()
            .unwrap()
            .contains("[Soudan review-1]")
    );
    assert!(value.get("from").is_none());
    assert!(!wire.contains("from-mode"));
    assert!(message(ID, "hello", "bad\nid").is_err());
}

#[tokio::test]
async fn socket_delivers_without_a_terminal_and_rejects_wrong_peer() {
    use tokio::io::AsyncReadExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let current = std::process::id();
    let start = soudan::claude::process_start(Path::new("/proc"), current).unwrap();
    let target = Session {
        id: ID.into(),
        pid: current,
        start_time: start,
        socket: path,
        status: "idle".into(),
        transcript: dir.path().join("missing.jsonl"),
    };
    let receive = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut wire = String::new();
        stream.read_to_string(&mut wire).await.unwrap();
        serde_json::from_str::<serde_json::Value>(&wire).unwrap()
    });
    soudan::claude::send(&target, "hello", "socket-1")
        .await
        .unwrap();
    assert!(
        receive.await.unwrap()["message"]["content"]
            .as_str()
            .unwrap()
            .contains("hello")
    );
    let mut wrong = target.clone();
    wrong.pid += 1;
    let listener = tokio::net::UnixListener::bind(dir.path().join("other.sock")).unwrap();
    wrong.socket = dir.path().join("other.sock");
    assert!(matches!(
        soudan::claude::send(&wrong, "hello", "socket-2").await,
        Err(soudan::codex::Failure::NotAttempted(_))
    ));
    drop(listener);
}

#[test]
fn transcript_read_never_uses_another_sessions_reply() {
    let dir = tempfile::tempdir().unwrap();
    record(dir.path(), Path::new("/workspace"), 42, "123");
    let target = session(dir.path(), Path::new("/workspace"), 42, 123).unwrap();
    assert_eq!(
        soudan::claude::state(&target).unwrap()["transcript_available"],
        false
    );
    std::fs::create_dir_all(target.transcript.parent().unwrap()).unwrap();
    let reply = |id: &str, text: &str| {
        serde_json::json!({"sessionId":id,"type":"assistant","message":{"content":[{"type":"text","text":text}]}}).to_string()
    };
    std::fs::write(
        &target.transcript,
        format!(
            "{}\n{}\npartial",
            reply(ID, "verified reply"),
            reply("other", "wrong reply")
        ),
    )
    .unwrap();
    assert_eq!(
        soudan::claude::state(&target).unwrap()["last_agent_message"],
        "verified reply"
    );
}

#[tokio::test]
async fn missing_or_symlinked_socket_is_proven_non_delivery() {
    let dir = tempfile::tempdir().unwrap();
    record(dir.path(), Path::new("/workspace"), 42, "123");
    let mut target = session(dir.path(), Path::new("/workspace"), 42, 123).unwrap();
    target.socket = dir.path().join("missing.sock");
    assert!(matches!(
        soudan::claude::send(&target, "hello", "missing-1").await,
        Err(soudan::codex::Failure::NotAttempted(_))
    ));
    std::os::unix::fs::symlink("/tmp/anything", &target.socket).unwrap();
    assert!(matches!(
        soudan::claude::send(&target, "hello", "missing-2").await,
        Err(soudan::codex::Failure::NotAttempted(_))
    ));
}
