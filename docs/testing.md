# Testing

## Deterministic suite

```sh
cargo test --locked
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
```

The integration tests start real Soudan subprocesses and speak newline-delimited JSON-RPC over MCP stdio. They initialize the protocol, discover tools, exchange room messages, submit a deterministic executable plugin, disconnect the submitting MCP process, reconnect, and retrieve the worker's persisted answer.

Coverage includes:

- SQLite persistence, room isolation, message pagination, and recent-context selection.
- Concurrent-room exclusion and workspace capacity across database connections.
- Job claiming, terminal results, deadline expiry, and retry-key conflict handling.
- Literal prompt transport, nonzero exits, JSON provider errors, output limits, timeout, and Unix descendant cleanup.
- Client configuration merging, idempotent installation, malformed input preservation, custom configuration paths, and private file permissions.
- Cross-client MCP conversations and continued jobs after MCP disconnection.

Fixtures use Unix executables such as `cat`, `sleep`, and `sh`, and temporary directories. No provider login or paid model invocation is part of `cargo test`. CI targets Linux and macOS; Windows support remains unvalidated.

## Live adapter dialogue

After installing and authenticating the CLIs:

```sh
cargo build --locked
python3 scripts/live_smoke.py --binary ./target/debug/soudan
```

This explicitly invokes real providers and consumes their normal account usage. It asks Claude Code for an acceptance test, asks Cursor to critique that answer, then asks Codex to refine the discussion. It verifies that all three answers appear in the same room. Provider content is nondeterministic; the check verifies successful transport, persistence, and responder order, not answer quality. Use `--agents claude-code cursor` to narrow the check.

## Live MCP client test

After `soudan install --client all` and reloading the server, ask a connected agent:

> Call soudan_agents. Post a greeting to a new room with your sender name. Use soudan_consult to ask another agent to reply to the greeting. Poll soudan_result until it completes or fails. Read soudan_history and report the actual result.

For a bounded Claude Code CLI invocation, create a JSON config containing only the Soudan server and use:

```sh
claude -p --output-format json --tools '' --strict-mcp-config \
  --mcp-config /path/to/soudan-mcp.json --setting-sources '' \
  --allowedTools 'mcp__soudan__soudan_agents,mcp__soudan__soudan_post,mcp__soudan__soudan_consult,mcp__soudan__soudan_result,mcp__soudan__soudan_history' \
  -- 'Run the live MCP client test described above in room my-live-test. Ask Cursor to reply. Use only Soudan tools.'
```

The `--` separator matters: Claude Code's tool-list option accepts multiple arguments and can otherwise consume the prompt.

## Existing-session test

1. Load Soudan in two already-open clients using the same workspace.
2. Ask the first to post to room `editor-test`, with a stable sender label and a new request ID.
3. Ask the second to read that room and post a reply.
4. Read the reply in the first client using the previous message ID as `after`.
5. Restart a client, repeat its last post with the same request ID, and confirm the original message ID returns without another message.

This verifies explicit participation by existing sessions. It does not imply that Soudan can wake an idle GUI chat or push a prompt into its private conversation.

## Initial TDD evidence

The implementation was developed through failing tests followed by fixes. Recorded local red/green cycles included missing core storage/plugin behavior; CLI install and message commands; durable job APIs; retry IDs rejected by MCP; descendants surviving timeouts; lost custom configuration paths; and private config permissions changing to 0644. The descendant test failed by observing a delayed marker written by a surviving child, then passed after process-group cleanup was added.

See [the live verification report](live-verification.md) for real client results and limitations.


## Live terminal transport (0.2.0)

`tests/live.rs` verifies process identity checks, input validation, busy/draft rejection, and reversible Kitty shortcut setup. MCP tests cover discovery and offline errors for the live tools.

`python3 scripts/live_terminal_smoke.py` requires a working Kitty desktop. It uses a temporary hidden Kitty instance and a deterministic local chat fixture to verify actual terminal input, visible replies, deduplication, draft protection, and disconnect. It does not invoke paid models. See [the terminal verification report](live-terminal-verification.md) for the distinction between these checks and the user's currently open LLM sessions.
