# Send messages to chats that are already open

Soudan can deliver a message into an existing interactive chat and read its reply. This path preserves the running session and its private conversation context. It does not start a headless replacement session.

The transport depends on the agent, because a terminal is a poor substitute for a session API:

| Agent | Transport | Setup |
| --- | --- | --- |
| Codex CLI | Codex's own session queue | none |
| Claude Code | Native peer inbox socket (Linux) | messaging enabled in the target |
| Grok Build | ACP through its shared leader process | `use_leader = true` before the chat is opened |

A target's id names its transport, because each agent has exactly one: Codex ids begin `codex:`, Claude Code ids begin `claude:`, Grok Build ids begin `grok:`. There is no fallback between them. A session whose own API cannot be reached is reported as an error rather than reached some other way, because the alternative was typing into its terminal, and keystrokes cannot tell an empty composer from one holding somebody's unsent draft.

**Cursor is not supported here.** It was reached by typing into its terminal through a Kitty bridge, which needed a keypress to arm, could not protect a draft, and existed for that one agent. That transport has been removed. Cursor returns when it exposes a session API of its own; its headless plugin integration is unaffected and still works with `soudan consult`.

Native editor panels, browser chats, and terminal emulators in general are not targets: delivery goes to the agent, not to whatever is drawing it.

## Codex sessions need no setup

`soudan live list` finds the session, and Soudan resolves its thread identity from the files the process holds open: the thread write lock and that thread's rollout, matched by exact name. That works whether or not the session was started with `codex resume`, whose command line is the only other place the thread id appears,. A process holding more than one thread lock is refused rather than guessed at.

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

## Claude Code sessions: native inbox

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

The same route is available through `soudan_live_targets`, `soudan_live_send`, and `soudan_live_read`. Discovery is currently Linux-only and scoped to interactive processes in the exact workspace. 

Soudan reads the selected process's `CLAUDE_CONFIG_DIR` (or `HOME/.claude`), then its `sessions/<pid>.json` record. It checks workspace, PID, process start time, canonical session UUID, and peer protocol 1. Before writing, it validates the socket type, directory ownership, connected peer UID/PID, and process lifetime. It does not read recipient auth keys or claim a permission class. A missing or unsupported inbox is an error, never a terminal fallback.

**Compatibility:** the inbox is a documented Claude feature, but its JSON wire format and registry layout are not a versioned public SDK contract. This adapter is experimental, targets peer protocol 1, and was inspected against Claude Code 2.1.268. Unknown protocol versions are refused. Upgrades should rerun the socket tests and a bounded live test; compatibility with all future versions is not promised. A protocol change can make Claude delivery unavailable until this adapter is updated; there is no fallback transport to select instead.

`submitted` means the message was written to the inbox. Claude may hold or refuse it according to `crossSessionInbound` and its permission mode. If Claude displays an approval dialog, approve it in that session if desired. Soudan never changes these controls. `not_delivered` is retryable with identical arguments; write failures are `uncertain` and are not automatically retried. Delivery history is committed before writing.

`read` returns `kind: claude_session`, `session`, `status`, `last_agent_message`, and `transcript_available`. Status comes from the registry (`busy` becomes `running`; unrecognized states become `unknown`). Reply text is the latest matching assistant text in the last 256 KiB of the session transcript, possibly an intermediate response or an older reply. A missing transcript returns null rather than blocking delivery. Verify a unique request marker; a successful write is not an answer. Replies remain visible in the original chat. There is no automatic reply loop or reverse socket address in this first adapter.

