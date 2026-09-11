# Agent plugin contract

Plugins are trusted executable adapters configured in TOML. They run out of process, so a future agent can be added without linking Rust code, matching a Rust ABI, or rebuilding Soudan. The plugin boundary is `command + args + input + output`; built-in agents use the same boundary as third-party agents.

Soudan loads `WORKSPACE/soudan.toml` automatically. `--config /path/to/file.toml` selects another file. Restart MCP connections after changing configuration. Custom entries extend the six built-ins; an entry with a built-in name **replaces the entire definition**, rather than merging individual fields.

```toml
[agents.local-reviewer]
command = "/opt/local-reviewer/bin/reviewer"
args = ["--quiet", "--format", "text"]
input = "stdin"
output = "text"
```

| Field | Values / behavior |
| --- | --- |
| `command` | Required nonempty executable name on PATH or absolute path. |
| `args` | Literal string array, default `[]`. No shell interpolation. |
| `input` | `argument` (default): append the full prompt as one argument; `stdin`: write it to stdin and close; `none`: no prompt transport, useful for fixtures. |
| `output` | `text` (default): stdout is the answer; `result_json`: stdout must be a JSON object with a string `result`; `is_error: true` fails the job. |

Stderr is captured separately. A nonzero exit status fails the consultation. Stdout must contain only the expected answer format, without startup banners or progress messages. Prompts contain an English consultation instruction and JSON-encoded recent room messages. The latest requester message is the question to answer.

The plugin runs in the selected workspace, inheriting the host environment for authentication. Soudan adds `SOUDAN_CHILD=1`; downstream Soudan instances refuse further consultations to prevent accidental recursion. Plugins must preserve this variable in their child environment. This is loop prevention for cooperative local processes, not a security sandbox.

## Built-in definitions

These defaults were verified against the locally installed CLIs on 2026-09-10:

```toml
[agents.claude-code]
command = "claude"
args = ["-p", "--output-format", "json", "--tools", "", "--strict-mcp-config", "--setting-sources", ""]
input = "stdin"
output = "result_json"

[agents.codex]
command = "codex"
args = ["exec", "--ephemeral", "--sandbox", "read-only", "--color", "never", "-"]
input = "stdin"
output = "text"

[agents.cursor]
command = "cursor-agent"
args = ["-p", "--mode", "ask", "--output-format", "json", "--trust"]
input = "argument"
output = "result_json"

[agents.grok]
command = "grok"
args = ["--output-format", "plain", "--tools", "", "-p"]
input = "argument"
output = "text"

[agents.opencode]
command = "opencode"
args = ["run", "--pure", "--agent", "plan"]
input = "argument"
output = "text"

[agents.copilot]
command = "copilot"
args = ["--output-format", "text", "--no-color", "--log-level", "none", "--deny-tool=shell", "--deny-tool=write", "-p"]
input = "argument"
output = "text"
```

The Grok Build, OpenCode, and GitHub Copilot CLI entries were verified on 2026-09-11 against grok 1.0.25, opencode 1.18.30, and GitHub Copilot CLI 1.0.83 installed locally. Both print the answer alone on stdout and send their banner to stderr, so neither needs a JSON envelope. Their JSON modes are deliberately unused: `grok --output-format json` names the answer `text` rather than the `result` field `result_json` reads, `opencode run --format json` is an NDJSON event stream rather than one object, and `copilot --output-format json` is JSONL.

Claude Code gets no built-in tools or inherited MCP configuration. Codex uses read-only sandbox mode, and Cursor uses ask mode. All receive an instruction to discuss without running commands or editing files. Grok Build is given no built-in tools. OpenCode runs its read-only `plan` agent with `--pure`, which skips external plugins; OpenCode still resolves agent permissions from workspace and user configuration, so a workspace that grants the `plan` agent write permission overrides this default. GitHub Copilot CLI denies shell and write access explicitly, because a non-interactive run cannot answer the permission prompt those tools would otherwise raise. Codex, Cursor, Grok Build, OpenCode, and GitHub Copilot CLI can still load client-specific configuration and extensions. These flags do not turn arbitrary third-party plugins into isolated sandboxes. Cursor's `--trust` trusts the selected workspace for this invocation, avoiding an interactive workspace prompt; it is not `--force` or blanket MCP approval.

To select a model, copy the full definition and add the CLI's supported model arguments. Soudan deliberately does not hard-code model names. Refer to [Codex noninteractive mode](https://developers.openai.com/codex/noninteractive/), [Claude Code programmatic mode](https://code.claude.com/docs/en/headless), [Cursor CLI](https://cursor.com/docs/cli/overview), and the `--help` output of `grok` and `opencode run` when adapting to another client version.

## Write and test an adapter

1. Make a small executable that reads a prompt, invokes the target agent through its supported API or CLI, and writes a final answer.
2. Add a TOML entry with an absolute command path.
3. Run `soudan --config /path/to/plugins.toml doctor`.
4. Run a short consultation, then a follow-up in the same room.
5. Test nonzero exit, malformed output, timeout, and excessive output before relying on the adapter.

For a deterministic transport smoke test on Unix, configure `command = "cat"`, `input = "stdin"`, and `output = "text"`. It echoes the assembled conversation without using an LLM.

Each turn launches a fresh CLI session. Soudan retains context by replaying up to 20 recent room messages, not by resuming a provider's private session ID. Native resume adapters, streaming output, remote transports, and richer plugin negotiation can be added in future versions without changing the room model.
