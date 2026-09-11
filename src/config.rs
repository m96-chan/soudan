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
                "copilot",
                "copilot",
                vec![
                    "--silent",
                    "--stream=off",
                    "--available-tools=",
                    "--disable-builtin-mcps",
                    "--no-ask-user",
                    "--no-auto-update",
                    "--no-custom-instructions",
                    "--prompt",
                ],
                Input::Argument,
                Output::Text,
            ),
            (
                "cursor",
                "cursor-agent",
                vec!["-p", "--mode", "ask", "--output-format", "json", "--trust"],
                Input::Argument,
                Output::ResultJson,
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