See the [official cross-session messaging documentation](https://code.claude.com/docs/en/cross-session-messaging#the-sessions-inbox-socket) for availability, inbound controls, and socket configuration.

## Send and verify

Use a target ID returned by `live list`, not a bare PID:

```sh
soudan live read claude:12345:987654
soudan live send claude:12345:987654 \
  --request-id design-question-1 \
  'From Codex: please review this proposal and reply here in this chat.'
soudan live delivery design-question-1
```

The target chat receives a visibly labelled message such as `[Soudan design-question-1] From Codex: ...`. Its own running agent responds in its own chat. To continue a conversation across two open chats, read the first reply and explicitly relay the relevant text to the second target.

`queued` and `submitted` describe what the sender did, not what the recipient got. Neither is proof of a model response. The `receipt` field reports what can actually be observed in the recipient's own log; see [Receipt evidence](#receipt-evidence).

If a send is interrupted after its intent is recorded, its state stays `uncertain`. Repeating its request ID never resubmits it, even if the original send may have failed; the recorded outcome comes back with `"replayed": true`. That answer is given before the target is looked at, so an ID whose fate is already decided still reports it after the chat has ended or while it is mid-turn. Inspect the target before deciding whether to create a new request. Reusing an ID with different text or a different target is rejected. Live request IDs contain 1–128 ASCII letters, digits, hyphens, underscores, periods, or colons.

## MCP workflow

Reload the Soudan MCP connection after upgrading. Four live tools are available:

| Tool | Purpose |
| --- | --- |
| `soudan_live_targets` | Discover existing agent terminals in this workspace. |
| `soudan_live_read` | Read one target's state: a Codex or Claude session's state and its most recent reply. |
| `soudan_live_delivery` | Inspect a request ID and derive receipt evidence without resending. |
| `soudan_live_send` | Deliver a message into one target's existing chat over that agent's transport. |

Example request to a coordinating agent:

> Use Soudan's live tools to send Claude Code a design question in its currently open chat. Read its reply, then send that reply to the open Codex session for critique. Show me which session received each message.

Following the format in [setup.md](setup.md#noninteractive-tool-permissions), add the four live tool names to the clients you want to use as coordinators. Approving `soudan_live_send` allows direct input to the selected terminal; headless consultation workers are explicitly prohibited from using this send path.

## Delivery checks and limits

Before a message is handed over, Soudan checks that:

- The target process is still the discovered agent, in the same workspace, with the same start time. A replaced process is refused rather than delivered to by name.
- For Claude Code, the connected socket peer's PID and UID match the target, and the inbox is a socket owned by this user in a directory no one else can write.
- The message is nonblank, at most 8 KiB, and contains no terminal control characters except newline and tab.
- The request ID is unused, or its recorded outcome proves nothing was delivered.

Delivery records are kept in `.soudan/state.db` and should stay out of version control.

## Verification

Run the deterministic Rust tests:

```sh
cargo test --locked
```

The tests build fake `/proc` trees and fake session logs, so they never depend on a running agent. Verifying delivery to a real session is a separate exercise: send to a chat you own and confirm the receipt, as recorded in [the native verification report](claude-native-verification.md).

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
| `unknown` | Coverage, process identity, or Codex state cannot be established. Absence of evidence is never reported as a failed delivery. |

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
Codex: the corresponding tool entry
under its Soudan MCP permissions). Room posts still do not wake another session.

For bounded room and receipt waits, see [waiting.md](waiting.md). A Claude message
held for human review can remain `waiting`: the current adapter cannot observe
its held queue or receive its policy callbacks. `live wait` times out in that
case; it does not release the message. `notify_idle` and direct inbox callbacks
are investigated in [claude-inbox-proposal.md](claude-inbox-proposal.md) and are
not implemented.

## Grok Build sessions: ACP through the leader

Grok speaks [ACP](https://agentclientprotocol.com), but a running chat is only
reachable when it was started against a shared leader process. Set it once:

```toml
# ~/.grok/config.toml
[cli]
use_leader = true
```

This takes effect for chats opened afterwards; a Grok already running without a
leader is discovered but cannot be delivered to, and says so. The leader listens
on `~/.grok/leader.sock` and wraps ACP in its own envelope: length-prefixed
frames, a registration handshake, then JSON-RPC carried as a string.

```sh
soudan live list
soudan live send grok:12345:987654 --request-id design-question-1 'Please review this.'
soudan live read grok:12345:987654
soudan live delivery design-question-1
```

Delivery hands the prompt over and closes the connection without waiting for the
turn. The leader keeps driving the session after a client disconnects, which was
verified by writing a prompt, closing immediately, and observing the turn run to
`turn_ended completed`. A turn can take minutes, so `submitted` means handed
over, never answered.

Receipt evidence is `chat_history.jsonl` in the session's directory. That file
exists from the moment a chat is created, while `updates.jsonl` only appears once
something streams, so anchoring on the former keeps the very first message
provable. Replies are read back from `updates.jsonl`, where one answer arrives as
a run of `agent_message_chunk` records that are reassembled in order.

**Compatibility:** the leader envelope and its registration frame are internal to
Grok, not a published contract, so this adapter is experimental. It checks
`leader_protocol_version` and refuses anything other than 1, and it requires the
agent to advertise `sessionCapabilities.resume`. It was inspected against Grok
Build 1.0.25. There is no fallback transport: a protocol change makes Grok
delivery unavailable until the adapter is updated.

## Declared sender

Use `soudan live send <target> --request-id <id> --sender codex 'Message'`, or
MCP `soudan_live_send` with `sender: "codex"`. The wire prefix retains
`[Soudan <id>]` and adds `Declared sender: codex (unverified)`. Omission produces
`unspecified`; Soudan never guesses from process settings or the recipient.
Labels use the request ID character set and length limit. The raw message and
nullable sender are recorded separately; retrying an ID with a different sender
is rejected, and legacy NULL senders remain compatible with omitted senders.

This is a caller declaration, not authenticated provenance, a human instruction,
or a permission assertion. It does not bind an originating session to this body
and destination. Claude's `from-mode` and inbound policy remain unchanged. A relay
must preserve earlier origins in its body and declare only its immediate sender;
this label cannot authenticate a multi-hop history. Delivery results expose the
sender through `live delivery`, with `sender_verification: unverified_declaration`.
