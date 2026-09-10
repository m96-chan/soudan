#![cfg(target_os = "linux")]
use soudan::codex::{Session, session, state};
use std::{os::unix::fs::symlink, path::Path};

const THREAD: &str = "01a08b4c-411f-7e70-a0d7-66b89fdb59c7";

/// A running Codex holds its thread lock and rollout open, whether or not it was resumed.
fn fixture(proc: &Path, pid: u32, home: &Path, links: &[&str]) {
    let fd = proc.join(pid.to_string()).join("fd");
    std::fs::create_dir_all(&fd).unwrap();
    for (index, link) in links.iter().enumerate() {
        symlink(home.join(link), fd.join(index.to_string())).unwrap();
    }
}

#[test]
fn thread_identity_comes_from_open_files_not_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let proc = home.join("proc");
    let rollout = format!("sessions/2026/09/10/rollout-2026-09-10T21-30-25-{THREAD}.jsonl");
    std::fs::create_dir_all(home.join("sessions/2026/09/10")).unwrap();
    std::fs::write(home.join(&rollout), "").unwrap();

    fixture(
        &proc,
        42,
        home,
        &[
            "codex/logs_2.sqlite",
            ".coordination.lock",
            &format!("thread-writer-locks/{THREAD}.lock"),
            &rollout,
        ],
    );
    assert_eq!(
        session(&proc, 42).unwrap(),
        Some(Session {
            thread: THREAD.into(),
            rollout: home.join(&rollout),
        })
    );

    // Without the pair there is no thread to address, so the caller falls back.
    fixture(&proc, 43, home, &["codex/logs_2.sqlite"]);
    assert_eq!(session(&proc, 43).unwrap(), None);
    fixture(
        &proc,
        44,
        home,
        &[&format!("thread-writer-locks/{THREAD}.lock")],
    );
    assert_eq!(session(&proc, 44).unwrap(), None);
    assert_eq!(session(&proc, 45).unwrap(), None);
}

#[test]
fn thread_state_reports_the_last_reply_and_whether_a_turn_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let started = r#"{"type":"event_msg","payload":{"type":"task_started"}}"#;
    let complete =
        r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"done"}}"#;
    let noise = r#"{"type":"response_item","payload":{"type":"message"}}"#;

    std::fs::write(&path, format!("{started}\n{noise}\n{complete}\n")).unwrap();
    let idle = state(&path).unwrap();
    assert_eq!(idle["status"], "idle");
    assert_eq!(idle["last_agent_message"], "done");

    // A turn that has not finished must not look answered.
    std::fs::write(&path, format!("{complete}\nnot json\n{started}\n")).unwrap();
    let running = state(&path).unwrap();
    assert_eq!(running["status"], "running");
    assert_eq!(running["last_agent_message"], "done");

    // No state event in range is not evidence that the session is free.
    std::fs::write(&path, "").unwrap();
    let empty = state(&path).unwrap();
    assert_eq!(empty["status"], "unknown");
    assert!(empty["last_agent_message"].is_null());

    let bulk = format!("{}\n", r#"{"type":"response_item","payload":{}}"#).repeat(8000);
    std::fs::write(&path, format!("{started}\n{bulk}")).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > 262144);
    assert_eq!(state(&path).unwrap()["status"], "unknown");
}

#[test]
fn an_ambiguous_process_is_refused_rather_than_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let proc = home.join("proc");
    let other = "01a067d4-d7a6-7b41-9b9e-bc55bc0c8b75";
    let rollout = format!("sessions/2026/09/10/rollout-2026-09-10T21-30-25-{THREAD}.jsonl");
    std::fs::create_dir_all(home.join("sessions/2026/09/10")).unwrap();
    std::fs::write(home.join(&rollout), "").unwrap();

    // Two held locks give no basis for choosing a destination.
    fixture(
        &proc,
        42,
        home,
        &[
            &format!("thread-writer-locks/{THREAD}.lock"),
            &format!("thread-writer-locks/{other}.lock"),
            &rollout,
        ],
    );
    assert!(session(&proc, 42).is_err());

    // A directory named after the thread does not make a rollout that thread's.
    let mismatched = format!("{THREAD}/sessions/rollout-2026-09-10T21-30-25-{other}.jsonl");
    std::fs::create_dir_all(home.join(THREAD).join("sessions")).unwrap();
    std::fs::write(home.join(&mismatched), "").unwrap();
    fixture(
        &proc,
        43,
        home,
        &[&format!("thread-writer-locks/{THREAD}.lock"), &mismatched],
    );
    assert_eq!(session(&proc, 43).unwrap(), None);

    // A lock outside the lock directory is not a thread lock.
    fixture(&proc, 44, home, &[&format!("{THREAD}.lock"), &rollout]);
    assert_eq!(session(&proc, 44).unwrap(), None);
}

#[tokio::test]
async fn a_command_that_never_ran_stays_retryable() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: the test binary is single-threaded here and restores nothing it needs.
    unsafe { std::env::set_var("PATH", dir.path()) };
    let failure = soudan::codex::queue(THREAD, "hello").await.unwrap_err();
    assert!(
        matches!(failure, soudan::codex::Failure::NotAttempted(_)),
        "a missing binary delivered nothing, so the request id must stay reusable"
    );
}
