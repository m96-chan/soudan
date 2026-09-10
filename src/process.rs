use crate::config::{Input, Output, Plugin};
use anyhow::{Context, Result, ensure};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

async fn bounded_read(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    reader.take(1_048_577).read_to_end(&mut bytes).await?;
    ensure!(bytes.len() <= 1_048_576, "Agent output exceeded 1 MiB");
    Ok(bytes)
}

pub async fn run_plugin(
    plugin: &Plugin,
    prompt: &str,
    workspace: &Path,
    timeout: u64,
) -> Result<String> {
    ensure!(
        (1..=600).contains(&timeout),
        "Timeout must be 1–600 seconds"
    );
    let mut command = Command::new(&plugin.command);
    command
        .args(&plugin.args)
        .current_dir(workspace)
        .env("SOUDAN_CHILD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    if matches!(plugin.input, Input::Argument) {
        command.arg(prompt);
    }
    let mut child = command.spawn().with_context(|| {
        format!(
            "Cannot start {}; install and authenticate the agent CLI",
            plugin.command
        )
    })?;
    #[cfg(unix)]
    let _group = ProcessGroup(child.id().context("Missing process ID")? as i32);
    let mut stdin = child.stdin.take().context("Missing stdin")?;
    let stdout = child.stdout.take().context("Missing stdout")?;
    let stderr = child.stderr.take().context("Missing stderr")?;
    let work = async {
        let write = async {
            if matches!(plugin.input, Input::Stdin) {
                stdin.write_all(prompt.as_bytes()).await?;
            }
            drop(stdin);
            Ok::<_, anyhow::Error>(())
        };
        let (_, stdout, stderr, status) =
            tokio::try_join!(write, bounded_read(stdout), bounded_read(stderr), async {
                Ok::<_, anyhow::Error>(child.wait().await?)
            })?;
        ensure!(
            status.success(),
            "Agent exited with {status}: {}",
            String::from_utf8_lossy(&stderr)
                .chars()
                .take(2000)
                .collect::<String>()
        );
        let text = String::from_utf8(stdout).context("Agent output is not UTF-8")?;
        let text = match plugin.output {
            Output::Text => text,
            Output::ResultJson => {
                let json: serde_json::Value =
                    serde_json::from_str(text.trim()).context("Agent did not return valid JSON")?;
                ensure!(
                    json.get("is_error").and_then(|v| v.as_bool()) != Some(true),
                    "Agent reported failure: {}",
                    json.get("result").unwrap_or(&json)
                );
                json.get("result")
                    .and_then(|v| v.as_str())
                    .context("Agent JSON has no string result")?
                    .to_owned()
            }
        };
        ensure!(!text.trim().is_empty(), "Agent returned an empty response");
        ensure!(text.len() <= 65536, "Agent response exceeds 64 KiB");
        Ok(text)
    };
    let result = tokio::time::timeout(Duration::from_secs(timeout), work).await;
    match result {
        Ok(Ok(text)) => Ok(text),
        other => {
            let _ = child.kill().await;
            match other {
                Ok(Err(e)) => Err(e),
                _ => anyhow::bail!("Agent timed out after {timeout} seconds"),
            }
        }
    }
}

// The CLI may start helper processes that retain our output pipes. Terminate the
// entire group on success, failure, timeout, or cancellation of this future.
#[cfg(unix)]
struct ProcessGroup(i32);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // SAFETY: a negative PID addresses the private process group created
        // above. No pointers are passed. ESRCH means it already exited.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}
