# Local conversation monitor

Start the read-only dashboard explicitly from your workspace:

```sh
soudan web --port 8765
```

Open `http://127.0.0.1:8765` on the same machine. The command runs in the foreground;
stop it with Ctrl+C. It prints a JSON startup record containing the URL and port.
If a port is occupied, choose a different one. Explicit `--port 0` asks the OS for
an available port; use the URL printed by the command.

There is **no default listening port**. `soudan web` without `--port` fails, and
other commands never start the dashboard. The server binds only to IPv4
**127.0.0.1**. There is no host/address option; attempts to supply one are rejected.
Use the exact printed address rather than a hostname alias.

## What the page shows

- Deliveries: request ID, destination, saved sender status, and independently
  derived receipt status and reason. Counts summarize the displayed page.
- Rooms: all recorded room names and counts, with a selector for message bodies.
- Jobs: recorded consultation status, agent, room, ID and any saved error.

Delivery and job lists show 100 records per page, newest first, with paging
controls. A selected room shows the latest 100 messages in chronological order;
use Older messages to read earlier pages and Latest messages to return. Room
messages are rendered as plain text, not executable HTML or Markdown.

The page polls about every four seconds. A new refresh does not overlap an
unfinished one, so large pages or slow receipt scans can lengthen the interval.
Receipt requests use four browser workers; one unavailable receipt does not hide
the other records. Failed snapshot refreshes visibly mark displayed records as
potentially stale. Job statuses come from stored records; the dashboard does not
repair or expire jobs.

Sender status (`queued`, `submitted`, `not_delivered`, `uncertain`, `discarded`)
describes the handoff. Receipt (`taken`, `waiting`, `blocked`, `lost`, `unknown`)
describes current evidence. A process waiting for human review can still appear
as `waiting`. `taken` is marker inclusion, not proof of an answer, and quoted
markers can be false positives. Missing send-time evidence on legacy rows yields
`unknown`. See [receipt limits](live-chats.md#receipt-evidence).

## Access and data boundaries

**There is no authentication.** Anyone able to connect to this machine's local
port can see room message bodies (including analysis, session IDs and any secrets
participants posted), delivery IDs/destinations and evidence reasons, and job
metadata/errors. Treat access to the local port as access to these records. The
server does not offer a public bind address. Deliberately forwarding the port
would expose this unauthenticated information to whoever can use that forwarding.

Only fixed assets and GET read APIs are served. There is no file-serving root,
path query, arbitrary workspace selector, send/retry endpoint or job-start action.
The workspace is fixed at startup. State DB symlinks resolving outside that
workspace are rejected. Receipt calculation reads previously recorded native log
paths (which normally live outside the project in agent config directories), but
only the derived verdict/reason is returned, never raw log contents. Room bodies
are already workspace data and are displayed as such.

Host must exactly match `127.0.0.1:<port>`; foreign Origin and cross-site browser
requests are rejected. No CORS access is enabled. Responses disable caching and
MIME sniffing; a restrictive Content Security Policy disallows framing, inline
scripts, external resources and form submission. These browser protections do
not authenticate local processes.

Web reads bypass application initialization. SQLite is opened read-only; the
server never creates or migrates state.db, changes its records, consumes messages,
changes permission policy, or holds a write transaction. An absent DB produces
an empty dashboard. Existing databases without receipt_basis remain unmodified.
There is no dependence on the private Codex queue database.

## Deadlines and limits

Each API observation runs in a separate subprocess with a three-second deadline.
An observer still blocked in a log read is killed, with at most 250 ms for reaping.
A failed or timed-out receipt becomes `unknown` with a reason; failed overview or
room snapshots return HTTP 503. The server continues serving other connections.

There are at most six observers and 32 active connections. HTTP headers are
limited to 8 KiB with a two-second read deadline; response writes have a
three-second deadline. Only HTTP/1.1 GET with no body is accepted; no keep-alive,
WebSocket, upload or push protocol is implemented. Observer output is capped at
64 MiB. At observation capacity, APIs return 503 rather than starting more work.

## Verification

Automated tests cover explicit startup, fixed loopback addressing, Host/Origin
and method rejection, malformed/duplicate queries, traversal rejection, absent
state, legacy schema preservation, external DB symlinks, pagination, positive
receipt evidence, and job states. A FIFO fixture blocks a receipt's log open:
that request becomes unknown at the deadline while the page and overview remain
responsive. No real agent process is needed by these tests.

A Chromium smoke check on the development workspace displayed 20 deliveries,
four rooms and 11 jobs, with five taken and 15 unknown at that observation. It
reported no page JavaScript errors. A 390-pixel viewport had no document overflow.
A browser-only injected message containing an HTML image with an event handler
was displayed literally: no image element was created and no script executed.
These counts are observations from development, not constants in the UI.

Final checks for this change: `cargo test` passed all 56 tests;
`cargo fmt --check` and `cargo clippy --all-targets` passed without warnings.
No Rust crate was added. Browser testing
used a temporary Playwright installation outside the repository.

A further browser check confirmed automatic polling and room selection. An
executable-replacement regression test verifies that rebuilding or replacing the
server binary does not break observer startup; its path is captured at startup.
Restart the dashboard after upgrading across incompatible observer protocols.
