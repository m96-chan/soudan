#![cfg(target_os = "linux")]
use serde_json::json;
use soudan::grok::{Session, session, state};
use std::path::{Path, PathBuf};

const ID: &str = "01a08e71-6ac7-7202-803e-f973164063d5";

/// Grok keeps one registry of open sessions and one directory per session,
/// named by an encoding of the workspace path.
fn fixture(home: &Path, workspace: &Path, pid: u32, id: &str) -> PathBuf {
    let dir = home.join("sessions/%2Fencoded%2Fworkspace").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        home.join("active_sessions.json"),
        json!([{"session_id": id, "pid": pid, "cwd": workspace,
                "opened_at": "2026-09-11T03:09:52.828566842Z"}])
        .to_string(),
    )
    .unwrap();
    dir
}

fn update(kind: &str, text: &str) -> String {
    json!({"timestamp":"2026-09-11T03:18:05.091Z","method":"session/update",
           "params":{"update":{"sessionUpdate":kind,"content":{"type":"text","text":text}}}})
    .to_string()
}

#[test]
fn a_session_is_resolved_only_for_its_own_process_and_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home/.grok");
    let workspace = temp.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    fixture(&home, &workspace, 42, ID);

    let found = session(&home, &workspace, 42).unwrap().unwrap();
    assert_eq!(found.id, ID);
    assert_eq!(found.leader, home.join("leader.sock"));
    // The transcript directory is located by session id, not by rebuilding the
    // workspace encoding, so the lookup survives a change to that scheme.
    assert!(found.dir.ends_with(ID));

    // Another process, and another workspace, are not this session.
    assert_eq!(session(&home, &workspace, 43).unwrap(), None);
    assert_eq!(session(&home, temp.path(), 42).unwrap(), None);

    // A registry that cannot be read is an absent target, not a guess.
    let empty = temp.path().join("no-such-home");
    assert_eq!(session(&empty, &workspace, 42).unwrap(), None);
}

#[test]
fn the_receipt_boundary_exists_before_a_session_has_streamed_anything() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home/.grok");
    let workspace = temp.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let dir = fixture(&home, &workspace, 42, ID);
    // A brand new chat has a history file but no update stream yet. Anchoring a
    // receipt on the stream would leave every first message unprovable.
    std::fs::write(dir.join("chat_history.jsonl"), "{}\n").unwrap();
    let found = session(&home, &workspace, 42).unwrap().unwrap();
    assert!(found.evidence().is_file());
    assert!(!found.updates().exists());
    assert!(soudan::receipt::Basis::capture(&found.evidence()).is_ok());
}

#[test]
fn a_reply_is_reassembled_from_its_chunks_and_stops_at_the_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home/.grok");
    let workspace = temp.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let dir = fixture(&home, &workspace, 42, ID);
    let turn = |prompt: &str, reply: &[&str]| {
        let mut lines = vec![update("user_message_chunk", prompt)];
        lines.extend(reply.iter().map(|r| update("agent_message_chunk", r)));
        lines.join("\n")
    };
    std::fs::write(
        dir.join("updates.jsonl"),
        format!(
            "{}\n{}\n",
            turn("first question", &["stale ", "answer"]),
            turn("second question", &["GROK-", "PLUGIN", "-OK"])
        ),
    )
    .unwrap();
    let found = session(&home, &workspace, 42).unwrap().unwrap();

    // A single chunk is a fragment; the whole latest reply is the answer, and
    // the previous turn must not bleed into it.
    let value = state(&found).unwrap();
    assert_eq!(value["last_agent_message"], "GROK-PLUGIN-OK");
    assert_eq!(value["session"], ID);
    // Without a state event there is no evidence the session is free.
    assert_eq!(value["status"], "unknown");
}

#[test]
fn an_unfinished_turn_does_not_look_answered() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home/.grok");
    let workspace = temp.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let dir = fixture(&home, &workspace, 42, ID);
    let events = dir.join("events.jsonl");
    let started = r#"{"ts":"2026-09-11T03:18:00.000Z","type":"turn_started"}"#;
    let completed =
        r#"{"ts":"2026-09-11T03:18:05.000Z","type":"turn_ended","outcome":"completed"}"#;
    let cancelled =
        r#"{"ts":"2026-09-11T03:18:05.000Z","type":"turn_ended","outcome":"cancelled"}"#;
    let found = session(&home, &workspace, 42).unwrap().unwrap();

    std::fs::write(&events, format!("{started}\n{completed}\n")).unwrap();
    assert_eq!(state(&found).unwrap()["status"], "idle");

    std::fs::write(&events, format!("{completed}\nnot json\n{started}\n")).unwrap();
    assert_eq!(state(&found).unwrap()["status"], "running");

    // An interrupted turn is over, but it is not a turn that answered.
    std::fs::write(&events, format!("{started}\n{cancelled}\n")).unwrap();
    assert_eq!(state(&found).unwrap()["status"], "aborted");
}

#[tokio::test]
async fn a_send_that_cannot_reach_the_leader_stays_retryable() {
    let temp = tempfile::tempdir().unwrap();
    let missing = Session {
        id: ID.into(),
        pid: 42,
        cwd: temp.path().into(),
        leader: temp.path().join("leader.sock"),
        dir: temp.path().into(),
    };
    // A Grok started without use_leader has no socket at all. Nothing can have
    // been delivered, so the request id must stay usable.
    let failure = soudan::grok::send(&missing, "hello", "r1")
        .await
        .unwrap_err();
    assert!(
        matches!(failure, soudan::codex::Failure::NotAttempted(_)),
        "an unreachable leader delivered nothing"
    );
}
