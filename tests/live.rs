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
    use soudan::live::{DEFAULT_SHORTCUT, configure_shortcut};
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".soudan")).unwrap();
    let config = dir.path().join("kitty.conf");
    let original = "font_size 12\n# keep this comment\n";
    std::fs::write(&config, original).unwrap();
    configure_shortcut(
        &config,
        dir.path(),
        Path::new("/old/soudan"),
        0,
        DEFAULT_SHORTCUT,
    )
    .unwrap();
    configure_shortcut(
        &config,
        dir.path(),
        Path::new("/new/soudan"),
        0,
        DEFAULT_SHORTCUT,
    )
    .unwrap();
    let updated = std::fs::read_to_string(&config).unwrap();
    assert_eq!(updated.matches("map ctrl+shift+f12").count(), 1);
    assert!(updated.contains("/new/soudan"));
    assert!(!updated.contains("/old/soudan"));
    assert!(!updated.contains("allow_remote_control yes"));
    soudan::live::disconnect(dir.path()).await.unwrap();
    assert_eq!(std::fs::read_to_string(config).unwrap(), original);
}

#[test]
fn a_second_workspace_needs_its_own_key_and_the_first_keeps_the_one_it_chose() {
    use soudan::live::{configure_shortcut, recorded_shortcut, validate_shortcut};
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one");
    let two = dir.path().join("two");
    for w in [&one, &two] {
        std::fs::create_dir_all(w.join(".soudan")).unwrap();
    }
    let config = dir.path().join("kitty.conf");
    std::fs::write(&config, "font_size 12\n").unwrap();

    configure_shortcut(
        &config,
        &one,
        Path::new("/soudan"),
        0,
        "ctrl+shift+backslash",
    )
    .unwrap();
    // One kitty.conf serves both, so reusing the key has to be refused, not silently
    // shadowed: the second mapping would never fire.
    let clash = configure_shortcut(
        &config,
        &two,
        Path::new("/soudan"),
        0,
        "ctrl+shift+backslash",
    )
    .unwrap_err()
    .to_string();
    assert!(clash.contains("already mapped"), "{clash}");
    configure_shortcut(&config, &two, Path::new("/soudan"), 0, "ctrl+shift+f9").unwrap();

    let updated = std::fs::read_to_string(&config).unwrap();
    assert_eq!(updated.matches("map ctrl+shift+backslash").count(), 1);
    assert_eq!(updated.matches("map ctrl+shift+f9").count(), 1);

    // Re-running setup must not move a workspace back to the default; the chosen
    // key is recorded precisely so a hand-picked one survives.
    assert_eq!(
        recorded_shortcut(&one).as_deref(),
        Some("ctrl+shift+backslash")
    );
    assert_eq!(recorded_shortcut(&two).as_deref(), Some("ctrl+shift+f9"));
    assert!(recorded_shortcut(dir.path()).is_none());

    // A record predating the stored key, such as one edited by hand to escape the
    // default, still has to survive an upgrade rather than snap back to it.
    let legacy = dir.path().join("legacy");
    std::fs::create_dir_all(legacy.join(".soudan")).unwrap();
    std::fs::write(
        legacy.join(".soudan/kitty-shortcut.json"),
        r#"{"config":"/k.conf","kitty_pid":1,"addition":"\n# Soudan bridge: /w\nmap ctrl+shift+backslash launch --type=background\n"}"#,
    )
    .unwrap();
    assert_eq!(
        recorded_shortcut(&legacy).as_deref(),
        Some("ctrl+shift+backslash")
    );

    // The key is written straight into kitty.conf, so it must not carry a second
    // directive of its own.
    for bad in [
        "ctrl+shift+a\nmap ctrl+q quit",
        "ctrl shift a",
        "",
        "Ctrl+Shift+A",
        "a+",
    ] {
        assert!(validate_shortcut(bad).is_err(), "accepted {bad:?}");
    }
    assert!(validate_shortcut("ctrl+shift+backslash").is_ok());
}

#[test]
fn an_unresolvable_target_says_which_directory_the_process_actually_runs_in() {
    use soudan::live::resolve_in;
    let dir = tempfile::tempdir().unwrap();
    let proc = dir.path().join("proc");
    let elsewhere = dir.path().join("other-project");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir(&proc).unwrap();
    fixture(&proc, 42, "/opt/claude/versions/2.1", &elsewhere, "52");

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
    fixture(&proc, 43, "/opt/claude/versions/2.1", dir.path(), "53");
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
    assert_eq!(ids, ["codex:42:12345", "codex:43:12345", "claude:44:12345"]);
    assert_eq!(targets[0].window_id, Some(53));
    assert_eq!(targets[1].window_id, None);
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

#[test]
fn the_terminal_transport_only_accepts_cursor_even_when_asked_directly() {
    let mut t = LiveTarget {
        id: "codex:42:12345".into(),
        agent: "codex".into(),
        pid: 42,
        start_time: 12345,
        window_id: Some(53),
        tty: "/dev/pts/99".into(),
    };
    // The bridge enforces Cursor-only routing itself, even for direct requests.
    // Claude Code and Codex must use their native inboxes regardless of window ID.
    assert!(soudan::live::window_match(&t).is_err());
    t.agent = "claude-code".into();
    assert!(soudan::live::window_match(&t).is_err());
    t.agent = "cursor".into();
    assert_eq!(soudan::live::window_match(&t).unwrap(), "id:53");
}
