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

## Round-trip confirmation after the implementation commit

After commit `c1b8eaa`, Claude Code returned `RECEIPT-NATIVE-7711` in
`receipt-approve-1` and reported an independent successful run of all 46 tests,
format checking and clippy with no warnings. A fresh local CLI observation also
confirmed `receipt-native-check-1` now has `receipt.status=taken`, while its sender
`status` remains `submitted`. The earlier `waiting` observation above describes
an earlier snapshot, not a failed delivery.

Fresh CLI checks confirmed both `impl-claude-transport-2` and
`claude-native-proof-1` still have `receipt.status=unknown`. Neither has a saved
send-time boundary. A missing marker in an unproven evidence window cannot
establish non-receipt, even when separate historical reports describe failure;
returning `lost` would turn that assumption into an unsupported observation.

The marker limitations are documented in `live-chats.md`: quoting the exact
request marker can produce a false positive, and transcript inclusion alone does
not prove that the agent read, understood or answered the message. Here the
separate response code supplies the round-trip confirmation.
