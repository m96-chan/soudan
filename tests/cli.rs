use std::{fs, process::Command};
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_soudan"))
}
#[test]
fn install_preserves_existing_servers_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".mcp.json"),
        r#"{"mcpServers":{"existing":{"command":"keep"}},"extra":true}"#,
    )
    .unwrap();
    for _ in 0..2 {
        let output = bin()
            .args([
                "--workspace",
                dir.path().to_str().unwrap(),
                "install",
                "--client",
                "all",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["existing"]["command"], "keep");
    assert_eq!(config["extra"], true);
    assert!(
        config["mcpServers"]["soudan"]["command"]
            .as_str()
            .unwrap()
            .ends_with("soudan")
    );
    assert!(dir.path().join(".cursor/mcp.json").exists());
    let codex = fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
    let codex: toml::Value = toml::from_str(&codex).unwrap();
    assert!(codex["mcp_servers"]["soudan"]["args"].is_array());
}
#[test]
fn malformed_config_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".mcp.json");
    fs::write(&path, "broken json").unwrap();
    assert!(
        !bin()
            .args([
                "--workspace",
                dir.path().to_str().unwrap(),
                "install",
                "--client",
                "claude-code"
            ])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "broken json");
}
#[test]
fn cli_can_post_and_read_room_messages() {
    let dir = tempfile::tempdir().unwrap();
    let output = bin()
        .args([
            "--workspace",
            dir.path().to_str().unwrap(),
            "post",
            "--room",
            "test",
            "--sender",
            "codex",
            "hello",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = bin()
        .args([
            "--workspace",
            dir.path().to_str().unwrap(),
            "history",
            "--room",
            "test",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let messages: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(messages[0]["text"], "hello");
}

#[test]
fn installer_carries_explicit_plugin_configuration_into_clients() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("custom.toml");
    fs::write(&plugins, "[agents.echo]\ncommand='cat'\n").unwrap();
    let output = bin()
        .args([
            "--workspace",
            dir.path().to_str().unwrap(),
            "--config",
            plugins.to_str().unwrap(),
            "install",
            "--client",
            "all",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    // The installer records an absolute configuration path, so the expectation
    // has to be canonical too. A temporary directory is reached through a
    // symbolic link on macOS, where TMPDIR lives under /var -> /private/var.
    let plugins = plugins.canonicalize().unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert!(
        value["mcpServers"]["soudan"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == plugins.to_str().unwrap())
    );
    let value: toml::Value =
        toml::from_str(&fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap())
            .unwrap();
    assert!(
        value["mcp_servers"]["soudan"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == plugins.to_str())
    );
}

#[cfg(unix)]
#[test]
fn installer_preserves_private_config_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".mcp.json");
    fs::write(&path, "{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        bin()
            .args([
                "--workspace",
                dir.path().to_str().unwrap(),
                "install",
                "--client",
                "claude-code"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn reinstall_preserves_soudan_client_options() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".mcp.json"),
        r#"{"mcpServers":{"soudan":{"command":"old","env":{"EXAMPLE":"keep"}}}}"#,
    )
    .unwrap();
    fs::create_dir(dir.path().join(".codex")).unwrap();
    fs::write(dir.path().join(".codex/config.toml"),"[mcp_servers.soudan]\ncommand='old'\n[mcp_servers.soudan.tools.soudan_history]\napproval_mode='approve'\n").unwrap();
    assert!(
        bin()
            .args(["--workspace", dir.path().to_str().unwrap(), "install"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(value["mcpServers"]["soudan"]["env"]["EXAMPLE"], "keep");
    let value: toml::Value =
        toml::from_str(&fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap())
            .unwrap();
    assert_eq!(
        value["mcp_servers"]["soudan"]["tools"]["soudan_history"]["approval_mode"].as_str(),
        Some("approve")
    );
}

#[test]
fn install_configures_grok_and_opencode_projects() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let output = bin()
            .args([
                "--workspace",
                dir.path().to_str().unwrap(),
                "install",
                "--client",
                "all",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Grok Build reads project MCP servers from ./.grok/config.toml; the shape
    // matches what `grok mcp add --scope project` writes.
    let grok: toml::Value =
        toml::from_str(&fs::read_to_string(dir.path().join(".grok/config.toml")).unwrap()).unwrap();
    let server = &grok["mcp_servers"]["soudan"];
    assert!(server["command"].as_str().unwrap().ends_with("soudan"));
    assert_eq!(
        server["args"].as_array().unwrap().last().unwrap().as_str(),
        Some("serve")
    );
    assert_eq!(server["enabled"].as_bool(), Some(true));
    // OpenCode reads opencode.json and takes the executable and its arguments
    // as one `command` array, per https://opencode.ai/config.json.
    let opencode: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("opencode.json")).unwrap())
            .unwrap();
    let server = &opencode["mcp"]["soudan"];
    assert_eq!(server["type"], "local");
    assert_eq!(server["enabled"], true);
    let command = server["command"].as_array().unwrap();
    assert!(command[0].as_str().unwrap().ends_with("soudan"));
    assert_eq!(command.last().unwrap(), "serve");
}

#[test]
fn opencode_install_keeps_other_settings_and_refuses_an_ambiguous_jsonc() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("opencode.json"),
        r#"{"mcp":{"existing":{"type":"local","command":["keep"]}},"model":"anthropic/claude"}"#,
    )
    .unwrap();
    let install = |dir: &std::path::Path| {
        bin()
            .args([
                "--workspace",
                dir.to_str().unwrap(),
                "install",
                "--client",
                "opencode",
            ])
            .status()
            .unwrap()
            .success()
    };
    assert!(install(dir.path()));
    let opencode: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("opencode.json")).unwrap())
            .unwrap();
    assert_eq!(opencode["model"], "anthropic/claude");
    assert_eq!(opencode["mcp"]["existing"]["command"][0], "keep");
    assert!(opencode["mcp"]["soudan"]["command"].is_array());
    // A commented opencode.jsonc cannot be rewritten without discarding the
    // comments, and writing opencode.json instead may be shadowed by it.
    // Refuse rather than silently pick one.
    fs::write(
        dir.path().join("opencode.jsonc"),
        "{\n  // keep this comment\n}\n",
    )
    .unwrap();
    assert!(!install(dir.path()));
}

#[test]
fn grok_install_preserves_unrelated_servers() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".grok")).unwrap();
    fs::write(
        dir.path().join(".grok/config.toml"),
        "[mcp_servers.existing]\ncommand = 'keep'\n",
    )
    .unwrap();
    assert!(
        bin()
            .args([
                "--workspace",
                dir.path().to_str().unwrap(),
                "install",
                "--client",
                "grok",
            ])
            .status()
            .unwrap()
            .success()
    );
    let grok: toml::Value =
        toml::from_str(&fs::read_to_string(dir.path().join(".grok/config.toml")).unwrap()).unwrap();
    assert_eq!(
        grok["mcp_servers"]["existing"]["command"].as_str(),
        Some("keep")
    );
    assert!(grok["mcp_servers"]["soudan"]["command"].is_str());
}

#[test]
fn copilot_install_uses_shared_workspace_config_and_preserves_tool_filter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".mcp.json");
    fs::write(
        &path,
        r#"{"mcpServers":{"other":{"command":"keep"},"soudan":{"tools":["soudan_history"]}}}"#,
    )
    .unwrap();
    for _ in 0..2 {
        let output = bin()
            .arg("--workspace")
            .arg(dir.path())
            .args(["install", "--client", "copilot"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(value["mcpServers"]["other"]["command"], "keep");
    assert_eq!(
        value["mcpServers"]["soudan"]["tools"],
        serde_json::json!(["soudan_history"])
    );
    assert_eq!(
        value["mcpServers"]["soudan"]["args"][1],
        dir.path().canonicalize().unwrap().to_str().unwrap()
    );
    assert!(!dir.path().join(".codex").exists());
    assert!(!dir.path().join(".cursor").exists());
}
