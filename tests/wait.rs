use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};
fn run(workspace: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_soudan"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .output()
        .unwrap()
}
fn value(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn room_wait_catches_the_history_to_wait_gap_without_consuming_messages() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let store = soudan::Store::open(&tmp.path().join(".soudan/state.db")).unwrap();
    let first = store.post("room", "one", "old").unwrap();
    let history = store.history("room", 0).unwrap();
    assert_eq!(history.last().unwrap().id, first);
    // Message lands after history was read but before the watcher is started.
    let second = store.post("room", "two", "arrived in gap").unwrap();
    store.post("other", "two", "not this room").unwrap();
    for _ in 0..2 {
        let out = run(
            tmp.path(),
            &[
                "wait",
                "--room",
                "room",
                "--after",
                &first.to_string(),
                "--timeout",
                "2",
            ],
        );
        assert_eq!(out.status.code(), Some(0), "{:?}", out);
        let v = value(&out);
        assert_eq!(v["outcome"], "event");
        assert_eq!(v["last_id"], second);
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
    }
    assert_eq!(store.history("room", 0).unwrap().len(), 2);
}
#[test]
fn timeout_is_bounded_json_and_does_not_initialize_a_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let out = run(
        tmp.path(),
        &["wait", "--room", "empty", "--after", "0", "--timeout", "1"],
    );
    assert_eq!(out.status.code(), Some(124), "{:?}", out);
    assert_eq!(value(&out)["outcome"], "timeout");
    assert!(start.elapsed() < Duration::from_secs(4));
    assert!(!tmp.path().join(".soudan").exists());
    let out = run(
        tmp.path(),
        &[
            "wait",
            "--room",
            "empty",
            "--after",
            "0",
            "--timeout",
            "3601",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(value(&out)["outcome"], "error");
}
#[test]
fn legacy_receipt_returns_unknown_without_migrating_or_changing_rows() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let db = rusqlite::Connection::open(tmp.path().join(".soudan/state.db")).unwrap();
    db.execute_batch("CREATE TABLE live_deliveries(request_id TEXT PRIMARY KEY,target TEXT,text TEXT,status TEXT,before_screen TEXT,error TEXT); INSERT INTO live_deliveries VALUES('old','claude:42:123','hello','submitted','',NULL);").unwrap();
    let before = fs::read(tmp.path().join(".soudan/state.db")).unwrap();
    let out = run(
        tmp.path(),
        &["live", "wait", "--request-id", "old", "--timeout", "1"],
    );
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    assert_eq!(value(&out)["delivery"]["receipt"]["status"], "unknown");
    assert_eq!(
        before,
        fs::read(tmp.path().join(".soudan/state.db")).unwrap()
    );
}

#[test]
fn room_wait_observes_a_later_wal_commit_without_holding_a_transaction() {
    use std::process::Stdio;
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let store = soudan::Store::open(&tmp.path().join(".soudan/state.db")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_soudan"))
        .arg("--workspace")
        .arg(tmp.path())
        .args(["wait", "--room", "r", "--after", "0", "--timeout", "4"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let id = store.post("r", "sender", "new WAL commit").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(value(&out)["last_id"], id);
}

#[test]
fn locked_database_wait_has_a_deadline_and_missing_delivery_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let db = rusqlite::Connection::open(tmp.path().join(".soudan/state.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE messages(id INTEGER,room TEXT,sender TEXT,text TEXT); BEGIN EXCLUSIVE;",
    )
    .unwrap();
    let start = Instant::now();
    let out = run(
        tmp.path(),
        &["wait", "--room", "r", "--after", "0", "--timeout", "1"],
    );
    assert_eq!(out.status.code(), Some(124), "{:?}", out);
    assert!(start.elapsed() < Duration::from_secs(4));
    db.execute_batch("ROLLBACK").unwrap();
    let out = run(
        tmp.path(),
        &["live", "wait", "--request-id", "absent", "--timeout", "1"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(value(&out)["outcome"], "error");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn receipt_wait_observes_fake_process_states_and_a_later_marker() {
    use soudan::{
        receipt::Basis,
        wait::{Watch, observe_in},
    };
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let proc = tmp.path().join("proc");
    fs::create_dir_all(proc.join("42")).unwrap();
    fs::write(
        proc.join("42/stat"),
        format!("42 (codex) {} 123", vec!["0"; 19].join(" ")),
    )
    .unwrap();
    let log = tmp.path().join("log");
    fs::write(
        &log,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
    )
    .unwrap();
    let basis = serde_json::to_string(&Basis::capture(&log).unwrap()).unwrap();
    let db = rusqlite::Connection::open(tmp.path().join(".soudan/state.db")).unwrap();
    db.execute_batch("CREATE TABLE live_deliveries(request_id TEXT PRIMARY KEY,target TEXT,text TEXT,status TEXT,before_screen TEXT,error TEXT,receipt_basis TEXT);").unwrap();
    db.execute(
        "INSERT INTO live_deliveries VALUES('r','codex:42:123','hello','queued','',NULL,?1)",
        [basis],
    )
    .unwrap();
    let watch = Watch::Delivery {
        request_id: "r".into(),
    };
    assert_eq!(
        observe_in(tmp.path(), &watch, 1, &proc).await.unwrap()["outcome"],
        "timeout"
    );
    let writer = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .unwrap()
            .write_all(b"[Soudan r]\n")
            .unwrap();
    };
    let (event, ()) = tokio::join!(observe_in(tmp.path(), &watch, 3, &proc), writer);
    assert_eq!(event.unwrap()["delivery"]["receipt"]["status"], "taken");
    // A fresh request without a marker should stop immediately on aborted/lost.
    db.execute("UPDATE live_deliveries SET request_id='blocked'", [])
        .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\"}}\n")
        .unwrap();
    let watch = Watch::Delivery {
        request_id: "blocked".into(),
    };
    assert_eq!(
        observe_in(tmp.path(), &watch, 1, &proc).await.unwrap()["delivery"]["receipt"]["status"],
        "blocked"
    );
    fs::remove_dir_all(proc.join("42")).unwrap();
    assert_eq!(
        observe_in(tmp.path(), &watch, 1, &proc).await.unwrap()["delivery"]["receipt"]["status"],
        "lost"
    );
}

#[cfg(unix)]
#[test]
fn supervisor_times_out_even_when_receipt_log_open_blocks() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".soudan")).unwrap();
    let log = tmp.path().join("log");
    fs::write(&log, "before").unwrap();
    let basis = serde_json::to_string(&soudan::receipt::Basis::capture(&log).unwrap()).unwrap();
    fs::remove_file(&log).unwrap();
    let path = std::ffi::CString::new(log.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a valid NUL-terminated name inside this test's tempdir.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let db = rusqlite::Connection::open(tmp.path().join(".soudan/state.db")).unwrap();
    db.execute_batch("CREATE TABLE live_deliveries(request_id TEXT PRIMARY KEY,target TEXT,text TEXT,status TEXT,before_screen TEXT,error TEXT,receipt_basis TEXT);").unwrap();
    db.execute(
        "INSERT INTO live_deliveries VALUES('r','codex:42:123','hello','queued','',NULL,?1)",
        [basis],
    )
    .unwrap();
    let start = Instant::now();
    let out = run(
        tmp.path(),
        &["live", "wait", "--request-id", "r", "--timeout", "1"],
    );
    assert_eq!(out.status.code(), Some(124), "{:?}", out);
    assert_eq!(value(&out)["outcome"], "timeout");
    assert!(start.elapsed() < Duration::from_secs(4));
}
