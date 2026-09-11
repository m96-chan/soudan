# Client setup

## Prerequisites

Install and authenticate the desired CLIs through their official distributions:

- [Claude Code](https://code.claude.com/docs/en/setup): executable `claude`.
- [Codex](https://developers.openai.com/codex/cli/): executable `codex`.
- [Cursor Agent](https://cursor.com/docs/cli/overview): executable `cursor-agent`. If your installation only exposes `agent`, override the Cursor plugin as shown below.

Use `soudan doctor` to check executable discovery. This does not check authentication, subscription status, model availability, or network access. Test those with a short `soudan consult --agent NAME --wait 'Say hello'`.

Build and install Soudan:

```sh
cargo install --path /path/to/soudan --locked
cd /path/to/your/project
soudan install --client all
```

Ensure the agent CLIs are on the PATH inherited by your editor. GUI applications may have a different PATH from your shell. Use absolute plugin command paths in `soudan.toml` when necessary. MCP configuration uses an absolute Soudan path, so the MCP host does not need to find Soudan on PATH.

## Automatic project configuration

| Client selector | File written |
| --- | --- |
| `claude-code` | `.mcp.json` |
| `codex` | `.codex/config.toml` |
| `cursor` | `.cursor/mcp.json` |
| `all` | All three files |

The installer validates all selected configuration files before writing any of them, merges the `soudan` entry, retains other entries, and backs up changed originals next to their files. Repeated installation with the same settings is idempotent. JSON/TOML formatting is normalized; TOML comments are not retained in the rewritten file, but remain in the backup. Each replacement uses a temporary file and rename; the three-file operation is not a filesystem-wide transaction.

Restart or reload each client's MCP connection. This does not restart the editor or its existing chats automatically. Confirm that the `soudan_*` tools are visible, including three `soudan_live_*` tools after upgrading. Local client trust or approval settings still apply.

Project configuration includes machine-specific absolute paths. Keep these generated files out of source control, or replace the paths with team-appropriate configuration. Never check in `.soudan/`, which contains conversation data.

## Manual configuration

For Claude Code's `.mcp.json` or Cursor's `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "soudan": {
      "command": "/absolute/path/to/soudan",
      "args": ["--workspace", "/absolute/path/to/project", "serve"]
    }
  }
}
```

For Codex's `.codex/config.toml`:

```toml
[mcp_servers.soudan]
command = "/absolute/path/to/soudan"
args = ["--workspace", "/absolute/path/to/project", "serve"]
```

Codex only loads project configuration for trusted projects. You can also register a server with the clients' own commands:

```sh
claude mcp add --transport stdio --scope project soudan -- /absolute/path/to/soudan --workspace /absolute/path/to/project serve
codex mcp add soudan -- /absolute/path/to/soudan --workspace /absolute/path/to/project serve
```

The Codex CLI command uses its own configuration scope; the Soudan installer uses project scope. See [Codex MCP documentation](https://developers.openai.com/codex/mcp/) and [Claude Code MCP documentation](https://code.claude.com/docs/en/mcp).

## Use the sample skill

Copy [the sample SKILL.md](examples/soudan/SKILL.md) into the appropriate project skill directory:

```sh
# Run from the Soudan checkout; replace /path/to/project.
mkdir -p /path/to/project/.claude/skills/soudan
cp docs/examples/soudan/SKILL.md /path/to/project/.claude/skills/soudan/SKILL.md

mkdir -p /path/to/project/.agents/skills/soudan
cp docs/examples/soudan/SKILL.md /path/to/project/.agents/skills/soudan/SKILL.md

mkdir -p /path/to/project/.cursor/skills/soudan
cp docs/examples/soudan/SKILL.md /path/to/project/.cursor/skills/soudan/SKILL.md
```

Reload skill discovery as required by your client. You can also paste the skill's short workflow into your project's existing agent instructions. Soudan itself does not require a skill to function.

## Troubleshooting

- **No tools visible:** confirm the executable path, project trust, and MCP server enablement; reload the connection. Running `soudan serve` directly waits for MCP input and intentionally prints no banner.
- **Agent unavailable:** install its CLI or set an absolute `command` in `soudan.toml`. Restart the MCP server after changing plugins.
- **Login/model/network failure:** read `soudan_result.error`, authenticate the CLI interactively, and submit a new request ID. Soudan does not manage credentials.
- **Cursor executable named `agent`:** replace the built-in plugin using the full definition in [plugins.md](plugins.md), with `command = "agent"`.
- **Room already busy:** wait for its current consultation to finish before asking another agent. Other rooms can proceed concurrently.
- **No message in another client:** compare the absolute `--workspace` arguments. Different workspaces have different databases.
- **Open editor does not answer automatically:** ask its agent to read and reply to the room. An MCP server cannot assume an idle editor will execute tools without a turn.
- **Nested consultation rejected:** the responder should return its answer to the caller. Only the coordinating agent starts further consultations.
- **Interrupted worker:** jobs become failed when their timeout plus ten seconds has elapsed. Submit a new request ID to retry a failed consultation; an identical old ID returns its original failed job.

To uninstall the connection, remove only the `soudan` entry from the generated client configuration. Stop active jobs before removing the binary or deleting local state.

## Noninteractive tool permissions

Server discovery and permission to execute its tools are separate client settings. During live testing, both Codex and Cursor discovered Soudan but rejected noninteractive calls until Soudan-specific tool permissions were configured. You can approve calls interactively, or explicitly allow the desired Soudan tools for automation.

For Claude Code, local project settings can approve this server and its individual tools:

```json
{
  "enabledMcpjsonServers": ["soudan"],
  "permissions": {
    "allow": [
      "mcp__soudan__soudan_agents",
      "mcp__soudan__soudan_history",
      "mcp__soudan__soudan_post",
      "mcp__soudan__soudan_consult",
      "mcp__soudan__soudan_result"
    ]
  }
}
```

Merge this into `.claude/settings.local.json`. Project trust still applies. See [Claude Code settings](https://code.claude.com/docs/en/settings).

For Cursor CLI, merge these entries into `.cursor/cli.json`; both `allow` and `deny` arrays are required by the tested version:

```json
{
  "permissions": {
    "allow": [
      "Mcp(soudan:soudan_agents)",
      "Mcp(soudan:soudan_history)",
      "Mcp(soudan:soudan_post)",
      "Mcp(soudan:soudan_consult)",
      "Mcp(soudan:soudan_result)"
    ],
    "deny": []
  }
}
```

Preserve existing allow and deny entries. Enable the server with `cursor-agent mcp enable soudan`. These are CLI settings; editor approvals may need separate configuration. See [Cursor CLI permissions](https://cursor.com/docs/cli/reference/permissions).

For Codex, add per-tool approval settings to the existing `.codex/config.toml`:

```toml
[mcp_servers.soudan.tools.soudan_agents]
approval_mode = "approve"
[mcp_servers.soudan.tools.soudan_history]
approval_mode = "approve"
[mcp_servers.soudan.tools.soudan_post]
approval_mode = "approve"
[mcp_servers.soudan.tools.soudan_consult]
approval_mode = "approve"
[mcp_servers.soudan.tools.soudan_result]
approval_mode = "approve"
```

These settings apply only to the named tools. Allowing `soudan_consult` permits starting configured agent processes and consuming their normal provider usage. The installer preserves existing Soudan tool options on reinstall, but does not automatically add these permissions. See [Codex's configuration reference](https://developers.openai.com/codex/config-reference/).

For direct delivery into an already-open Claude Code or Codex chat, follow [the live chat guide](live-chats.md). Room posting alone does not trigger live delivery.
