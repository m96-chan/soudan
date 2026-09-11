# Architecture and contracts

## Components

- `mcp.rs`: consultation, room, and live-terminal tools served over stdio by the official Rust MCP SDK (`rmcp`). Protocol output stays on stdout; application failures use MCP tool error results.
- `app.rs`: workspace setup, plugin discovery, job submission, and detached worker execution.
- `store.rs`: SQLite messages, jobs, atomic idempotency records, and job admission limits.
- `config.rs` / `process.rs`: executable plugin definitions, literal prompt transport, bounded output, timeout, and child cleanup.
- `live.rs` / `codex.rs`: delivery into chats that are already open. Each agent uses its own best transport; the Kitty terminal path is the fallback for agents without a session API.
- `install.rs`: client configuration merging and backups.

Each MCP client starts its own Soudan server process. Servers for the same canonical workspace share `.soudan/state.db`. SQLite uses WAL and a five-second busy timeout. No TCP port, broker daemon, hosted service, or database installation is needed.

## Consultation lifecycle

1. `soudan_consult` validates the request and transactionally creates a job plus its requester message.
2. Soudan starts a separate worker process and immediately returns the job ID and room.
3. Exactly one worker can atomically claim the queued job. It builds a prompt from the recent room transcript and the job's explicitly assigned question, then invokes the selected plugin.
4. The worker atomically stores the terminal job state and, on success, the agent's answer as a room message.
5. `soudan_result` reads the result from any client connected to this workspace.

Worker processes outlive the MCP connection that submitted them. Closing a client is not cancellation. If the worker crashes, the machine restarts, or the CLI stalls, a job is reported failed when read after its deadline (timeout plus a ten-second startup allowance). There is no automatic paid retry. A new request ID deliberately starts a new attempt.

Only one consultation may be active per room, and at most four may be active in a workspace. These constraints are enforced in SQLite transactions across server processes. Interactive posts can still be added during a consultation. Workers answer their assigned question; the context is captured when the worker starts, not an immutable snapshot at submission.

## Rooms, identity, and pagination

Rooms are named shared discussion streams. Without a room, Soudan generates a UUID; if `request_id` is supplied without a room, that request ID becomes the room name so retries resolve consistently.

`sender` on an interactive post is a caller-supplied label, such as `cursor-design-review`. Reuse it across restarts if you want stable attribution. It is not an authenticated participant identity. Consultation prompts are attributed to `requester`, and answers to the configured plugin name. This release targets cooperating clients running under the same OS user, not mutually untrusted tenants.

Message IDs are monotonically increasing database-wide integers. `soudan_history(room, after)` returns up to 100 matching messages with `id > after`, in ascending order. Store the last **processed** message ID and pass it as `after` on the next read. Gaps are expected when other rooms receive messages. An empty page means caught up; it does not mean the discussion is closed. Reading does not acknowledge or delete messages.

Messages and terminal job results survive restarts. The context sent to a headless agent includes at most the latest 20 messages; the complete stored history remains available through pagination. Soudan does not resume or import a client's private chat history.

## Retry semantics

`request_id` is optional on `soudan_consult` and `soudan_post`, but recommended for clients that retry tool calls.

- Consultations: the key is unique across consultations in a workspace. An identical room, agent, prompt, and timeout returns the original job ID, including after completion or failure.
- Posts: the key is scoped to `(room, sender)`. Identical text returns the original message ID.
- Reusing a key with different arguments returns an error.
- Without a key, a repeated request is a new request. The busy-room guard does not provide deduplication after completion.

Key registration and the associated write use one transaction. Duplicate worker launches cannot execute the same job twice because claiming a job is atomic. Idempotency does not guarantee exactly-once external provider execution across arbitrary machine failure; Soudan does not replay an uncertain job automatically.

## Limits and process behavior

| Resource | Limit |
| --- | --- |
| Room, sender, request ID | 1–128 bytes, nonblank |
| Prompt / message / final answer | 64 KiB each |
| Agent timeout | 1–600 seconds; default 180 |
| Captured stdout / stderr | 1 MiB each |
| History page | 100 messages |
| Consultation context | Latest 20 messages, at most 128 KiB of serialized entries |
| Active jobs | 4 per workspace, 1 per room |

The context limit may reject a long discussion; begin a new room with a concise summary. Each input argument is passed directly to the executable, without shell interpolation. Plugins may still execute whatever their own implementation permits. On Unix, each plugin gets a process group, which is terminated on completion, failure, timeout, or future cancellation to clean up helpers as well as the direct child. Plugins that intentionally detach into another session are outside this cleanup guarantee. Windows descendant cleanup is not implemented or validated.

`.soudan/` is restricted to the current user on Unix. Conversation text, results, and error excerpts are stored locally in plaintext. The selected provider receives the prompt and recent transcript; normal provider account terms apply. There is no automatic retention expiry. Stop jobs and clients before deleting `.soudan/` to clear local history.

`SOUDAN_CHILD=1` blocks nested Soudan consultations from worker descendants. It prevents cooperative recursion; it is not a security boundary against plugins that deliberately alter their environment.

## Initial scope

The transport is stdio MCP. Room participation is pull-based; notifications and sampling are not required. Messages can be delivered into chats that are already open. Codex is reached through its own session queue, with no setup. Claude Code uses its native peer inbox on Linux; Cursor uses the Kitty terminal transport after a one-time attachment; see [live chats](live-chats.md). Other GUI chats must explicitly use the room tools after loading the server. Remote authenticated transport, native editor-panel delivery, consultation cancellation, and plugin capability negotiation are possible extensions, not current features.
