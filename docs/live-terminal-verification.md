# Live terminal transport verification

This report covers the 0.2.0 live-terminal implementation. The earlier [0.1.0 report](live-verification.md) covers headless model calls and MCP room participation.

## Verified

- Discovered the existing Claude Code, Codex CLI, and Cursor Agent CLI processes in the Soudan workspace, including their distinct Kitty window IDs.
- Built and tested the live MCP discovery/read/send tools.
- Ran a real Kitty desktop smoke test with a temporary chat fixture: input appeared in the already-running fixture's terminal, the fixture produced a visible reply, and the reply was retrieved through the bridge.
- Repeated the request ID and verified no second terminal input or reply occurred.
- Inserted an unfinished input draft, attempted a live send, and verified it was rejected without appending to the draft.
- Disconnected the bridge while preserving the terminal, and tested cleanup of a crashed bridge's stale socket.
- Tested stale process identities, wrong windows/workspaces, control-character rejection, multiline draft detection, and shortcut update/removal.

The fixture test is `python3 scripts/live_terminal_smoke.py`; it uses a real Kitty instance but no model account. It is not a substitute for observing a reply from the user's existing LLM chat.

## Current existing-session connection

The user's Kitty instance was started with remote control disabled. Reloading its global remote-control setting did not activate remote control; the exploratory configuration change was restored and its checksum verified.

The installed setup now uses Kitty's supported per-process connection: a Ctrl+Shift+F12 mapping launches Soudan's background bridge with only `get-text`, `send-text`, and `send-key` permissions. The global remote-control mode remains unchanged. The shortcut must be pressed once in the existing Kitty instance before Soudan can read or send to those sessions.

The attachment has since been completed and the bridge run against the user's real sessions. Reading worked for all three agents. Sending did not, and the failures were transport-specific:

- **Codex** was refused by the readiness check. Its empty composer draws the hint `Ask Codex to do anything`, and it animates braille particles across the input area, so an idle session looked like it held a draft. No send to an idle Codex could ever have succeeded through this transport. Codex now has its own transport and is no longer driven through a terminal at all, so the check is not taught these quirks: stripping the animation would also hide a draft made of the same characters.
- **Cursor** is still broken. Its composer line begins with `→`, which is not a recognized prompt, so the reverse scan latches onto a shell prompt left in the scrollback and reports a draft. Sending to Cursor fails.
- **Claude Code** behaved as designed; the busy guard correctly refused a session mid-turn.

`soudan live setup` hardcodes `ctrl+shift+f12`, which cannot be pressed on every keyboard. There is no option to choose the key, so the generated mapping has to be edited by hand.

## Codex no longer uses this transport

Codex exposes a session API, so it is reached through `codex queue` instead. Verified end to end with the bridge stopped: a message was delivered into the user's running Codex session and its reply was read back as structured data, with request ID replay and argument-mismatch rejection behaving as they do on the terminal path. See [live chats](live-chats.md).
