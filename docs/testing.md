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

## Reproducing a macOS failure from Linux

CI runs the deterministic suite on Linux and macOS. Two classes of macOS-only
failure have reached `main`, and neither needs a Mac to reproduce. Cross
compiling is not the answer: `cargo clippy --target aarch64-apple-darwin` fails
while building the vendored SQLite in `libsqlite3-sys`, which needs a macOS C
toolchain. Simulate the two conditions separately instead.

**Platform-gated compilation.** Delivery adapters are `#[cfg(target_os =
"linux")]`, and several test files are gated at the file level, so macOS
compiles different code and runs a smaller suite. Rewrite the gate to another
operating system name to compile what macOS compiles, in both `src` and
`tests`:

```sh
sed -i 's/target_os = "linux"/target_os = "freebsd"/g' src/*.rs tests/*.rs
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
git checkout -- src tests
```

Use a real target name. An invented one trips the `unexpected_cfgs` lint and
buries the failure you were looking for. `cfg(unix)` stays true, as on macOS.
**Commit or copy your work first:** the `git checkout` that ends the
simulation discards uncommitted changes in those directories.

This catches unused imports that only appear when the Linux-only code is gone.
An unused `std::time::Duration` in `src/claude.rs` failed the macOS Clippy step
this way, and because Clippy runs before the test step, it hid every macOS test
result until it was fixed.

**Path canonicalization.** macOS resolves `TMPDIR` through `/var`, a symbolic
link to `/private/var`, so a temporary path and its canonical form differ.
Linux temporary paths canonicalize to themselves, which hides any assertion
that compares a recorded absolute path against a raw one. Point `TMPDIR` at a
symbolic link to reproduce it:

```sh
mkdir -p /short/path/real && ln -s /short/path/real /short/path/link
TMPDIR=/short/path/link cargo test --locked
```

Keep that path short. A Unix socket address is limited to about 104 bytes, so a
long `TMPDIR` fails the socket tests for a reason that has nothing to do with
macOS. This condition caught
`installer_carries_explicit_plugin_configuration_into_clients`, which compared
the `--config` path the installer records, canonicalized, against the raw
temporary path. The fix belonged in the expectation, not in the assertion:
canonicalize the expected path and keep checking that it reaches the client.

**Accepted socket mode.** TCP fixture listeners use nonblocking `accept` so
shutdown can be polled, but their connection handlers perform blocking reads.
BSD/macOS inherits the listener's nonblocking mode on accepted sockets; Linux
does not. Set `stream.set_nonblocking(false)` explicitly after accepting.
Otherwise a read can return `WouldBlock` before a frame arrives, closing the
connection and producing EOF or `Broken pipe` in the client. Reproduce on Linux
by temporarily setting the accepted stream to nonblocking before the handler;
`cargo test --locked --test copilot rpc::lost_ack_is_uncertain_and_retry_never_sends_again`
then fails with `not_delivered` instead of the expected `uncertain`. Restoring
blocking mode passes without weakening the receipt assertions. See
[accept portability notes](https://man7.org/linux/man-pages/man2/accept.2.html).

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

`tests/live.rs` verifies process identity checks, input validation, replay of a settled delivery, and the error naming a target's actual working directory. MCP tests cover discovery and the refusal of an unserved transport.


## Claude native inbox tests

`tests/claude.rs` uses temporary session registries and an in-process Unix socket peer. No authenticated model or user chat is required. It tests process/workspace/protocol matching, single-line JSON framing, native submission, wrong-peer rejection, missing/symlinked sockets, and session-scoped transcript reads.

A live check must use a session in the requested workspace and a unique verification marker. Check `soudan live delivery` and `soudan live read` separately: a submitted socket write is not proof that Claude's inbound policy delivered the message or that the model replied. See [Claude native verification](claude-native-verification.md).
