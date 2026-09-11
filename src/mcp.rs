use crate::app::App;
use rmcp::{ErrorData, RoleServer, ServerHandler, model::*, service::RequestContext};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub struct Mcp {
    pub app: Arc<App>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Consult {
    request_id: Option<String>,
    agent: String,
    prompt: String,
    room: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}
fn default_timeout() -> u64 {
    180
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultArgs {
    job_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Post {
    in_reply_to: Option<String>,
    request_id: Option<String>,
    room: String,
    sender: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    room: String,
    #[serde(default)]
    after: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveDelivery {
    request_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveRead {
    target: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveSend {
    sender: Option<String>,
    target: String,
    text: String,
    request_id: String,
}
impl Mcp {
    fn definitions() -> Vec<Tool> {
        let string = json!({"type":"string","minLength":1});
        [
            ("soudan_live_targets", "Find existing interactive agent sessions in this workspace. These are open chats, not new headless sessions. Each is reached through its own session API: Codex through its queue, Claude Code through its registered inbox socket. Sessions running in another directory are not listed, because discovery matches the working directory exactly.", json!({}), vec![]),
            ("soudan_live_delivery", "Inspect a saved live delivery and derive receipt.status: taken, waiting, blocked, lost, or unknown. Sender status remains unchanged. Receipt is not proof of a reply. Never automatically resend based on this observation.", json!({"request_id":string}), vec!["request_id"]),
            ("soudan_live_read", "Read an existing chat's current state. Codex and Claude Code return session state and the most recent reply text. State is read from the session's own log, so it reports what that agent recorded, not a live screen.", json!({"target":string}), vec!["target"]),
            ("soudan_live_send", "Submit a message directly into an existing interactive chat. Only use when the user has authorized messaging that session. Optional sender is an unverified caller label, never a permission assertion. Requires a unique request_id; retries never resubmit uncertain deliveries. Codex and Claude Code both accept a message mid-turn, so neither has to be idle; Claude inbound policy may still hold or refuse it. The result carries a receipt observation, not an acknowledgement. Read the target, or wait on the agreed room, to see the reply. Do not create automatic reply loops.", json!({"target":string,"text":string,"request_id":string,"sender":string}), vec!["target","text","request_id"]),
            ("soudan_agents", "List agent plugins and local executable availability. Availability does not verify authentication.", json!({}), vec![]),
            ("soudan_consult", "Start an AI consultation and return a job ID immediately. Poll soudan_result for the answer. Reuse room for follow-up questions or invite another agent to the same discussion. Consultations use fresh headless CLI sessions with the recent room transcript.", json!({"agent":string,"prompt":{"type":"string","minLength":1,"maxLength":65536},"room":string,"request_id":string,"timeout_seconds":{"type":"integer","minimum":1,"maximum":600,"default":180}}), vec!["agent","prompt"]),
            ("soudan_result", "Read a consultation job. queued/running means wait briefly and poll again; completed includes result; failed includes error. Jobs survive MCP server disconnection.", json!({"job_id":string}), vec!["job_id"]),
            ("soudan_post", "Post a message from your current interactive session to a shared room. Set in_reply_to to the incoming Soudan request ID when replying. Posts do not live-send to other editors; participants can wait with soudan wait --reply-to or read with soudan_history.", json!({"room":string,"sender":string,"text":string,"request_id":string,"in_reply_to":string}), vec!["room","sender","text"]),
            ("soudan_history", "Read up to 100 room messages in ID order. Pass the last message ID as after to read the next page or poll for replies.", json!({"room":string,"after":{"type":"integer","minimum":0,"default":0}}), vec!["room"]),
        ].into_iter().map(|(name,desc,props,required)|Tool::new(name,desc,json!({"type":"object","properties":props,"required":required,"additionalProperties":false}).as_object().unwrap().clone())).collect()
    }
    async fn dispatch(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        match name {
            "soudan_live_targets" => {
                anyhow::ensure!(
                    args.as_object().is_some_and(|a| a.is_empty()),
                    "No arguments expected"
                );
                Ok(serde_json::to_value(crate::live::discover(
                    &self.app.workspace,
                )?)?)
            }
            "soudan_live_delivery" => {
                let p: LiveDelivery = serde_json::from_value(args)?;
                crate::live::delivery(&self.app.workspace, &p.request_id)
            }
            "soudan_live_read" => {
                let p: LiveRead = serde_json::from_value(args)?;
                crate::live::read(&self.app.workspace, &p.target).await
            }
            "soudan_live_send" => {
                let p: LiveSend = serde_json::from_value(args)?;
                crate::live::send_as(
                    &self.app.workspace,
                    &p.target,
                    &p.text,
                    &p.request_id,
                    p.sender.as_deref(),
                )
                .await
            }
            "soudan_agents" => {
                anyhow::ensure!(
                    args.as_object().is_some_and(|a| a.is_empty()),
                    "No arguments expected"
                );
                Ok(self.app.agents())
            }
            "soudan_consult" => {
                let p: Consult = serde_json::from_value(args)?;
                self.app.start(
                    &p.agent,
                    &p.prompt,
                    p.room.as_deref(),
                    p.timeout_seconds,
                    p.request_id.as_deref(),
                )
            }
            "soudan_result" => {
                let p: ResultArgs = serde_json::from_value(args)?;
                Ok(serde_json::to_value(self.app.store.job(&p.job_id)?)?)
            }
            "soudan_post" => {
                let p: Post = serde_json::from_value(args)?;
                Ok(
                    json!({"id":self.app.store.post_reply(&p.room,&p.sender,&p.text,p.request_id.as_deref(),p.in_reply_to.as_deref())?}),
                )
            }
            "soudan_history" => {
                let p: History = serde_json::from_value(args)?;
                anyhow::ensure!(p.after >= 0, "after must be nonnegative");
                Ok(serde_json::to_value(
                    self.app.store.history(&p.room, p.after)?,
                )?)
            }
            _ => anyhow::bail!("Unknown tool: {name}"),
        }
    }
}
impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("soudan", env!("CARGO_PKG_VERSION")))
            .with_instructions("Use soudan_agents to discover agents. Start with soudan_consult, poll soudan_result, and reuse its room for dialogue. For existing editor sessions, exchange messages using soudan_post and soudan_history. Never recursively consult from a consultation worker.")
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: Self::definitions(),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let result = self
            .dispatch(
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
            )
            .await;
        Ok(match result {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]),
        }
        .into())
    }
}
