use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{path::Path, sync::Mutex, time::Duration};

pub struct Store {
    connection: Mutex<Connection>,
}
#[derive(Debug, Serialize)]
pub struct Message {
    pub id: i64,
    pub room: String,
    pub sender: String,
    pub text: String,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL;
          CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY AUTOINCREMENT, room TEXT NOT NULL, sender TEXT NOT NULL, text TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS messages_room ON messages(room,id);
          CREATE TABLE IF NOT EXISTS requests(scope TEXT NOT NULL, key TEXT NOT NULL, fingerprint TEXT NOT NULL, result TEXT NOT NULL, PRIMARY KEY(scope,key));
          CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, room TEXT NOT NULL, agent TEXT NOT NULL, prompt TEXT NOT NULL, timeout INTEGER NOT NULL, deadline INTEGER NOT NULL, status TEXT NOT NULL, result TEXT, error TEXT);")?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }
    pub fn post(&self, room: &str, sender: &str, text: &str) -> Result<i64> {
        self.post_once(room, sender, text, None)
    }
    pub fn post_once(
        &self,
        room: &str,
        sender: &str,
        text: &str,
        request_id: Option<&str>,
    ) -> Result<i64> {
        ensure!(
            !room.trim().is_empty() && room.len() <= 128,
            "Room must contain 1–128 bytes"
        );
        ensure!(
            !sender.trim().is_empty() && sender.len() <= 128,
            "Sender must contain 1–128 bytes"
        );
        ensure!(
            !text.trim().is_empty() && text.len() <= 65536,
            "Message must contain 1–65536 bytes"
        );
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let scope = serde_json::to_string(&("post", room, sender))?;
        if let Some(id) = previous(&tx, &scope, request_id, text)? {
            return Ok(id.parse()?);
        }
        tx.execute(
            "INSERT INTO messages(room,sender,text) VALUES(?1,?2,?3)",
            params![room, sender, text],
        )?;
        let id = tx.last_insert_rowid();
        remember(&tx, &scope, request_id, text, &id.to_string())?;
        tx.commit()?;
        Ok(id)
    }
    pub fn history(&self, room: &str, after: i64) -> Result<Vec<Message>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        let mut stmt = conn.prepare("SELECT id,room,sender,text FROM messages WHERE room=?1 AND id>?2 ORDER BY id LIMIT 100")?;
        Ok(stmt
            .query_map(params![room, after], |r| {
                Ok(Message {
                    id: r.get(0)?,
                    room: r.get(1)?,
                    sender: r.get(2)?,
                    text: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn context(&self, room: &str) -> Result<String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        let mut stmt = conn.prepare("SELECT sender,text FROM (SELECT id,sender,text FROM messages WHERE room=?1 ORDER BY id DESC LIMIT 20) ORDER BY id")?;
        let rows = stmt.query_map([room], |r| {
            Ok(serde_json::json!({"sender": r.get::<_,String>(0)?, "text": r.get::<_,String>(1)?}))
        })?;
        let mut history = vec![];
        let mut size = 0;
        for row in rows {
            let row = row?;
            size += row.to_string().len();
            ensure!(
                size <= 131072,
                "Recent conversation exceeds 128 KiB; use a new room"
            );
            history.push(row);
        }
        Ok(serde_json::to_string(&history)?)
    }
}

#[derive(Debug, Serialize)]
pub struct Job {
    pub id: String,
    pub room: String,
    pub agent: String,
    pub prompt: String,
    pub timeout: u64,
    pub status: String,
    pub result: Option<String>,
    pub error: Option<String>,
}
impl Store {
    pub fn create_job(
        &self,
        room: &str,
        agent: &str,
        prompt: &str,
        timeout: u64,
    ) -> Result<String> {
        self.create_job_once(room, agent, prompt, timeout, None)
    }
    pub fn create_job_once(
        &self,
        room: &str,
        agent: &str,
        prompt: &str,
        timeout: u64,
        request_id: Option<&str>,
    ) -> Result<String> {
        ensure!(!room.trim().is_empty() && room.len() <= 128, "Invalid room");
        ensure!(
            !prompt.trim().is_empty() && prompt.len() <= 65536,
            "Prompt must contain 1–65536 bytes"
        );
        ensure!(
            (1..=600).contains(&timeout),
            "Timeout must be 1–600 seconds"
        );
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let fingerprint = serde_json::to_string(&(room, agent, prompt, timeout))?;
        if let Some(id) = previous(&tx, "consult", request_id, &fingerprint)? {
            return Ok(id);
        }
        tx.execute("UPDATE jobs SET status='failed',error='Worker interrupted or deadline exceeded' WHERE status IN ('queued','running') AND deadline < unixepoch()", [])?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status IN ('queued','running')",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            count < 4,
            "Four consultations are already active; poll their results first"
        );
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM jobs WHERE room=?1 AND status IN ('queued','running')",
            [room],
            |r| r.get(0),
        )?;
        ensure!(count == 0, "This room already has an active consultation");
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO jobs(id,room,agent,prompt,timeout,deadline,status) VALUES(?1,?2,?3,?4,?5,unixepoch()+?5+10,'queued')", params![id,room,agent,prompt,timeout as i64])?;
        tx.execute(
            "INSERT INTO messages(room,sender,text) VALUES(?1,'requester',?2)",
            params![room, prompt],
        )?;
        remember(&tx, "consult", request_id, &fingerprint, &id)?;
        tx.commit()?;
        Ok(id)
    }
    pub fn claim_job(&self, id: &str) -> Result<bool> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        Ok(conn.execute("UPDATE jobs SET status='running' WHERE id=?1 AND status='queued' AND deadline >= unixepoch()", [id])? == 1)
    }
    pub fn job(&self, id: &str) -> Result<Job> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        conn.execute("UPDATE jobs SET status='failed',error='Worker interrupted or deadline exceeded' WHERE id=?1 AND status IN ('queued','running') AND deadline < unixepoch()", [id])?;
        Ok(conn.query_row(
            "SELECT id,room,agent,prompt,timeout,status,result,error FROM jobs WHERE id=?1",
            [id],
            |r| {
                Ok(Job {
                    id: r.get(0)?,
                    room: r.get(1)?,
                    agent: r.get(2)?,
                    prompt: r.get(3)?,
                    timeout: r.get::<_, i64>(4)? as u64,
                    status: r.get(5)?,
                    result: r.get(6)?,
                    error: r.get(7)?,
                })
            },
        )?)
    }
    pub fn finish_job(&self, id: &str, result: std::result::Result<&str, &str>) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (status, text, error) = match result {
            Ok(text) => ("completed", Some(text), None),
            Err(error) => ("failed", None, Some(error)),
        };
        let updated = tx.execute("UPDATE jobs SET status=?2,result=?3,error=?4 WHERE id=?1 AND status IN ('queued','running')", params![id,status,text,error])?;
        if updated == 1
            && let Some(text) = text
        {
            tx.execute(
                "INSERT INTO messages(room,sender,text) SELECT room,agent,?2 FROM jobs WHERE id=?1",
                params![id, text],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn previous(
    tx: &Connection,
    scope: &str,
    key: Option<&str>,
    fingerprint: &str,
) -> Result<Option<String>> {
    let Some(key) = key else { return Ok(None) };
    ensure!(
        !key.trim().is_empty() && key.len() <= 128,
        "request_id must contain 1–128 bytes"
    );
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT fingerprint,result FROM requests WHERE scope=?1 AND key=?2",
            params![scope, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((saved, id)) => {
            ensure!(
                saved == fingerprint,
                "request_id was already used with different arguments"
            );
            Ok(Some(id))
        }
        None => Ok(None),
    }
}
fn remember(
    tx: &Connection,
    scope: &str,
    key: Option<&str>,
    fingerprint: &str,
    result: &str,
) -> Result<()> {
    if let Some(key) = key {
        tx.execute(
            "INSERT INTO requests(scope,key,fingerprint,result) VALUES(?1,?2,?3,?4)",
            params![scope, key, fingerprint, result],
        )?;
    }
    Ok(())
}
