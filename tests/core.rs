use soudan::{Config, Store, run_plugin};

#[test]
fn messages_survive_reopening_and_support_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rooms.db");
    let store = Store::open(&path).unwrap();
    let first = store.post("design", "codex", "Question?").unwrap();
    store.post("design", "claude-code", "Answer").unwrap();
    store.post("other", "cursor", "Unrelated").unwrap();
    drop(store);
    let messages = Store::open(&path)
        .unwrap()
        .history("design", first)
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].sender, "claude-code");
    assert_eq!(messages[0].text, "Answer");
}

#[test]
fn rejects_empty_or_oversized_messages() {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    assert!(store.post("", "codex", "hello").is_err());
    assert!(store.post("room", "", "hello").is_err());
    assert!(store.post("room", "codex", " ").is_err());
    assert!(store.post("room", "codex", &"x".repeat(65537)).is_err());
}

#[test]
fn config_extends_builtins_and_rejects_invalid_plugins() {
    let c = Config::parse("[agents.echo]\ncommand = 'cat'\nargs = []\ninput = 'stdin'\n").unwrap();
    assert!(c.agents.contains_key("claude-code"));
    assert!(c.agents.contains_key("codex"));
    assert!(c.agents.contains_key("cursor"));
    assert!(c.agents.contains_key("echo"));
    assert!(Config::parse("[agents.bad]\ncommand = ''\n").is_err());
}

#[tokio::test]
async fn plugin_passes_prompt_literally_and_reports_failures() {
    let config = Config::parse(
        "[agents.echo]\ncommand = 'cat'\ninput = 'stdin'\n[agents.fail]\ncommand = 'false'\n",
    )
    .unwrap();
    let prompt = "hello ' $(touch /tmp/soudan-should-never-exist)\nworld";
    let response = run_plugin(&config.agents["echo"], prompt, std::path::Path::new("."), 2)
        .await
        .unwrap();
    assert_eq!(response, prompt);
    assert!(
        run_plugin(&config.agents["fail"], "hi", std::path::Path::new("."), 2)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn plugin_times_out_and_limits_output() {
    let config = Config::parse("[agents.slow]\ncommand = 'sleep'\nargs = ['10']\ninput = 'none'\n[agents.large]\ncommand = 'yes'\ninput = 'none'\n").unwrap();
    let now = std::time::Instant::now();
    assert!(
        run_plugin(&config.agents["slow"], "hi", std::path::Path::new("."), 1)
            .await
            .is_err()
    );
    assert!(now.elapsed().as_secs() < 5);
    assert!(
        run_plugin(&config.agents["large"], "hi", std::path::Path::new("."), 2)
            .await
            .is_err()
    );
}

#[test]
fn jobs_have_durable_results_and_exclude_concurrent_room_writers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.db");
    let store = Store::open(&path).unwrap();
    let id = store.create_job("design", "echo", "question", 30).unwrap();
    assert!(store.create_job("design", "echo", "overlap", 30).is_err());
    assert_eq!(store.job(&id).unwrap().status, "queued");
    assert!(store.claim_job(&id).unwrap());
    assert!(!store.claim_job(&id).unwrap());
    store.finish_job(&id, Ok("answer")).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.job(&id).unwrap().result.as_deref(), Some("answer"));
    assert_eq!(
        store.history("design", 0).unwrap().last().unwrap().text,
        "answer"
    );
    assert!(store.create_job("design", "echo", "follow up", 30).is_ok());
}

