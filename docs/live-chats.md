# Send messages to chats that are already open

Soudan can deliver a message into an existing interactive chat and read its reply. This path preserves the running session and its private conversation context. It does not start a headless replacement session.

The transport depends on the agent, because a terminal is a poor substitute for a session API:

| Agent | Transport | Setup |
| --- | --- | --- |
| Codex CLI | Codex's own session queue | none |
| Claude Code | Native peer inbox socket (Linux) | messaging enabled in the target |
| Cursor Agent CLI | Linux + Kitty terminal | one-time bridge attachment |

A target's id names its transport, because each agent has exactly one. Codex ids begin `codex:`, Claude Code ids begin `claude:`, and Cursor ids begin `kitty:`. There is no fallback between them: keystroke automation cannot reliably tell an empty composer from one holding a draft, so a Codex session whose thread cannot be read is reported as an error rather than typed into.

Claude Code uses its cross-session inbox as described below. Cursor Agent CLI currently uses terminal delivery. Native Cursor editor panels, browser chats, and other terminal emulators are not supported. The existing headless plugin integrations remain available on their previously supported platforms.

## Codex sessions need no setup

`soudan live list` finds the session, and Soudan resolves its thread identity from the files the process holds open: the thread write lock and that thread's rollout, matched by exact name. That works whether or not the session was started with `codex resume`, whose command line is the only other place the thread id appears, and it does not require the session to be running under Kitty. A process holding more than one thread lock is refused rather than guessed at.

```sh
soudan live read codex:900463:210017161
soudan live send codex:900463:210017161 \
  --request-id design-question-1 \
  'From Claude Code: please review this proposal.'
soudan live read codex:900463:210017161
```

`read` returns structured fields rather than a screen:

```json
{
  "kind": "codex_thread",
  "status": "idle",
  "last_agent_message": "…",
  "thread": "01a08b4c-411f-7e70-a0d7-66b89fdb59c7"
}
```

`status` is `idle`, `running`, `aborted`, or `unknown`. It distinguishes a turn in flight from a finished one, so a reply is not mistaken for a partially rendered screen. `unknown` means no start or completion appeared in the part of the transcript that was read, which a very long turn can cause; it is not a claim that the session is free. `aborted` means the last turn was interrupted: Codex does not drain its queue after an interruption, so a message delivered to that thread waits for the person at the keyboard. Delivery reports `queued`, which means Codex accepted the message, not that it answered.

Codex accepts a message **even while it is working**, and answers it in order. There is no draft to protect and no idle composer to wait for, so none of the terminal restrictions below apply. `codex` must be on `PATH`; when it is missing the delivery is recorded as `not_delivered` and the same request ID may be retried, because a command that never ran cannot have delivered anything.

The Kitty sections below apply only to Cursor.

## Claude Code sessions: native inbox, no Kitty

Use the current Soudan binary and reload the Soudan MCP server in the sending client after upgrading. The receiving Claude session does not need a new MCP server, channels flag, bridge, or restart when its native inbox is already enabled.

```sh
soudan live list
# Copy the exact claude: target ID returned above.
soudan live read claude:12345:67890
soudan live send claude:12345:67890 --request-id claude-review-1 \
  'From Codex: please review the latest change and reply here.'
soudan live delivery claude-review-1
soudan live read claude:12345:67890
```

The same route is available through `soudan_live_targets`, `soudan_live_send`, and `soudan_live_read`. Discovery is currently Linux-only and scoped to interactive processes in the exact workspace. Claude does not need `KITTY_WINDOW_ID`.

Soudan reads the selected process's `CLAUDE_CONFIG_DIR` (or `HOME/.claude`), then its `sessions/<pid>.json` record. It checks workspace, PID, process start time, canonical session UUID, and peer protocol 1. Before writing, it validates the socket type, directory ownership, connected peer UID/PID, and process lifetime. It does not read recipient auth keys or claim a permission class. A missing or unsupported inbox is an error, never a terminal fallback.

**Compatibility:** the inbox is a documented Claude feature, but its JSON wire format and registry layout are not a versioned public SDK contract. This adapter is experimental, targets peer protocol 1, and was inspected against Claude Code 2.1.268. Unknown protocol versions are refused. Upgrades should rerun the socket tests and a bounded live test; compatibility with all future versions is not promised. A protocol change can make Claude delivery unavailable until this adapter is updated; Kitty cannot be selected as a workaround in this implementation.

