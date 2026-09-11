#![cfg(target_os = "linux")]
use soudan::live::{LiveTarget, discover_in, validate_message, validate_target};
use std::path::Path;
fn fixture(root: &Path, pid: u32, exe: &str, cwd: &Path) {
    use std::os::unix::fs::symlink;
    let p = root.join(pid.to_string());
    std::fs::create_dir_all(p.join("fd")).unwrap();
    symlink(exe, p.join("exe")).unwrap();
    symlink(cwd, p.join("cwd")).unwrap();
    symlink("/dev/pts/99", p.join("fd/0")).unwrap();
    std::fs::write(p.join("environ"), "SECRET=not-for-output\0").unwrap();
    std::fs::write(
        p.join("stat"),
        format!("{pid} (agent name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 0"),
    )
    .unwrap();
}
#[test]
fn discovery_only_returns_workspace_agent_terminals_with_stable_identity() {
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    std::fs::create_dir(&proc).unwrap();
    fixture(&proc, 42, "/opt/claude/versions/2.1", dir.path());
    fixture(&proc, 43, "/opt/cursor-agent/versions/v/node", dir.path());
    fixture(&proc, 44, "/opt/codex/bin/codex", Path::new("/other"));
    fixture(&proc, 45, "/bin/bash", dir.path());
    let targets = discover_in(&proc, dir.path()).unwrap();
    // Cursor is no longer discovered: it had no session API, and the terminal
    // transport that reached it is gone.
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].agent, "claude-code");
    assert_eq!(targets[0].id, "claude:42:12345");
    assert!(!serde_json::to_string(&targets).unwrap().contains("SECRET"));
    assert!(validate_target(&proc, dir.path(), &targets[0]).is_ok());
    fixture_stat(&proc, 42, 987);
    assert!(validate_target(&proc, dir.path(), &targets[0]).is_err());
}
fn fixture_stat(root: &Path, pid: u32, start: u64) {
    std::fs::write(
        root.join(pid.to_string()).join("stat"),
        format!("{pid} (agent) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 {start} 0"),
    )
    .unwrap();
}
#[test]
fn live_messages_reject_control_sequences_and_oversized_input() {
    assert!(validate_message("Please review this.\nSecond line.").is_ok());
    for text in ["", "  ", "\u{1b}[200~oops", "hi\rcommand", "\u{7}", "\0"] {
        assert!(validate_message(text).is_err());
    }
    assert!(validate_message(&"a".repeat(8193)).is_err());
}
#[test]
fn target_validation_rejects_a_tampered_identity() {
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    std::fs::create_dir(&proc).unwrap();
    fixture(&proc, 42, "/opt/claude/versions/2.1", dir.path());
    let discovered: LiveTarget = discover_in(&proc, dir.path()).unwrap().remove(0);
    assert!(validate_target(&proc, dir.path(), &discovered).is_ok());

    // Validation compares the whole record, so a caller cannot relabel a target
    // and have it delivered through another agent's transport.
    let mut t = discovered.clone();
    t.agent = "codex".into();
    assert!(validate_target(&proc, dir.path(), &t).is_err());
    let mut t = discovered;
    t.tty = Path::new("/dev/pts/1").into();
    assert!(validate_target(&proc, dir.path(), &t).is_err());
}

#[test]
fn an_unresolvable_target_says_which_directory_the_process_actually_runs_in() {
    use soudan::live::resolve_in;
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    let elsewhere = dir.path().join("other-project");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir(&proc).unwrap();
    fixture(&proc, 42, "/opt/claude/versions/2.1", &elsewhere);

    // Discovery compares the working directory exactly, so a chat opened one
    // directory away disappears. The error has to name that directory.
    let error = resolve_in(&proc, dir.path(), "claude:42:12345")
        .unwrap_err()
        .to_string();
    assert!(error.contains(&elsewhere.display().to_string()), "{error}");
    assert!(error.contains("--workspace"), "{error}");

    // A process that no longer exists is a different problem and says so.
    let gone = resolve_in(&proc, dir.path(), "claude:9999:12345")
        .unwrap_err()
        .to_string();
    assert!(gone.contains("is gone"), "{gone}");

    // A live process whose start time moved on was replaced, not relocated.
    fixture(&proc, 43, "/opt/claude/versions/2.1", dir.path());
    let replaced = resolve_in(&proc, dir.path(), "claude:43:99999")
        .unwrap_err()
        .to_string();
    assert!(replaced.contains("was replaced"), "{replaced}");
    assert!(resolve_in(&proc, dir.path(), "claude:43:12345").is_ok());
}

#[test]
fn delivery_ids_cannot_inject_terminal_control_characters() {
    for id in ["\u{1b}[201~", "x\rcommand", "a\nb", "a\tb", " "] {
        assert!(soudan::live::validate_request_id(id).is_err());
    }
    assert!(soudan::live::validate_request_id("claude-review:2026-09-10.1").is_ok());
}

#[test]
fn every_discovered_target_names_a_native_transport() {
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    std::fs::create_dir(&proc).unwrap();
    fixture(&proc, 42, "/opt/codex/bin/codex", dir.path());
    fixture(&proc, 43, "/opt/claude/versions/2.1", dir.path());
    fixture(&proc, 44, "/opt/cursor-agent/versions/v/node", dir.path());

    let targets = discover_in(&proc, dir.path()).unwrap();
    let ids: Vec<_> = targets.iter().map(|t| t.id.as_str()).collect();
    // Discovery offers only what can actually be delivered to. Nothing depends on
    // a terminal emulator any more, so no window is recorded or required.
    assert_eq!(ids, ["codex:42:12345", "claude:43:12345"]);
}

/// Seed the record an interrupted sender would have left behind.
fn record(workspace: &Path, request_id: &str, target: &str, text: &str, status: &str) {
    let db = rusqlite::Connection::open(workspace.join(".soudan/state.db")).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS live_deliveries(request_id TEXT PRIMARY KEY,target TEXT NOT NULL,text TEXT NOT NULL,status TEXT NOT NULL,before_screen TEXT NOT NULL,error TEXT);").unwrap();
    db.execute(
        "INSERT INTO live_deliveries(request_id,target,text,status,before_screen,error) VALUES(?1,?2,?3,?4,'',NULL)",
        rusqlite::params![request_id, target, text, status],
    )
    .unwrap();
}

#[tokio::test]
async fn a_settled_delivery_is_reported_even_though_its_chat_has_ended() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".soudan")).unwrap();
    record(dir.path(), "r1", "codex:42:12345", "hello", "submitted");

    // Nothing in this workspace can be resolved, which is exactly the case that
    // used to lose the verdict: the sender that never saw the first reply must
    // still learn that the message went out.
    let value = soudan::live::send(dir.path(), "codex:42:12345", "hello", "r1")
        .await
        .unwrap();
    assert_eq!(value["status"], "submitted");
    assert_eq!(value["replayed"], true);
    assert_eq!(value["receipt"]["status"], "unknown");

    // The same id carrying a different message stays a mistake, not a replay.
    assert!(
        soudan::live::send(dir.path(), "codex:42:12345", "other", "r1")
            .await
            .is_err()
    );

    // A delivery that provably never happened is retryable, so it goes on to the
    // target and fails there instead of replaying.
    record(dir.path(), "r2", "codex:42:12345", "hello", "not_delivered");
    assert!(
        soudan::live::send(dir.path(), "codex:42:12345", "hello", "r2")
            .await
            .is_err()
    );
}
