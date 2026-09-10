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
