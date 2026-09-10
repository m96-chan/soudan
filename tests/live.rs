#![cfg(target_os = "linux")]
use soudan::live::{LiveTarget, discover_in, validate_message, validate_target};
use std::path::Path;
fn fixture(root: &Path, pid: u32, exe: &str, cwd: &Path, window: &str) {
    use std::os::unix::fs::symlink;
    let p = root.join(pid.to_string());
    std::fs::create_dir_all(p.join("fd")).unwrap();
    symlink(exe, p.join("exe")).unwrap();
    symlink(cwd, p.join("cwd")).unwrap();
    symlink("/dev/pts/99", p.join("fd/0")).unwrap();
    std::fs::write(
        p.join("environ"),
        format!("KITTY_WINDOW_ID={window}\0SECRET=not-for-output\0"),
    )
    .unwrap();
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
    fixture(&proc, 42, "/opt/claude/versions/2.1", dir.path(), "52");
    fixture(
        &proc,
        43,
        "/opt/cursor-agent/versions/v/node",
        dir.path(),
        "56",
    );
    fixture(&proc, 44, "/opt/codex/bin/codex", Path::new("/other"), "53");
    fixture(&proc, 45, "/bin/bash", dir.path(), "54");
    let targets = discover_in(&proc, dir.path()).unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].agent, "claude-code");
    assert_eq!(targets[0].id, "kitty:42:12345");
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
fn target_validation_rejects_window_replacement_and_other_workspaces() {
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    std::fs::create_dir(&proc).unwrap();
    fixture(&proc, 42, "/opt/claude/versions/2.1", dir.path(), "52");
    let mut t: LiveTarget = discover_in(&proc, dir.path()).unwrap().remove(0);
    t.window_id = Some(53);
    assert!(validate_target(&proc, dir.path(), &t).is_err());
}

#[test]
fn ready_check_refuses_busy_screens_and_existing_drafts() {
    use soudan::live::ensure_ready;
    assert!(
        ensure_ready(
            "claude-code",
            "A previous response.\n────────\n❯ \n────────\n? for shortcuts"
        )
        .is_ok()
    );
    assert!(ensure_ready("claude-code", "❯ unfinished user draft\n────────").is_err());
    assert!(ensure_ready("claude-code", "Thinking…\nesc to interrupt").is_err());
    assert!(ensure_ready("cursor", "No recognizable input box").is_err());
    assert!(ensure_ready("codex", "› \n? for shortcuts").is_ok());
}

#[test]
fn multiline_draft_is_not_mistaken_for_empty_input() {
    assert!(
        soudan::live::ensure_ready("claude-code", "❯ \n  unfinished second line\n────────")
            .is_err()
    );
}

#[tokio::test]
async fn shortcut_setup_updates_binary_and_disconnect_restores_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".soudan")).unwrap();
    let config = dir.path().join("kitty.conf");
    let original = "font_size 12\n# keep this comment\n";
    std::fs::write(&config, original).unwrap();
    soudan::live::configure_shortcut(&config, dir.path(), Path::new("/old/soudan"), 0).unwrap();
    soudan::live::configure_shortcut(&config, dir.path(), Path::new("/new/soudan"), 0).unwrap();
    let updated = std::fs::read_to_string(&config).unwrap();
    assert_eq!(updated.matches("map ctrl+shift+f12").count(), 1);
    assert!(updated.contains("/new/soudan"));
    assert!(!updated.contains("/old/soudan"));
    assert!(!updated.contains("allow_remote_control yes"));
    soudan::live::disconnect(dir.path()).await.unwrap();
    assert_eq!(std::fs::read_to_string(config).unwrap(), original);
}

#[test]
fn delivery_ids_cannot_inject_terminal_control_characters() {
    for id in ["\u{1b}[201~", "x\rcommand", "a\nb", "a\tb", " "] {
        assert!(soudan::live::validate_request_id(id).is_err());
    }
    assert!(soudan::live::validate_request_id("claude-review:2026-09-10.1").is_ok());
}

#[tokio::test]
async fn disconnect_cleans_up_a_crashed_bridges_stale_socket() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".soudan")).unwrap();
    let path = dir.path().join(".soudan/kitty.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    drop(listener);
    soudan::live::disconnect(dir.path()).await.unwrap();
    assert!(!path.exists());
}

#[test]
fn codex_is_discoverable_without_a_kitty_window() {
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    std::fs::create_dir(&proc).unwrap();
    fixture(&proc, 42, "/opt/codex/bin/codex", dir.path(), "53");
    fixture(&proc, 43, "/opt/codex/bin/codex", dir.path(), "");
    fixture(&proc, 44, "/opt/claude/versions/2.1", dir.path(), "");
    std::fs::write(proc.join("43/environ"), "TERM=xterm\0").unwrap();
    std::fs::write(proc.join("44/environ"), "TERM=xterm\0").unwrap();

    let targets = discover_in(&proc, dir.path()).unwrap();
    let ids: Vec<_> = targets.iter().map(|t| t.id.as_str()).collect();
    // The id names the transport, and Codex is never delivered to through Kitty.
    assert_eq!(ids, ["codex:42:12345", "codex:43:12345"]);
    assert_eq!(targets[0].window_id, Some(53));
    assert_eq!(targets[1].window_id, None);
}