`submitted` means the message was written to the inbox. Claude may hold or refuse it according to `crossSessionInbound` and its permission mode. If Claude displays an approval dialog, approve it in that session if desired. Soudan never changes these controls. `not_delivered` is retryable with identical arguments; write failures are `uncertain` and are not automatically retried. Delivery history is committed before writing.

`read` returns `kind: claude_session`, `session`, `status`, `last_agent_message`, and `transcript_available`. Status comes from the registry (`busy` becomes `running`; unrecognized states become `unknown`). Reply text is the latest matching assistant text in the last 256 KiB of the session transcript, possibly an intermediate response or an older reply. A missing transcript returns null rather than blocking delivery. Verify a unique request marker; a successful write is not an answer. Replies remain visible in the original chat. There is no automatic reply loop or reverse socket address in this first adapter.

See the [official cross-session messaging documentation](https://code.claude.com/docs/en/cross-session-messaging#the-sessions-inbox-socket) for availability, inbound controls, and socket configuration.

## Connect without restarting your chats

This applies to Cursor targets only. Install the current binary, then discover the running chats in your project:

```sh
cargo install --path /path/to/soudan --locked --force
cd /path/to/your/project
soudan live list
```

Each target contains its agent name, PID, Kitty window ID, and a stable process identity. Choose a PID from this list:

```sh
soudan live setup --via-pid 12345
```

The command adds a project-specific **Ctrl+Shift+F12** shortcut to Kitty's configuration and reloads its key mappings. Press that shortcut once in the existing Kitty instance. It launches a background bridge with permission to read screens, paste text, and send the Enter key. Your open chats remain running.

This key press is needed when Kitty was started without remote control. Its global remote-control mode cannot be enabled by reloading configuration. Soudan does not change the global mode or restart Kitty. The shortcut grants a dedicated connection only to the bridge. If the key is already mapped, setup refuses to overwrite it; configure a different shortcut manually.

Setup expects Kitty's standard configuration path (`$XDG_CONFIG_HOME/kitty/kitty.conf`, or `~/.config/kitty/kitty.conf`). For a Kitty instance launched with a custom config path, place the generated mapping in that config and reload it yourself. Only one Kitty instance is connected per workspace in this version.

## Send and verify

Use a target ID returned by `live list`, not a bare PID or window title:

```sh
soudan live read kitty:12345:987654
soudan live send kitty:12345:987654 \
  --request-id design-question-1 \
  'From Codex: please review this proposal and reply here in this chat.'
soudan live read kitty:12345:987654
soudan live delivery design-question-1
```

The target chat receives a visibly labelled message such as `[Soudan design-question-1] From Codex: ...`. Its own running agent responds in that same terminal. Read the screen again after the agent finishes. To continue a conversation across two open chats, read the first reply and explicitly relay the relevant text to the second target.

`submitted` means Kitty accepted the paste and Enter operations. It is **not** proof of a model response. `live read` returns a `terminal_snapshot`; terminal output includes prompts, user messages, status text, and assistant responses. Verify the visible exchange rather than treating the entire snapshot as an assistant answer.

If a send is interrupted after its intent is recorded, its state stays `uncertain`. Repeating its request ID never resubmits it, even if the original send may have failed; the recorded outcome comes back with `"replayed": true`. That answer is given before the target is looked at, so an ID whose fate is already decided still reports it after the chat has ended or while it is mid-turn. Inspect the target before deciding whether to create a new request. Reusing an ID with different text or a different target is rejected. Live request IDs contain 1–128 ASCII letters, digits, hyphens, underscores, periods, or colons.

## MCP workflow

Reload the Soudan MCP connection after upgrading. Four live tools are available:

| Tool | Purpose |
| --- | --- |
| `soudan_live_targets` | Discover existing agent terminals in this workspace. |
| `soudan_live_read` | Read one target's state: a Codex or Claude session's state and recent reply, or Cursor's visible terminal screen. |
| `soudan_live_delivery` | Inspect a request ID and derive receipt evidence without resending. |
| `soudan_live_send` | Deliver a message into one target's existing chat over that agent's transport. |

Example request to a coordinating agent:

> Use Soudan's live tools to send Claude Code a design question in its currently open chat. Read its reply, then send that reply to the currently open Cursor chat for critique. Show me which terminal received each message.

Client tool permissions are separate from Kitty attachment. Following the format in [setup.md](setup.md#noninteractive-tool-permissions), add the four live tool names to the clients you want to use as coordinators. Approving `soudan_live_send` allows direct input to the selected terminal; headless consultation workers are explicitly prohibited from using this send path.

## Delivery checks and limits

These apply to the Kitty transport. Codex deliveries are subject only to the message limits and the request ID rules above.

Before input is sent, Soudan checks that:

- The target process is still the discovered agent, with the same start time and Kitty window ID.
- It is still in the same canonical workspace and the same Kitty instance as the bridge.
- It is the terminal's foreground process group.
- The visible input prompt is recognizable and empty, with no detected draft or busy indicator.
- The message is nonblank, at most 8 KiB, and contains no terminal control characters except newline and tab.

The input check fails closed for unsupported prompt layouts. Do not work around a refusal by clearing another person's draft. These screen-based checks cannot make keyboard input atomic with a person typing: leave the destination input untouched during a send. Sending to busy sessions is not queued automatically.

The bridge socket lives at `.soudan/kitty.sock` with mode 0600 inside the private state directory. Requests are serialized. Delivery records, including the pre-send screen, are kept in `.soudan/state.db` and should stay out of version control.

## Disconnect

```sh
soudan live disconnect
```

This stops the bridge and removes the exact shortcut block installed by Soudan, preserving the rest of Kitty's configuration. It leaves chats and delivery history intact. If you edited that block manually, remove it manually when the command reports a conflict. After rebuilding or reinstalling Soudan, disconnect, rerun setup, and press the shortcut to launch the updated bridge.

## Verification

Run the deterministic Rust tests:

```sh
cargo test --locked
```

A separate desktop integration test uses a temporary, hidden Kitty window with a local chat fixture. It consumes no model usage:

```sh
cargo build --locked
python3 scripts/live_terminal_smoke.py
```

It tests real terminal input, a visible fixture reply, retry deduplication, draft protection, and disconnect. This is distinct from verifying the user's already-running LLM sessions; that requires the one-time bridge attachment and an actual send/read exchange.

See [the verification report](live-terminal-verification.md) for completed checks and current attachment status.

## Receipt evidence

`live send` includes a machine-readable `receipt` object. Inspect it again with
`soudan live delivery <request_id>` or MCP `soudan_live_delivery` with
`{"request_id":"<request_id>"}`. These observations never overwrite the saved
sender `status` and never resend, cancel, or manipulate another agent's queue.

| `receipt.status` | Observation |
| --- | --- |
| `taken` | The recipient log contains `[Soudan <request_id>]` after the send boundary. |
| `waiting` | No marker yet; the original process is alive (Codex must be idle/running). Claude policy can still hold or refuse input. |
| `blocked` | No marker; the original Codex process is alive but its turn is aborted. Human interaction is required before the queue is collected. |
| `lost` | The covered log range has no marker and the original PID/start time is gone or replaced. |
| `unknown` | Coverage, process identity, or Codex state cannot be established. Cursor always returns this because it has no persistent receipt log. |

Aborted Codex sessions still accept messages. Their send result explains that
waiting alone will not deliver the message; someone must interact with the terminal.
`waiting` is also an observation, not a promise of eventual delivery.

Before delivery, Soudan commits the log path, byte offset, device/inode and up to
128 preceding bytes alongside its request reservation. Receipt checks stream the
entire appended range through a bounded buffer, including markers more than
256 KiB behind the end. Normal rotation, truncation, a changed boundary, missing
logs or unreadable evidence yield `unknown`. This assumes append-only logs:
arbitrary in-place rewrites that restore the boundary cannot be detected. Each
result is a snapshot and subsequent writes can change it. `lost` concerns the
original recipient process; it is not a guarantee that a resumed thread could
never process an old queue item. Do not automatically resend based on it.

Existing databases are migrated transactionally with a nullable `receipt_basis`
column. Legacy rows have no send boundary and return `unknown`, including the
historical failed queue and Claude envelope probes. Missing transcripts at send
time also produce `unknown`; they do not prevent sending. Observations are not
stored. No Codex private queue database is read and no terminal scraping is used
for receipts.

A marker is evidence of transcript inclusion, **not proof that the agent read,
understood, or answered the message**. Another message quoting the exact marker
can produce a false positive. Use `live read` to verify the actual reply.

After upgrading, reload the MCP connection and permit `soudan_live_delivery` in
clients with explicit tool allowlists (Claude: `mcp__soudan__soudan_live_delivery`;
Cursor: `Mcp(soudan:soudan_live_delivery)`; Codex: the corresponding tool entry
under its Soudan MCP permissions). Room posts still do not wake another session.
