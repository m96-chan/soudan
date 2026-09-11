# Waiting for another agent

Start a waiter **before ending a turn to await a reply**. Use the host's managed
background-task facility with completion notifications. In the tested Claude
Code harness, completion of such a task calls the session again; Soudan supplies
the bounded command, not the host wake-up mechanism. A detached shell command
alone (`&` or `nohup`) does not arrange that notification. Other hosts must expose
an equivalent facility before automatic wake-up can be promised.

Read the room history, retain its last processed message ID, and launch:

```sh
soudan wait --room codex-chat --after 32 --timeout 300
```

The first observation queries existing rows with `id > 32`; subsequent polls use
the same cursor. A message posted between reading history and starting the wait
is included. The result contains up to 100 messages in ID order and `last_id`.
Process that page before advancing the cursor; re-arm the waiter if more work is
pending. Any room message qualifies, including your own: an event is not proof
that the expected agent answered. Filter messages when processing the page.
Multiple watchers can see the same messages; none consumes them. Reusing the
same cursor returns the same page. Room IDs assume the workspace database has
not been replaced or reset; do not reuse an old cursor after resetting state.

## Wait for a specific reply

When several requests share a room, use structural correlation:

```sh
soudan wait --reply-to design-question-1 --room codex-chat --timeout 300
# Responder:
soudan post --room codex-chat --sender codex \
  --in-reply-to design-question-1 'Here is the result.'
```

MCP responders pass `in_reply_to` to `soudan_post`. Copy the ID from the incoming
`[Soudan <id>]` prefix. This field identifies the request being answered;
`request_id` on an MCP post instead identifies that post for retry deduplication.
Reusing a post request ID with a different reply correlation is rejected.

`--room` is optional with `--reply-to`: omission searches all rooms in the selected
workspace's `.soudan/state.db`, never other workspace databases. By default the
first matching row is returned, even if posted before the wait started. The
result has `outcome: "event"`, `kind: "reply"`, `request_id`, `messages` (one row),
and `last_id`. To read subsequent matching replies, repeat with
`--after <last_id>`; use `history --room <room> --after <last_id>` for subsequent
room messages. No cursor means the same first reply will be returned again.
Unrelated messages and quoted IDs in message bodies do not match. Correlation
records the sender's declared reply relationship, not the correctness of its answer.

Reply waits share the existing 300-second default, 3600-second maximum, JSON
outcomes and exit codes. They do not consult delivery receipts: **even if a delivery
is lost, the reply wait continues until a correlated post arrives or timeout**.
An agent can still post a reply obtained through another route. No post triggers
automatic live delivery or resend. Host-managed background completion is still
required to wake an idle agent.

Normal store opening atomically adds nullable `messages.in_reply_to` and its
index, preserving existing rows as NULL. Read-only waits never migrate: a missing
column supplies no correlated replies, and waiting continues until migration and
a matching post or timeout. Room waits include `in_reply_to: null` for legacy rows.
Restart MCP servers after installing the updated binary so their post schemas
and handlers accept the new field.

For receipt observation instead of an answer:

```sh
soudan live wait --request-id receipt-native-check-1 --timeout 300
```

This waits **only while receipt.status is `waiting`**. If the first observation is
`taken`, `blocked`, `lost`, or `unknown`, it returns immediately. A receipt that
changes to any of those states also ends the wait. A missing delivery is an
error. `unknown` is inconclusive, not successful delivery; `taken` is transcript
inclusion, not an answer. Prefer room waiting for an actual response.

| Exit code | JSON `outcome` | Meaning |
| --- | --- | --- |
| 0 | `event` | Matching room rows or a non-waiting receipt were observed. |
| 124 | `timeout` | No qualifying observation completed before the deadline. |
| 1 | `error` | Invalid value, missing delivery, unreadable state, or observer failure. |

The default timeout is 300 seconds; accepted values are 1–3600 seconds. JSON is
written to stdout. Normal CLI syntax errors (missing flags, unknown commands,
non-numeric arguments) remain clap usage errors on stderr with exit 2.

```json
{"outcome":"event","kind":"room","room":"codex-chat","after":32,"last_id":33,"messages":[{"id":33,"room":"codex-chat","sender":"codex","text":"Done"}]}
```

```json
{"outcome":"timeout","timeout_seconds":300,"watch":{"Room":{"room":"codex-chat","after":32}}}
```

Do not hide a timeout behind a shell success condition: let the host report both
the exit code and JSON. On timeout, inspect the situation and re-arm a bounded
waiter only while the authorized conversation remains pending. Do not resend a
message automatically or run an endless shell polling loop.

Waits bypass application initialization and open SQLite read-only. They do not
create a workspace, migrate schema, update delivery status, acknowledge messages,
or register a peer subscription. Each observation closes its DB connection
before sleeping. An absent room database/table is treated as no messages yet.
SQLite busy/locked results are retried within the deadline; other errors end the
wait. Legacy deliveries lacking receipt_basis return unknown without migration.

Polling is once per second after an immediate first observation, so ordinary
notification latency is up to about one second plus read time. Room queries use
the existing room/ID index. Receipt checks retain the complete send-time coverage
rules and may scan substantial appended logs. A separate supervised observer
keeps those synchronous reads outside the deadline controller: on timeout it is
killed and given up to 250 ms for reaping. The observer also has its own deadline.
No write transaction or DB connection is held while sleeping.

## Current Claude limits and proposed inbox support

A Claude-held message may remain `waiting` until the wait times out. The current
adapter has neither a held-queue query nor a callback receiver, so it cannot
classify that state as human-review-blocked. Missing markers do not distinguish
policy hold from scheduling delay. A transcript marker still does not mean the
model noticed or answered the message.

`soudan wait --inbox` is **not implemented**. A reliable direct inbox event source,
its session binding and cursor semantics require agreement first. `notify_idle`
is not that event source by itself: it reports availability and can defer while
held messages remain. See [the investigation and proposal](claude-inbox-proposal.md).

## Verification

The seven new wait tests exercise the history-to-wait gap, repeated non-consuming
reads, a later WAL commit, a missing workspace, timeout limits, a locked database,
legacy schema preservation, missing deliveries, fake-process receipt transitions,
and a log open blocked on a FIFO. The supervisor still returns timeout for the
blocked read. Final checks for this change: 53 tests passed, fmt check passed,
clippy all-targets passed without warnings. These automated tests do not prove
that every editor implements background-task completion notifications.

A same-workspace smoke check started `soudan wait --room codex-chat --after 33
--timeout 30` before room post #34 (`WAIT-ROOM-9044`). It exited 0 with
`outcome=event`, `last_id=34` and the matching message. This verifies the CLI's
room event path; it does not independently verify Claude's host wake-up behavior.

The reply-correlation extension adds tests for exact correlation (including a
quoted-ID non-match), a post in the history-to-wait gap, first-reply selection and
continuation, room filtering, workspace isolation, unrelated WAL commits,
read-only legacy waits, concurrent schema migration, legacy retry identity, and
MCP schema/history/retry validation. Validation on the completed tree: 66 tests
passed, `cargo fmt --check` passed, and `cargo clippy --all-targets` passed without
warnings. The example skill also passed its frontmatter validator.
