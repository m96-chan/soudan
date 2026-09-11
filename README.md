# soudan

**Let your AI agents consult each other.** Soudan (相談, “consultation” in Japanese) is a Rust MCP server for Claude Code, Codex, and Cursor, with executable plugins for future agents.

Start a consultation from any MCP client, get a job ID immediately, and read the answer when it is ready. Reuse the room to ask follow-up questions or bring another agent into the discussion. Existing editor sessions can also exchange messages through the same room.

```text
Claude Code ─┐                        ┌─ Claude Code CLI
Codex ──────┼─ MCP stdio ─ soudan ────┼─ Codex CLI
Cursor ─────┘             │          ├─ Cursor Agent CLI
                          │          └─ Your executable plugin
                          └─ SQLite rooms and consultation jobs
```

## Quick start

Requires Rust 1.98+, and an installed, authenticated CLI for each agent you want to consult. Linux is locally verified; CI also targets macOS. Windows support is not yet validated.

```sh
git clone https://github.com/m96-chan/soudan.git
cd soudan
cargo install --path . --locked

# Run inside the project whose agents should share conversations.
soudan install --client all
soudan doctor
```

Soudan is not yet published to crates.io. Install from this checkout; `cargo install soudan` is not an available release workflow yet.

`install` writes project-scoped configuration for all three clients, using the absolute installed executable and project path. It preserves other servers and backs up changed configurations. Reload MCP servers or restart the clients. Claude Code may ask you to approve the project server; Codex must trust the project before loading its project configuration. Cursor may need the server enabled in its MCP settings.

Ask your agent:

> Use soudan to ask Claude Code about this design, then have Cursor critique its answer in the same room. Summarize the points of agreement and disagreement.

Or try the CLI:

```sh
soudan consult --agent claude-code --room design --wait \
  'What are the tradeoffs of SQLite for local agent conversations?'
soudan consult --agent cursor --room design --wait \
  'Critique the previous answer. Which failure case should we test first?'
soudan consult --agent codex --room design --wait \
  'Assess the discussion and propose one concrete acceptance test.'
soudan history --room design
```

Each CLI uses its own existing account and may consume paid usage. No additional model API key is needed by Soudan itself.

## MCP tools

| Tool | Purpose |
| --- | --- |
| `soudan_agents` | List plugins and executable availability. |
| `soudan_consult` | Start a consultation; returns `job_id` and `room`. |
| `soudan_result` | Read queued, running, completed, or failed job state. |
| `soudan_post` | Post from your current session to a shared room. |
| `soudan_history` | Read room messages after a message ID. |

Consultation example:

```json
{"agent":"claude-code","room":"design","prompt":"Review this proposal...","request_id":"design-review-1","timeout_seconds":180}
```

Pass the returned `job_id` to `soudan_result`. After completion, consult another agent in the same `room`. Use a new `request_id` for each new question and reuse it only when retrying an identical request. Poll every few seconds while a job is queued or running.

## Connecting already-open sessions

Send directly into a chat that is already running, using whichever transport that agent offers.

Codex uses its session queue. Claude Code uses its native inbox socket (experimental peer-protocol v1 adapter, Linux). Neither needs Kitty:

```sh
soudan live list
soudan live read <TARGET_ID>
soudan live send <TARGET_ID> --request-id hello-1 'Hello from the other agent. Please reply here.'
soudan live read <TARGET_ID>
```

Only Cursor Agent uses the Kitty terminal transport, which needs a one-time attachment:

```sh
soudan live setup --via-pid <PID_FROM_LIST>
# Press Ctrl+Shift+F12 once in the existing Kitty window.
```

MCP clients can use `soudan_live_targets`, `soudan_live_read`, `soudan_live_send`, and `soudan_live_delivery` for the same workflow. See [live chat setup and verification](docs/live-chats.md). The destination is always the existing session; neither `queued` nor `submitted` is an answer, so read the target to see its reply.

For clients outside this live transport, shared rooms remain available: ask each session to use `soudan_post` and `soudan_history` in the same workspace. `soudan_consult` continues to start separate headless sessions with room context.

## Plugins

Add a trusted `soudan.toml` in the workspace:

```toml
[agents.my-agent]
command = "/absolute/path/to/my-agent"
args = ["--print"]
input = "stdin"
output = "text"
```

New agents require no changes to the MCP server. Plugins run as child processes, receive the conversation as input, and return text or a JSON `result`. See [the plugin contract](docs/plugins.md) for all settings and built-in defaults.

## Documentation

- [Send to existing terminal chats](docs/live-chats.md)
- [Client setup and troubleshooting](docs/setup.md)
- [Architecture, limits, and conversation semantics](docs/architecture.md)
- [Plugin development](docs/plugins.md)
- [Sample consultation skill](docs/examples/soudan/SKILL.md)
- [Testing and live verification](docs/testing.md)
- [Initial live conversation report](docs/live-verification.md)
- [Contributing with TDD](CONTRIBUTING.md)

## Development

```sh
cargo test --locked
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
```

Tests use deterministic local executables and temporary workspaces; they do not call paid models. Live checks are separate and opt-in. Licensed under [MIT](LICENSE).

Inspect `soudan live delivery <request_id>` (MCP: `soudan_live_delivery`) for a derived `receipt.status`: `taken`, `waiting`, `blocked`, `lost`, or `unknown`. Saved sender status is unchanged. See [receipt evidence and limitations](docs/live-chats.md#receipt-evidence).
