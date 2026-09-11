#![cfg(target_os = "linux")]
use soudan::receipt::{Basis, observe};
use std::{fs, io::Write};

#[test]
fn receipt_requires_complete_evidence_and_original_process_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("rollout.jsonl");
    let proc = tmp.path().join("proc");
    fs::create_dir_all(proc.join("42")).unwrap();
    let stat = format!("42 (codex) {} 123", vec!["0"; 19].join(" "));
    fs::write(proc.join("42/stat"), &stat).unwrap();
    fs::write(
        &log,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
    )
    .unwrap();
    let basis = Basis::capture(&log).unwrap();
    let check = || observe(Some(&basis), &proc, "codex:42:123", "request-1");
    assert_eq!(check()["status"], "waiting");
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\"}}\n")
        .unwrap();
    assert_eq!(check()["status"], "blocked");
    // Reproduce the abandoned queue item after a replacement process resumes.
    fs::write(proc.join("42/stat"), stat.replace("123", "124")).unwrap();
    assert_eq!(check()["status"], "lost");
    // The marker may be beyond the old 256 KiB state window.
    let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
    file.write_all(b"[Soudan request-1]\n").unwrap();
    file.write_all(&vec![b'x'; 300_000]).unwrap();
    assert_eq!(check()["status"], "taken");
    assert_eq!(
        observe(None, &proc, "codex:42:123", "request-1")["status"],
        "unknown"
    );
    fs::write(&log, "truncated").unwrap();
    assert_eq!(check()["status"], "unknown");
}

#[test]
fn discarded_claude_envelope_is_not_a_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("transcript.jsonl");
    fs::write(&log, "previous transcript\n").unwrap();
    let basis = Basis::capture(&log).unwrap();
    // Socket write succeeded but the receiver never logged the wrong envelope.
    assert_eq!(
        observe(
            Some(&basis),
            tmp.path(),
            "claude:42:123",
            "claude-native-proof-1"
        )["status"],
        "lost"
    );
    fs::rename(&log, tmp.path().join("old")).unwrap();
    fs::write(&log, "previous transcript\n").unwrap();
    assert_eq!(
        observe(
            Some(&basis),
            tmp.path(),
            "claude:42:123",
            "claude-native-proof-1"
        )["status"],
        "unknown"
    );
    assert_eq!(
        observe(Some(&basis), tmp.path(), "kitty:42:123", "x")["status"],
        "unknown"
    );
}

#[test]
fn malformed_process_state_and_changed_anchor_are_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("42")).unwrap();
    fs::write(tmp.path().join("42/stat"), "unreadable stat format").unwrap();
    let log = tmp.path().join("log");
    fs::write(&log, "original bytes").unwrap();
    let basis = Basis::capture(&log).unwrap();
    assert_eq!(
        observe(Some(&basis), tmp.path(), "codex:42:123", "x")["status"],
        "unknown"
    );
    fs::remove_dir_all(tmp.path().join("42")).unwrap();
    fs::write(&log, "replacement bytes and regrowth").unwrap();
    assert_eq!(
        observe(Some(&basis), tmp.path(), "codex:42:123", "x")["status"],
        "unknown"
    );
}

#[test]
fn migration_preserves_old_rows_and_receipts_never_update_sender_status() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let db = rusqlite::Connection::open(tmp.path().join(".soudan/state.db")).unwrap();
    db.execute_batch("CREATE TABLE live_deliveries(request_id TEXT PRIMARY KEY,target TEXT NOT NULL,text TEXT NOT NULL,status TEXT NOT NULL,before_screen TEXT NOT NULL,error TEXT); INSERT INTO live_deliveries VALUES('old','codex:42:123','hello','queued','before',NULL);").unwrap();
    for _ in 0..2 {
        let row = soudan::live::delivery_in(tmp.path(), "old", tmp.path()).unwrap();
        assert_eq!(row["status"], "queued");
        assert_eq!(row["receipt"]["status"], "unknown");
    }
    let log = tmp.path().join("log");
    fs::write(&log, "before").unwrap();
    let basis = serde_json::to_string(&Basis::capture(&log).unwrap()).unwrap();
    db.execute("UPDATE live_deliveries SET receipt_basis=?1", [&basis])
        .unwrap();
    assert_eq!(
        soudan::live::delivery_in(tmp.path(), "old", tmp.path()).unwrap()["receipt"]["status"],
        "lost"
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"[Soudan old]")
        .unwrap();
    let row = soudan::live::delivery_in(tmp.path(), "old", tmp.path()).unwrap();
    assert_eq!(row["receipt"]["status"], "taken");
    assert_eq!(row["status"], "queued");
    let saved: (String, String) = db
        .query_row(
            "SELECT status,before_screen FROM live_deliveries",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(saved, ("queued".into(), "before".into()));
}

#[test]
fn only_post_send_markers_count_even_across_scan_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("log");
    fs::write(&log, "[Soudan boundary]\n").unwrap();
    let basis = Basis::capture(&log).unwrap();
    assert_eq!(
        observe(Some(&basis), tmp.path(), "codex:42:123", "boundary")["status"],
        "lost"
    );
    let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
    file.write_all(&vec![b'x'; 65530]).unwrap();
    file.write_all(b"[Soudan boundary]").unwrap();
    assert_eq!(
        observe(Some(&basis), tmp.path(), "codex:42:123", "boundary")["status"],
        "taken"
    );
}
