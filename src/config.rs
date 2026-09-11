use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Plugin {
    pub command: String,
    pub args: Vec<String>,
    pub input: Input,
    pub output: Output,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Input {
    #[default]
    Argument,
    Stdin,
    None,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    #[default]
    Text,
    ResultJson,
}
impl Default for Plugin {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: vec![],
            input: Input::Argument,
            output: Output::Text,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub agents: BTreeMap<String, Plugin>,
}
impl Default for Config {
    fn default() -> Self {
        let definitions = [
            (
                "claude-code",
                "claude",
                vec![
                    "-p",
                    "--output-format",
                    "json",
                    "--tools",
                    "",
                    "--strict-mcp-config",
                    "--setting-sources",
                    "",
                ],
                Input::Stdin,
                Output::ResultJson,
            ),
            (
                "codex",
                "codex",
                vec![
                    "exec",
                    "--ephemeral",
                    "--sandbox",
                    "read-only",
                    "--color",
                    "never",
                    "-",
                ],
                Input::Stdin,
                Output::Text,
            ),
            (
                "cursor",
                "cursor-agent",
                vec!["-p", "--mode", "ask", "--output-format", "json", "--trust"],
                Input::Argument,
                Output::ResultJson,
            ),
            // Grok Build prints the answer alone on stdout and keeps its banner
            // on stderr. Its `--output-format json` reports the answer as
            // `text`, not the `result` field Output::ResultJson reads, so the
            // plain format is the stable contract. `--tools ""` removes the
            // built-in tools a consultation has no use for.
            (
                "grok",
                "grok",
                vec!["--output-format", "plain", "--tools", "", "-p"],
                Input::Argument,
                Output::Text,
            ),
            // OpenCode's `--format json` is an NDJSON event stream rather than
            // one object, so the default format is used and the answer arrives
            // alone on stdout. `--agent plan` selects the read-only agent, and
            // `--pure` skips external plugins for a predictable run.
            (
                "opencode",
                "opencode",
                vec!["run", "--pure", "--agent", "plan"],
                Input::Argument,
                Output::Text,
            ),
            // GitHub Copilot CLI's `--output-format json` is JSONL, so the text
            // format is used. A non-interactive run cannot answer a permission
            // prompt, so shell and write access are denied outright instead of
            // being left to a prompt nobody can confirm.
            (
                "copilot",
                "copilot",
                vec![
                    "--output-format",
                    "text",
                    "--no-color",
                    "--log-level",
                    "none",
                    "--deny-tool=shell",
                    "--deny-tool=write",
                    "-p",
                ],
                Input::Argument,
                Output::Text,
            ),
        ];
        Self {
            agents: definitions
                .into_iter()
                .map(|(name, command, args, input, output)| {
                    (
                        name.into(),
                        Plugin {
                            command: command.into(),
                            args: args.into_iter().map(String::from).collect(),
                            input,
                            output,
                        },
                    )
                })
                .collect(),
        }
    }
}
impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let mut config = Self::default();
        if !text.trim().is_empty() {
            let custom: Self = toml::from_str(text)?;
            for (name, plugin) in custom.agents {
                ensure!(
                    !name.trim().is_empty() && name.len() <= 128,
                    "Invalid agent name"
                );
                ensure!(
                    !plugin.command.trim().is_empty(),
                    "Agent {name} needs a command"
                );
                config.agents.insert(name, plugin);
            }
        }
        Ok(config)
    }
}
