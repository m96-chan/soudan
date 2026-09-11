# OpenCode native sessions

Soudan uses OpenCode's published HTTP API, not composer or terminal input.
The adapter was verified with OpenCode 1.18.30 and the OpenAPI 3.1 document
served by that installation at `/doc`. It requires the v2 session operations and
client-supplied prompt IDs; incompatible contracts fail before a prompt is sent.
The server API is [publicly documented](https://dev.opencode.ai/docs/server/).
The local `/doc` describes the exact installed contract, including `/api/session`
routes; older documentation may describe `/session` routes instead.

## Setup

Start OpenCode yourself in the project to share:

```sh
opencode --port 4096 --hostname 127.0.0.1
soudan live list
```

Discovery is Linux-only and checks `/proc`: executable, working directory,
process start time, and ownership of the listening socket's inode. Only IPv4
127.0.0.1 listeners are accepted. Soudan does not scan arbitrary network ports,
restart an existing OpenCode, or change its configuration. Sessions are filtered
by the exact workspace directory; remote workspaces are excluded.

A target looks like `opencode:<pid>:<start_time>:<port>:<session_id>`. The API may
expose several saved sessions through one running server. Discovery does not
identify which conversation the TUI currently displays. Read the intended
session before sending. Process and listener checks reduce stale-target mistakes;
they do not provide cryptographic authentication of another local process.

If the server uses Basic authentication, supply the same
`OPENCODE_SERVER_PASSWORD` to the Soudan process. Set
`OPENCODE_SERVER_USERNAME` if it differs from `opencode`. For MCP, configure
these in the server environment and reconnect. Soudan does not harvest credentials
from OpenCode's process environment, save them in delivery records, or print
server error bodies. HTTP redirects and environment proxies are disabled.
Without the required credentials, discovery cannot list the server's sessions.

## Send, read, and optionally notify

```sh
soudan live read <TARGET_ID>
soudan live send <TARGET_ID> --request-id review-1 --sender codex \
  'Please review the current diff and reply in this session.'
soudan live delivery review-1
soudan live read <TARGET_ID>
# Optional, separate visual notification; this sends no conversation input.
soudan live notify <TARGET_ID> 'Soudan delivered a review request. Replies are available through the session API.'
```

MCP exposes the same operations through `soudan_live_targets`,
`soudan_live_send`, `soudan_live_delivery`, `soudan_live_read`, and
`soudan_live_notify`. Reload the MCP server after upgrading. The `sender` is an
unverified caller declaration, never authenticated provenance or permission.

In the tested version, external API prompts are processed but **not rendered in
the TUI**, even while it displays the destination session. Soudan reads the
assistant message detail endpoint, including structured content, completion,
errors and token information when supplied. The latest assistant message is not
necessarily a response to your request: verify its content and completion.
`/api/session/active` indicates running sessions, not the displayed TUI session.

An optional toast is independent of admission, receipt, and model execution.
Its `submitted` result only means the toast API accepted it, not that a human
saw it. Composer operations (`append-prompt`, `submit-prompt`), session selection,
and the unimplemented session wait endpoint are not used. An API-only agent
need not have Soudan MCP or room access; read its reply through `live read`
instead of assuming it will post to a room or wake a room waiter.

## Delivery and receipt

Before any POST, the usual transactional request claim commits an evidence
record containing the destination and a fresh `msg_` ID. The POST supplies that
ID and must return the same ID and session. `submitted` means durable prompt
admission was acknowledged; it does not mean a model answered. Preflight errors
are `not_delivered` and allow the same request ID and arguments to be retried.
Once POST starts, errors remain `uncertain`, including lost or mismatched
responses. There is no automatic retry. Evidence survives a lost response.

Receipt queries the destination session's exact message-detail endpoint:

- `taken`: the exact ID exists as a user message. Quoting a request marker in
  another message cannot satisfy this check. This proves session inclusion,
  not that the model read, understood, or answered it.
- `waiting`: the original server still owns the listener and the message returns
  404; admission may not yet be projected. This is not a promise of progress.
- `unknown`: credentials, identity, schema, or evidence cannot be verified,
  including a replaced or stopped server. A persistent admission could survive
  its process, so Soudan does not infer `lost` from process exit.

Sender status stays unchanged; receipt is derived on each read. OpenCode uses
neither a transcript byte boundary nor raw SQLite/WAL scanning. Each HTTP request
has a two-second total timeout and a 4 MiB response limit; multi-request reads
check an eight-second deadline before each request and bound pagination.
The web and wait observers retain their independent process deadlines.

## Verification

Deterministic tests use fake process trees and a local fixture HTTP server, never
real agent processes. They cover socket ownership, workspace isolation, API
guards, pre-POST durable evidence, retry semantics, exact-ID receipts, detailed
reply reading, authentication failure, redirect refusal, timeout, and separate
toast delivery.

On 2026-09-11, the same-workspace live probe `opencode-native-check-1` returned
`submitted`, then `taken` for `msg_545fb0a7dd9e4c6792d8309117b01551`.
`live read` retrieved the requested `OPENCODE-NATIVE-7734` reply with
`finish: stop`. This verifies an actual round trip through the implemented
adapter; it does not establish TUI visibility.
