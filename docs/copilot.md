# GitHub Copilot CLI

Soudan supports GitHub Copilot CLI as a consultation agent and MCP client.
The adapter targets the standalone `copilot` executable, not the legacy
`gh copilot` extension or the VS Code Copilot chat UI. CLI flags were checked
against locally installed version 1.0.83 on Linux on 2026-09-11.

## Setup

Install and authenticate Copilot CLI, then run in the desired workspace:

```sh
soudan doctor
soudan install --client copilot
soudan consult --agent copilot --room review --wait 'Review this design.'
```

`doctor` checks executable availability, not account authentication.
`install --client all` also configures Copilot. The project `.mcp.json` is shared
with Claude Code; installing either client preserves other servers and existing
Soudan options, including an explicit `tools` filter. Changed files are backed up.
Trust the workspace in Copilot, reload its MCP configuration, and approve tool
calls when prompted. No user-level Copilot configuration is installed.

## Consultation behavior

Each consultation launches a new CLI process in the selected workspace. The
question and recent room history are passed as one literal `--prompt` argument.
Soudan uses `--silent --stream=off` to receive plain response text, and the
existing plugin runner handles failures, timeouts, output limits, and process
cleanup. Authentication is inherited from the CLI environment.

The default uses `--available-tools=` to expose no tools, disables built-in MCP
servers and custom instructions, and disables interactive questions and automatic
updates. It does not grant blanket tool permissions. These flags are provider
settings, not an operating-system sandbox; the CLI still reads its configuration
and writes its own session state. The prompt may be visible in local process
arguments. Do not include credentials in consultation prompts.

To customize the model, copy the complete `[agents.copilot]` definition from
[the plugin contract](plugins.md) to `soudan.toml` and add the supported `--model`
argument before `--prompt`. An override replaces the entire built-in definition.
No model or reasoning effort is selected by Soudan.

## Existing chats

Consultations do not attach to an existing TUI chat. Copilot can use the installed
MCP tools to post and read shared room messages, but those posts do not wake an
idle chat. Copilot is not yet a `live` target. Its ACP server is a separate mode;
starting or resuming a session through ACP must not be presented as delivery to
an already-running TUI.

## References

- [CLI command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference)
- [Project MCP configuration](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers)
- [ACP server](https://docs.github.com/en/copilot/reference/copilot-cli-reference/acp-server)