#[tokio::test]
async fn json_plugins_reject_provider_errors_and_missing_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reply.json");
    let config = Config::parse(&format!(
        "[agents.json]\ncommand='cat'\nargs=['{}']\ninput='none'\noutput='result_json'\n",
        path.display()
    ))
    .unwrap();
    for value in [
        r#"{"is_error":true,"result":"quota exceeded"}"#,
        r#"{"message":"no result"}"#,
        "not json",
    ] {
        std::fs::write(&path, value).unwrap();
        assert!(
            run_plugin(&config.agents["json"], "hi", dir.path(), 2)
                .await
                .is_err()
        );
    }
    std::fs::write(&path, r#"{"result":"actual answer"}"#).unwrap();
    assert_eq!(
        run_plugin(&config.agents["json"], "hi", dir.path(), 2)
            .await
            .unwrap(),
        "actual answer"
    );
}

#[test]
fn job_capacity_is_shared_between_database_connections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.db");
    let a = Store::open(&path).unwrap();
    let b = Store::open(&path).unwrap();
    for n in 0..4 {
        a.create_job(&format!("room-{n}"), "echo", "hi", 30)
            .unwrap();
    }
    assert!(b.create_job("overflow", "echo", "hi", 30).is_err());
}

#[test]
fn interrupted_jobs_expire_instead_of_remaining_running_forever() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.db");
    let store = Store::open(&path).unwrap();
    let id = store.create_job("room", "echo", "hi", 1).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("UPDATE jobs SET deadline=0", [])
        .unwrap();
    assert_eq!(store.job(&id).unwrap().status, "failed");
    assert!(store.create_job("room", "echo", "retry", 1).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_terminates_descendant_processes() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("leaked");
    let script = dir.path().join("spawn.sh");
    std::fs::write(
        &script,
        format!("(sleep 2; touch '{}') &\nwait\n", marker.display()),
    )
    .unwrap();
    let config = Config::parse(&format!(
        "[agents.spawn]\ncommand='sh'\nargs=['{}']\ninput='none'\n",
        script.display()
    ))
    .unwrap();
    assert!(
        run_plugin(&config.agents["spawn"], "hi", dir.path(), 1)
            .await
            .is_err()
    );
    tokio::time::sleep(std::time::Duration::from_millis(1400)).await;
    assert!(
        !marker.exists(),
        "A descendant survived the consultation timeout"
    );
}

#[test]
fn pagination_catches_up_without_skipping_and_keeps_latest_context() {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    for n in 0..205 {
        store.post("room", "test", &format!("message-{n}")).unwrap();
    }
    let first = store.history("room", 0).unwrap();
    assert_eq!(first.len(), 100);
    let second = store.history("room", first.last().unwrap().id).unwrap();
    assert_eq!(second.len(), 100);
    let third = store.history("room", second.last().unwrap().id).unwrap();
    assert_eq!(third.len(), 5);
    assert_eq!(third.last().unwrap().text, "message-204");
    let context: serde_json::Value = serde_json::from_str(&store.context("room").unwrap()).unwrap();
    assert_eq!(context.as_array().unwrap().len(), 20);
    assert_eq!(context[0]["text"], "message-185");
}

#[cfg(unix)]
#[tokio::test]
async fn copilot_consultation_passes_literal_prompt_and_disables_tools() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("copilot-fixture.sh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
[ "$SOUDAN_CHILD" = 1 ] || exit 2
[ "$1" = --silent ] || exit 3
[ "$2" = --stream=off ] || exit 4
[ "$3" = --available-tools= ] || exit 5
[ "$4" = --disable-builtin-mcps ] || exit 6
[ "$5" = --no-ask-user ] || exit 7
[ "$6" = --no-auto-update ] || exit 8
[ "$7" = --no-custom-instructions ] || exit 9
[ "$8" = --prompt ] || exit 10
[ "$#" = 9 ] || exit 11
printf '%s' "$9"
"#,
    )
    .unwrap();
    let mut plugin = Config::default().agents["copilot"].clone();
    plugin.command = "sh".into();
    plugin.args.insert(0, script.to_str().unwrap().into());
    let prompt = "日本語 ' $(false)\nsecond line";
    assert_eq!(
        run_plugin(&plugin, prompt, dir.path(), 2).await.unwrap(),
        prompt
    );
}
