//! Derived receipt observations for append-only native session logs.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Basis {
    path: PathBuf,
    offset: u64,
    device: u64,
    inode: u64,
    anchor: Vec<u8>,
}
impl Basis {
    #[cfg(unix)]
    pub fn capture(path: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "Evidence is not a regular file");
        let offset = meta.len();
        let mut anchor = vec![0; offset.min(128) as usize];
        file.seek(SeekFrom::Start(offset - anchor.len() as u64))?;
        file.read_exact(&mut anchor)?;
        Ok(Self {
            path: path.into(),
            offset,
            device: meta.dev(),
            inode: meta.ino(),
            anchor,
        })
    }
    #[cfg(not(unix))]
    pub fn capture(_: &Path) -> Result<Self> {
        anyhow::bail!("Receipt evidence requires Unix file identity")
    }

    #[cfg(unix)]
    fn contains(&self, marker: &[u8]) -> Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let mut file = File::open(&self.path)?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.dev() == self.device
                && meta.ino() == self.inode
                && meta.len() >= self.offset,
            "Evidence was replaced or truncated"
        );
        ensure!(
            self.anchor.len() as u64 <= self.offset,
            "Invalid evidence anchor"
        );
        file.seek(SeekFrom::Start(self.offset - self.anchor.len() as u64))?;
        let mut anchor = vec![0; self.anchor.len()];
        file.read_exact(&mut anchor)?;
        ensure!(
            anchor == self.anchor,
            "Evidence changed before the send offset"
        );
        let mut remaining = meta.len() - self.offset;
        let mut carry = Vec::new();
        let mut found = false;
        while remaining > 0 {
            let count = remaining.min(65536) as usize;
            let old = carry.len();
            carry.resize(old + count, 0);
            file.read_exact(&mut carry[old..])?;
            found |= carry.windows(marker.len()).any(|w| w == marker);
            remaining -= count as u64;
            let keep = carry.len().min(marker.len() - 1);
            carry.drain(..carry.len() - keep);
        }
        let current = std::fs::metadata(&self.path)?;
        ensure!(
            current.dev() == self.device
                && current.ino() == self.inode
                && current.len() >= meta.len(),
            "Evidence changed during observation"
        );
        Ok(found)
    }
    #[cfg(not(unix))]
    fn contains(&self, _: &[u8]) -> Result<bool> {
        anyhow::bail!("Unsupported evidence platform")
    }
}

pub fn unknown(reason: &str) -> Value {
    json!({"status":"unknown", "reason":reason})
}

/// Observe a complete log range and the original process, without changing either.
/// `proc` is injectable so tests never depend on real agent processes.
pub fn observe(basis: Option<&Basis>, proc: &Path, target: &str, id: &str) -> Value {
    let result = || -> Result<Value> {
        let parts: Vec<_> = target.split(':').collect();
        ensure!(
            parts.len() == 3 && matches!(parts[0], "codex" | "claude" | "grok"),
            "Transport has no persistent receipt evidence"
        );
        let basis = basis.ok_or_else(|| {
            anyhow::anyhow!("No send-time evidence boundary; coverage is unproven")
        })?;
        let pid: u32 = parts[1].parse()?;
        let start: u64 = parts[2].parse()?;
        // Check process first, then scan: do not miss a final write made before exit.
        let alive = match crate::claude::process_start(proc, pid) {
            Ok(actual) => Some(actual == start),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Some(false)
            }
            Err(_) => None,
        };
        if basis.contains(format!("[Soudan {id}]").as_bytes())? {
            return Ok(
                json!({"status":"taken", "reason":"Request marker observed in the recipient log; this is not a reply acknowledgement"}),
            );
        }
        match alive {
            Some(false) => Ok(
                json!({"status":"lost", "reason":"No marker since send; the original recipient process exited or was replaced"}),
            ),
            None => Ok(unknown("Recipient process identity cannot be read")),
            Some(true) if parts[0] == "codex" => {
                let state = crate::codex::state(&basis.path)?;
                Ok(match state["status"].as_str() {
                    Some("aborted") => {
                        json!({"status":"blocked", "reason":"Codex's turn is interrupted. The queue will not be collected until someone interacts with that terminal; waiting alone will not deliver the message."})
                    }
                    Some("idle" | "running") => {
                        json!({"status":"waiting", "reason":"No marker yet; the original recipient process is alive"})
                    }
                    _ => unknown("Codex state is unknown"),
                })
            }
            // Grok's leader keeps driving a session after a client disconnects, so
            // an interrupted turn does not hold a handover the way Codex's does.
            Some(true) if parts[0] == "grok" => Ok(
                json!({"status":"waiting", "reason":"No marker yet; the original Grok process is alive and its leader drives the session independently of any client"}),
            ),
            Some(true) => Ok(
                json!({"status":"waiting", "reason":"No marker yet; the original Claude process is alive. Inbound policy may still hold or refuse the message."}),
            ),
        }
    };
    result().unwrap_or_else(|e| unknown(&format!("{e:#}")))
}
