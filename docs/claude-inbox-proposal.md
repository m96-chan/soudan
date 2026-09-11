# Claude inbox observation proposal (not implemented)

The implemented room/receipt waits are documented in [waiting.md](waiting.md).
This document records static inspection of the installed Claude Code **2.1.268**
bundle at `~/.local/share/claude/versions/2.1.268`. Offsets below are byte offsets
in that binary, not stable APIs. No callback or idle subscription was sent during
this investigation. Keep permission-class assertions absent and do not change
`crossSessionInbound`.

## Held messages

The inbound gate at offset **199747924** stores held messages in
`_s().inbound.held` and invokes `sendPeerReceipt(message, "held")`. The registered
callback at **204718350** sends `type: control`, `action: peer_message_status`,
`orig_msg_id`, status and a reason to a validated reply address. States include
held, denied, expired, delivered, refused and dropped (refusal has a compatibility
encoding as expired plus status_detail). Here delivered means a previously held
message was released; it does not promise an answer.

The existing Soudan wire message has no top-level `from` callback address and its
random wire `msg_id` is not retained as a delivery correlation key. The callback
therefore has nowhere to go. No retrospective held-message query was found in
the inspected socket dispatcher; registry/transcript reads do not supply one.
Debug text and UI notices are not a dependable structured receipt API. Existing
`waiting` receipts must remain inconclusive, including human-review holds.

## Idle subscriptions

The `notify_idle` feature is published at **182748431**. The schemas at
**204670200** describe a `notify_when_idle` control request containing `from`,
`msg_id` and optional `from_mode`; the response is `peer_idle_notice` with
`orig_msg_id`, `state` and optional `finished_at`, `detail`, `from`, `from_mode`.
The receiver validates the reply namespace and rejects self-targets
(**204695100**, `CBt` / `KDn` at **182732859**). Subscriptions retain the verified
PID and process-start identity; callbacks reconnect to that original process.

A subscription is prospective and one-shot. The in-memory table is bounded
(32 entries) and expires old subscriptions (12 hours), not a durable event log.
A requester tracks outstanding IDs and rejects uncorrelated notices
(**204678290**). Recognized availability states are idle, exited and unavailable.

At **204674200**, notification is deferred if the session is no longer idle,
has queued work, or has held messages. Reply detail is further gated by matching
registry ownership and inbound policy. A bare availability notice must not be
interpreted as release of a held instruction, proof of response, or authority to
execute its optional detail.

The UI sentence about subscribing only after an accompanying message is delivered
needs qualification: the SendMessage branch at **199851668** awaits
`sendToUdsSocket` then subscribes. `uet` at **183640971** returns a message ID after
the transport operation; it does not wait for transcript inclusion or held-message
release. That ordering is not an end-to-end acknowledgement.

`IdleNotificationMessageSchema` at **184365521** describes a different
`idle_notification` shape. It is not the `peer_idle_notice` schema used by this
socket subscription. Symbol names alone do not establish compatibility.

## Proposed next step, requiring agreement

A Soudan-owned callback listener could collect correlated policy/availability
events without asserting a permission class or releasing anything held. It must
own a private reply socket in a namespace accepted by the native receiver, and
**the same long-lived process must perform sends/subscriptions**: replies are
bound to that process's verified PID/start time. A short-lived CLI send followed
by a different listener would not satisfy that contract. Do not borrow a Claude
session's socket address, registry identity or tokens.

Before implementation, validate the contract with a scoped same-workspace
experiment: callback peer identity, hold/deny/release, expired requests, registry
requirements for details, process replacement and late callbacks. No mode
assertions or recipient policy changes are part of that experiment.

If feasible, retain raw authenticated callbacks in a Soudan event journal,
separate from immutable sender outcomes. Start listening and record correlation
before sending. The event collector necessarily writes; it must be distinct from
a **read-only** `wait --inbox` that queries the journal. Define which session's
inbox is observed and expose an explicit cursor so startup gaps are recoverable.
Missing callbacks remain unknown; stored callback states need ordering, restart
and expiry semantics before they can drive derived receipts.

Subscribing changes the recipient's in-memory subscription table. It must be an
explicit authorized operation, not a hidden side effect of a read-only wait.
No cancellation contract was established; short waiter timeouts do not prove the
peer subscription was removed. Existing held messages without a prior callback
address cannot be retroactively recovered through this mechanism.

This can supply events to a host-managed background waiter. It cannot force a
held message through, and idle notification alone does not solve the held case.
For now the agreed room plus a bounded waiter is the practical completed route.
