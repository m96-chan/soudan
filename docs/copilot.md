# GitHub Copilot CLI

Soudan supports GitHub Copilot CLI as a consultation agent and MCP client.
The adapter targets the standalone `copilot` executable, not the legacy
`gh copilot` extension or the VS Code Copilot chat UI. CLI flags were checked
against locally installed version 1.0.83 on Linux on 2026-09-11.

## Setup

Install and authenticate Copilot CLI, then run in the desired workspace:

```sh
soudan doctor
soudan install --client copilot
soudan consult --agent copilot --room review --wait 'Review this design.'
```

`doctor` checks executable availability, not account authentication.
`install --client all` also configures Copilot. The project `.mcp.json` is shared
with Claude Code; installing either client preserves other servers and existing
Soudan options, including an explicit `tools` filter. Changed files are backed up.
Trust the workspace in Copilot, reload its MCP configuration, and approve tool
calls when prompted. No user-level Copilot configuration is installed.

## Consultation behavior

Each consultation launches a new CLI process in the selected workspace. The
question and recent room history are passed as one literal `--prompt` argument.
Soudan uses `--silent --stream=off` to receive plain response text, and the
existing plugin runner handles failures, timeouts, output limits, and process
cleanup. Authentication is inherited from the CLI environment.

The default uses `--available-tools=` to expose no tools, disables built-in MCP
servers and custom instructions, and disables interactive questions and automatic
updates. It does not grant blanket tool permissions. These flags are provider
settings, not an operating-system sandbox; the CLI still reads its configuration
and writes its own session state. The prompt may be visible in local process
arguments. Do not include credentials in consultation prompts.

To customize the model, copy the complete `[agents.copilot]` definition from
[the plugin contract](plugins.md) to `soudan.toml` and add the supported `--model`
argument before `--prompt`. An override replaces the entire built-in definition.
No model or reasoning effort is selected by Soudan.

## Existing chats

Start the Copilot TUI with its embedded server in the workspace to share:

```sh
copilot --ui-server --host 127.0.0.1 --port 4097
```

Then, from another terminal or an MCP client in that same workspace:

```sh
soudan live list
soudan live send <TARGET_ID> --request-id review-1 --sender codex 'Please review this design.'
soudan live delivery review-1
soudan live read <TARGET_ID>
```

Copilot targets have the form `copilot:<pid>:<start_time>:<port>:<session_uuid>`.
Discovery checks the executable, process start time, exact working directory,
and ownership of the IPv4 loopback listener. It asks `session.getForeground` for
the currently displayed session and checks the saved session ID and workspace.
Historical sessions and normal TUI processes without an embedded server are not
listed. Restart such a TUI with `--ui-server` yourself; Soudan does not restart it.
No fixed port is required; `--port 0` also works with automatic discovery.

The adapter speaks the SDK's Content-Length framed JSON-RPC protocol 3, **not
ACP**. It binds the connection with `session.resume` only after verifying that
exact session is already foreground, without supplying model, tools, permissions,
or client handlers. It rechecks foreground and process identity before
`session.send`, with `mode: "enqueue"`. The TUI retains tool approvals and its
composer. Closing Soudan's socket does not abort or destroy the session. It does
not create a separate headless session, switch the displayed session, or submit
any existing draft. A switched foreground is refused before sending; rediscover
and deliberately choose the new target.

`--ui-server` is currently hidden from CLI help, although foreground APIs are
part of the official SDK. This integration is experimental and was verified
against Copilot CLI 1.0.83 on Linux. Sent prompts and replies were observed in
the TUI, including a round trip through Soudan's commands while an unsent draft
remained in the composer.

If the server uses `COPILOT_CONNECTION_TOKEN`, explicitly provide the same
variable to the Soudan process (and its MCP server environment). Soudan never
extracts authentication tokens from another process or changes its policy.
Authentication errors are not retried with another identity. Bind to loopback;
without a connection token the Copilot server accepts local clients.

### Delivery and receipts

Before sending, Soudan commits the request identity and event-log boundary. On
acknowledgement it atomically stores the returned message ID and `queued` status.
Repeated calls with that request ID report the saved result, and changing its
text, target, or declared sender is rejected. A failure before `session.send`
is `not_delivered`; any failed/ambiguous send is `uncertain` and cannot be retried
automatically. An acknowledgement lost before its message ID is persisted leaves
receipt status `unknown`, even if the message later appears in the TUI.

`receipt.status = taken` requires a complete root `user.message` event with both
the acknowledged message ID and exact prompt after the saved boundary. Quoted
text in an assistant response is insufficient. Device, inode, anchor, and
truncation checks protect the observation; missing or changed evidence reports
`unknown`. An acknowledged message not yet observed is `waiting` while the
original process exists. A covered positive receipt remains available after
exit. Receipts prove admission, not an assistant reply.

`live read` reads complete persisted events without opening a new RPC session
or changing Copilot state. It returns the latest root assistant message,
message/interaction IDs, and observed turn state; child-agent and system messages
are excluded. `reply_scope: session_latest` explicitly does not correlate that
reply to a particular Soudan request. Check its content or agree to post a
correlated reply in a Soudan room. Recorded state can lag in-flight work; `idle`
describes the last recorded root turn, not all background activity.

RPC observations have an eight-second deadline, a 16 MiB total response budget,
and a 4 KiB header limit. Discovery probes at most four owned loopback listeners
per Copilot process. State reading and receipt scans also have a 16 MiB limit;
large, unreadable, or partially replaced logs return an error/unknown rather
than claiming complete coverage. Live discovery currently requires Linux.

## References

- [CLI command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference)
- [Project MCP configuration](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers)
- [ACP server](https://docs.github.com/en/copilot/reference/copilot-cli-reference/acp-server)
- [Official SDK foreground-session APIs](https://github.com/github/copilot-sdk/blob/main/nodejs/README.md)
