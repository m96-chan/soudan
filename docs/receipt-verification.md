# Receipt verification

Validated on 2026-09-11 against the current working tree:

- `cargo test`: 46 passed, 0 failed (1 unit test and 45 integration tests).
- `cargo fmt --check`: exit 0.
- `cargo clippy --all-targets`: exit 0, no warnings.
- The sample skill validator reports `Skill is valid!`.

Tests first failed because the receipt module did not exist. Implementation then
passed fake-process tests covering PID replacement, a discarded Claude envelope,
complete evidence beyond 256 KiB, markers split across scan buffers, pre-send
markers, rotation, truncation/regrowth, malformed process metadata, legacy DB
migration and preservation of sender status. Claim tests verify that replay does
not replace evidence and only proven non-delivery permits a new reservation.
The MCP integration test calls the new tool against an existing legacy record.

A real native send to this workspace's Claude Code session used request ID
`receipt-native-check-1`. Its result was `status=submitted` with
`receipt.status=waiting`. Subsequent delivery reads still reported `waiting`:
no matching marker or response was observed during this check. This demonstrates
why successful socket writes must not be reported as receipt. It does not prove
that the message was discarded or will eventually arrive. No automatic resend,
receiver policy change, queue manipulation or terminal injection was attempted.

The earlier historical failures have no saved send boundary and therefore remain
`unknown`; no negative verdict is inferred from a recent transcript tail.
