# Claude native inbox verification — 2026-09-11

## Scope and evidence

Implemented on top of ea455b9, without changing the user's other Claude sessions.
The recipient was the already-running Claude Code 2.1.268 session in this
workspace. Its registered inbox was used with the Kitty bridge offline.

The JSON frame was checked against the installed 2.1.268 sender and receiver:
peer protocol 1, `msgV`, UUID `msg_id`, `type: user`, string `message.content`,
`priority: next`, and optional recipient `session_id`. The native envelope is
`cross-session-message`, not `agent-message`. A regression test covers this.
No recipient key/token, permission class, or another Claude identity is borrowed.

The socket feature is documented by Anthropic at
https://code.claude.com/docs/en/cross-session-messaging#the-sessions-inbox-socket.
The registry and frame format are implementation details rather than a stable
public SDK contract. The adapter is experimental and refuses unknown protocol
versions. Future incompatibility produces an error instead of a terminal fallback.

## Completed checks

- 40 Rust tests passed (`cargo test --offline`), including 5 new Claude tests.
- `cargo fmt --check` and `cargo clippy --offline --all-targets -- -D warnings` passed.
- The sample Skill passed `quick_validate.py`.
- The temporary Kitty fixture (now representing Cursor) passed actual paste,
  visible reply, deduplication, draft protection, and disconnect checks.
- The live native read resolved the correct session and its existing transcript.
- Native socket writes `claude-native-proof-1` and `claude-native-status-2`
  returned `submitted` with transport `claude_socket`. The second used the
  corrected native envelope and included the recipient session ID.
- The installed Soudan binary and existing local Skill copies were updated.

## Confirmed live round trip

On 2026-09-11, a subsequent native `live read` returned the receiving Claude's
assistant response containing **CLAUDE-NATIVE-6622**, explicitly acknowledging
`claude-native-status-2` in the existing session. This confirms native delivery
and an assistant reply with the Kitty bridge offline. The response was read
from that session's transcript, not inferred from the delivery database.

The sender's implementation/build/send sequence confirms that the first probe
(`claude-native-proof-1`) used the initial `agent-message` envelope. It returned
`submitted`, but the recipient reported in `codex-chat` message #27 that it never
appeared on their screen. The recipient observed only `claude-native-status-2`,
sent after the envelope was corrected to `cross-session-message`, and confirmed
that no inbound approval or bypass settings were changed.

This is an observed unacknowledged submission followed by a confirmed delivery,
not a packet capture or proof of the receiver's internal rejection reason. An
envelope-related silent drop is a hypothesis; it has not been isolated from
other differences between the probes (including the added recipient session ID).
The evidence demonstrates that `submitted` means write completion, not receipt.
Do not automatically retry the first probe or describe envelope mismatch as the
only possible silent failure: inbound policy and recipient exit can also prevent
a submitted message from reaching the model.

This verification covers Claude Code 2.1.268 on this Linux machine. It does not
establish future protocol compatibility or bypass hold/refuse policies. The
stored delivery status remains `submitted`: Soudan does not automatically turn
observed transcript text into a durable recipient acknowledgement.

Reload Soudan's MCP connection in sending clients that still run the old server
process. The receiving Claude session itself does not require an MCP reload.

## Note added after the terminal transport was removed

The observations above were recorded while the Kitty bridge still existed, and
they name it because the point at the time was that native delivery worked
without it. That transport and its fixture have since been removed along with
Cursor support, so the commands they describe no longer exist. The native
results themselves are unaffected: they never used the bridge.
