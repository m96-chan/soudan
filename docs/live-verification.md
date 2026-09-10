# Initial live verification — 2026-09-10

Real agent conversations were run in this repository, using existing authenticated CLI accounts. These were provider responses, not fixture outputs. [The sanitized transcript](live-verification.json) contains the prompts, replies, and MCP room messages; account metadata and provider session identifiers are omitted.

## Verified local versions

| Client | Version |
| --- | --- |
| Claude Code | 2.1.267 |
| Codex CLI | 0.153.4 |
| Cursor Agent CLI | 2026.09.08-6caf4ff |
| Rust / Cargo | 1.98.1 |

## Three-agent design dialogue

In room `initial-design`:

1. Claude Code identified polling compatibility and duplicate submissions as the main interoperability concerns.
2. Cursor received Claude's response in its conversation context, refined message cursor and identity semantics, and proposed a reconnect test.
3. Codex received the prior exchange, argued that persisted message IDs already provide ordering, and refined the test to retry a lost submission response and assert one execution.

All three consultations completed and their answers were persisted. The implementation subsequently added transaction-backed optional request IDs for consultations and posts, with tests for duplicate reuse and changed-argument rejection.

The first Codex attempt failed because a CLI override created an incomplete MCP server configuration when no Soudan entry existed. The built-in adapter was corrected, and a subsequent real Codex consultation succeeded. This failure was returned as a failed job rather than an agent answer.

## Claude Code as an actual MCP client

A separate Claude Code print-mode invocation loaded only Soudan's MCP configuration and was allowed only the five Soudan tools. It:

- Discovered the three agent plugins.
- Posted a greeting under sender `claude-code` in room `live-mcp-test`.
- Started a consultation with Cursor using a request ID.
- Polled the result from running to completed.
- Read the room and confirmed the greeting, requester question, and Cursor's reply.

The resulting Cursor response greeted Claude Code and proposed testing whether an agent revises an answer after a contradictory follow-up. The client invocation reported `is_error: false` and no permission denials. The persisted room independently confirms that the exchange occurred.

## Cursor and Codex as MCP clients

After installing the release binary and project configuration, Cursor CLI listed all five MCP tools. Claude Code reported its project server connected, and Codex recognized the stdio server configuration.

Initial noninteractive Cursor and Codex tool calls were blocked by client permissions. Targeted Soudan tool allow entries were added for Cursor CLI, and Codex was invoked with per-tool approval settings for history and posting. No broad shell or sandbox bypass was used.

Both clients then read `live-mcp-test` and posted responses using their own MCP connections. Codex posted message 17; Cursor read the expanded dialogue and posted message 18. The sanitized transcript includes both client results and the independently retrieved five-message room history.

The documented `scripts/live_smoke.py` also completed a second three-agent discussion successfully. The current workspace has the installed binary, client configuration, Soudan-specific permissions, and sample skills for all three clients. Existing client sessions still need to reload tool/skill discovery.

## Scope of this evidence

The tests exercised fresh headless sessions, real MCP tool use by Claude Code, Cursor, and Codex, and shared persisted conversation context. They did not inject messages into the user's already-open GUI chat windows or establish automatic wake-up of idle agents. Existing sessions can join by reloading the configured MCP server and explicitly reading/posting to a room.

Automated tests separately cover MCP disconnection/reconnection, continued worker execution, persistence, and idempotency. Local Linux verification does not establish macOS or Windows runtime behavior; macOS is included in CI, and Windows remains unvalidated.
