use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{fs, path::Path};

pub fn install(
    workspace: &Path,
    executable: &Path,
    client: &str,
    config: Option<&Path>,
) -> Result<Vec<String>> {
    ensure!(
        ["all", "claude-code", "codex", "cursor"].contains(&client),
        "Unknown client: {client}"
    );
    let mut args = vec![
        "--workspace".to_string(),
        workspace.to_string_lossy().into_owned(),
    ];
    if let Some(config) = config {
        let config = config.canonicalize()?;
        crate::Config::parse(&fs::read_to_string(&config)?)?;
        args.extend([
            "--config".to_string(),
            config.to_string_lossy().into_owned(),
        ]);
    }
    args.push("serve".to_string());
    let mut writes = vec![];
    for (name, relative) in [("claude-code", ".mcp.json"), ("cursor", ".cursor/mcp.json")] {
        if client != "all" && client != name {
            continue;
        }
        let path = workspace.join(relative);
        let mut value = if path.exists() {
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&path)?)
                .with_context(|| format!("Invalid JSON in {}", path.display()))?
        } else {
            json!({})
        };
        let root = value
            .as_object_mut()
            .context("MCP config must be an object")?;
        let servers = root
            .entry("mcpServers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("mcpServers must be an object")?;
        let server = servers
            .entry("soudan")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("soudan must be an object")?;
        server.remove("url");
        if server.contains_key("type") {
            server.insert("type".into(), json!("stdio"));
        }
        server.insert("command".into(), json!(executable));
        server.insert("args".into(), json!(args));
        writes.push((path, format!("{}\n", serde_json::to_string_pretty(&value)?)));
    }
    if client == "all" || client == "codex" {
        let path = workspace.join(".codex/config.toml");
        let mut value: toml::Value = if path.exists() {
            toml::from_str(&fs::read_to_string(&path)?)?
        } else {
            toml::Value::Table(Default::default())
        };
        let root = value
            .as_table_mut()
            .context("Codex config must be a table")?;
        let servers = root
            .entry("mcp_servers")
            .or_insert_with(|| toml::Value::Table(Default::default()))
            .as_table_mut()
            .context("mcp_servers must be a table")?;
        let server = servers
            .entry("soudan")
            .or_insert_with(|| toml::Value::Table(Default::default()))
            .as_table_mut()
            .context("soudan must be a table")?;
        server.remove("url");
        server.insert(
            "command".into(),
            toml::Value::String(executable.to_string_lossy().into()),
        );
        server.insert(
            "args".into(),
            toml::Value::Array(args.iter().cloned().map(toml::Value::String).collect()),
        );
        writes.push((path, toml::to_string_pretty(&value)?));
    }
    let mut paths = vec![];
    for (path, content) in writes {
        fs::create_dir_all(path.parent().context("Config has no parent")?)?;
        if path.exists() && fs::read_to_string(&path)? != content {
            fs::copy(
                &path,
                path.with_extension(format!("backup-{}", uuid::Uuid::new_v4())),
            )?;
        }
        let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        use std::io::Write;
        file.write_all(content.as_bytes())?;
        if path.exists() {
            fs::set_permissions(&temporary, fs::metadata(&path)?.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        paths.push(path.display().to_string());
    }
    Ok(paths)
}
