use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
struct Client {
    child: Child,
    input: ChildStdin,
    output: tokio::io::Lines<BufReader<ChildStdout>>,
}
impl Client {
    async fn start(path: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_soudan"))
            .arg("--workspace")
            .arg(path)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut c = Self {
            child,
            input,
            output,
        };
        let init=c.rpc(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
        assert_eq!(init["result"]["serverInfo"]["name"], "soudan");
        c.input
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        c
    }
    async fn rpc(&mut self, v: Value) -> Value {
        self.input
            .write_all(format!("{v}\n").as_bytes())
            .await
            .unwrap();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(10), self.output.next_line())
                .await
                .unwrap()
                .unwrap()
                .expect("MCP stdout closed");
            let response: Value = serde_json::from_str(&line).unwrap();
            if response.get("id") == v.get("id") {
                return response;
            }
        }
    }
    async fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":args}})).await
    }
}
fn payload(v: Value) -> Value {
    assert_ne!(v["result"]["isError"], true, "{v}");
    serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}
#[tokio::test]
async fn mcp_clients_share_rooms_and_jobs_survive_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("soudan.toml"),
        "[agents.echo]\ncommand='sh'\nargs=['-c','sleep 0.5; cat']\ninput='stdin'\n",
    )
    .unwrap();
    let mut a = Client::start(dir.path()).await;
    let tools = a
        .rpc(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))
        .await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 9);
    let posted = payload(
        a.call(
            "soudan_post",
            json!({"room":"design","sender":"cursor","text":"What about plugins?"}),
        )
        .await,
    );
    let started = payload(
        a.call(
            "soudan_consult",
            json!({"agent":"echo","room":"design","prompt":"Review the previous suggestion."}),
        )
        .await,
    );
    let pending = payload(
        a.call("soudan_result", json!({"job_id":started["job_id"]}))
            .await,
    );
    assert!(
        pending["status"] == "queued" || pending["status"] == "running",
        "{pending}"
    );
    a.child.kill().await.unwrap();
    let mut b = Client::start(dir.path()).await;
    let mut completed = None;
    for _ in 0..40 {
        let job = payload(
            b.call("soudan_result", json!({"job_id":started["job_id"]}))
                .await,
        );
        if job["status"] == "completed" {
            completed = Some(job);
            break;
        }
        assert_ne!(job["status"], "failed", "{job}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let job = completed.expect("worker did not complete");
    assert!(
        job["result"]
            .as_str()
            .unwrap()
            .contains("What about plugins?")
    );
    let history = payload(
        b.call(
            "soudan_history",
            json!({"room":"design","after":posted["id"]}),
        )
        .await,
    );
    assert_eq!(history.as_array().unwrap().len(), 2);
    assert_eq!(history[1]["sender"], "echo");
    let error = b
        .call("soudan_consult", json!({"agent":"missing","prompt":"Hi"}))
        .await;
    assert_eq!(error["result"]["isError"], true);
    let error = b
        .call(
            "soudan_post",
            json!({"room":"design","sender":"test","text":""}),
        )
        .await;
    assert_eq!(error["result"]["isError"], true);
    let error = b
        .call(
            "soudan_consult",
            json!({"agent":"echo","prompt":"hi","timeout_seconds":0}),
        )
        .await;
    assert_eq!(error["result"]["isError"], true);
}

#[tokio::test]
async fn retrying_a_consultation_with_same_request_id_returns_original_job() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("soudan.toml"),
        "[agents.echo]\ncommand='cat'\ninput='stdin'\n",
    )
    .unwrap();
    let mut client = Client::start(dir.path()).await;
    let args = json!({"agent":"echo","prompt":"hello","room":"retry","request_id":"request-1"});
    let first = payload(client.call("soudan_consult", args.clone()).await);
    let second = payload(client.call("soudan_consult", args).await);
    assert_eq!(first["job_id"], second["job_id"]);
    let different = client
        .call(
            "soudan_consult",
            json!({"agent":"echo","prompt":"different","room":"retry","request_id":"request-1"}),
        )
        .await;
    assert_eq!(different["result"]["isError"], true);
    let args = json!({"room":"posts","sender":"cursor","text":"hello","request_id":"post-1"});
    let first = payload(client.call("soudan_post", args.clone()).await);
    let second = payload(client.call("soudan_post", args).await);
    assert_eq!(first["id"], second["id"]);
    let history = payload(client.call("soudan_history", json!({"room":"posts"})).await);
    assert_eq!(history.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn live_tools_are_discoverable_and_report_offline_bridge_as_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::start(dir.path()).await;
    let tools = client
        .rpc(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}))
        .await;
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for name in [
        "soudan_live_targets",
        "soudan_live_delivery",
        "soudan_live_read",
        "soudan_live_send",
    ] {
        assert!(names.contains(&name));
    }
    let db = rusqlite::Connection::open(dir.path().join(".soudan/state.db")).unwrap();
    db.execute_batch("CREATE TABLE live_deliveries(request_id TEXT PRIMARY KEY,target TEXT NOT NULL,text TEXT NOT NULL,status TEXT NOT NULL,before_screen TEXT NOT NULL,error TEXT); INSERT INTO live_deliveries VALUES('legacy','codex:42:123','hello','queued','',NULL);").unwrap();
    let receipt = payload(
        client
            .call("soudan_live_delivery", json!({"request_id":"legacy"}))
            .await,
    );
    assert_eq!(receipt["status"], "queued");
    assert_eq!(receipt["receipt"]["status"], "unknown");
    let targets = payload(client.call("soudan_live_targets", json!({})).await);
    assert!(targets.as_array().unwrap().is_empty());
    // A transport Soudan does not serve is refused by name rather than attempted.
    let response = client
        .call(
            "soudan_live_send",
            json!({"target":"kitty:1:1","text":"hello","request_id":"test"}),
        )
        .await;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("kitty"), "{text}");
}
