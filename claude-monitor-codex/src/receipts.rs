//! Gateway-owned durable submission receipts. Codex stores are never opened here.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) struct Receipts(Connection);

#[derive(Debug)]
pub(super) struct ActionConflict;

impl std::fmt::Display for ActionConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("actionId already belongs to a different request or runtime endpoint")
    }
}
impl std::error::Error for ActionConflict {}

impl Receipts {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o600)
                .open(path)
                .context("create owner-only Gateway receipt database")?;
        }
        let connection = Connection::open(path).context("open Gateway action receipts")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .context("restrict Gateway receipt database to its owner")?;
        }
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS actions (
                action_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,
                status TEXT NOT NULL, response TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS queue_steers (
                endpoint TEXT NOT NULL, thread_id TEXT NOT NULL,
                submission_id TEXT NOT NULL, action_id TEXT NOT NULL,
                PRIMARY KEY(endpoint,thread_id,submission_id));",
        )?;
        Ok(Self(connection))
    }

    /// Claim before delivery. A process crash leaves an uncertain receipt; that action is
    /// never replayed automatically because the runtime may have accepted it.
    pub fn claim(
        &mut self,
        id: &str,
        endpoint: &str,
        route: &str,
        payload: &Value,
    ) -> Result<Option<Value>> {
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(format!("{endpoint}\0{route}\0{payload}").as_bytes())
        );
        let prior: Option<(String, String)> = self
            .0
            .query_row(
                "SELECT fingerprint,response FROM actions WHERE action_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((previous, response)) = prior {
            if previous != fingerprint {
                return Err(ActionConflict.into());
            }
            return Ok(Some(serde_json::from_str(&response)?));
        }
        let response = json!({"ok":false,"error":{"code":"uncertain",
            "message":"Delivery has not been confirmed. Reconcile this action with runtime history; do not resend it with another action ID."},
            "receipt":{"actionId":id,"status":"uncertain"}});
        self.0.execute("INSERT INTO actions(action_id,fingerprint,status,response) VALUES(?1,?2,'uncertain',?3)",
            params![id, fingerprint, response.to_string()])?;
        Ok(None)
    }

    /// Bind a native queued submission before steering. Keep the binding after acceptance,
    /// disconnect or process crash so another device cannot steer the same queued copy again.
    pub fn claim_queue_steer(
        &mut self,
        endpoint: &str,
        thread_id: &str,
        submission_id: &str,
        action_id: &str,
    ) -> Result<bool> {
        Ok(self.0.execute(
            "INSERT OR IGNORE INTO queue_steers(endpoint,thread_id,submission_id,action_id) VALUES(?1,?2,?3,?4)",
            params![endpoint, thread_id, submission_id, action_id],
        ).context("claim native queued message for steering")? == 1)
    }

    /// A definite native rejection permits a fresh action; uncertain delivery never does.
    pub fn release_queue_steer(
        &mut self,
        endpoint: &str,
        thread_id: &str,
        submission_id: &str,
        action_id: &str,
    ) -> Result<()> {
        self.0.execute(
            "DELETE FROM queue_steers WHERE endpoint=?1 AND thread_id=?2 AND submission_id=?3 AND action_id=?4",
            params![endpoint, thread_id, submission_id, action_id],
        ).context("release definitely rejected queued steering")?;
        Ok(())
    }

    pub fn finish(&mut self, id: &str, status: &str, response: &Value) -> Result<()> {
        let changed = self.0.execute(
            "UPDATE actions SET status=?2,response=?3 WHERE action_id=?1",
            params![id, status, response.to_string()],
        )?;
        if changed != 1 {
            bail!("cannot complete unclaimed Gateway action {id}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_steer_claim_survives_crash_and_only_its_rejected_action_can_release_it() {
        let root =
            std::env::temp_dir().join(format!("gateway-queue-receipts-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        let path = root.join("receipts.sqlite3");
        let mut store = Receipts::open(&path).unwrap();
        assert!(store
            .claim_queue_steer("ws://one", "thread", "native", "action")
            .unwrap());
        drop(store);
        let mut store = Receipts::open(&path).unwrap();
        assert!(!store
            .claim_queue_steer("ws://one", "thread", "native", "other-device")
            .unwrap());
        store
            .release_queue_steer("ws://one", "thread", "native", "other-device")
            .unwrap();
        assert!(!store
            .claim_queue_steer("ws://one", "thread", "native", "retry")
            .unwrap());
        assert!(store
            .claim_queue_steer("ws://two", "thread", "native", "different-server")
            .unwrap());
        assert!(store
            .claim_queue_steer("ws://one", "other-thread", "native", "different-thread")
            .unwrap());
        store
            .release_queue_steer("ws://one", "thread", "native", "action")
            .unwrap();
        assert!(store
            .claim_queue_steer("ws://one", "thread", "native", "after-rejection")
            .unwrap());
        drop(store);
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn restart_never_replays_an_uncertain_or_accepted_action() {
        let root = std::env::temp_dir().join(format!("gateway-receipts-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        let path = root.join("receipts.sqlite3");
        let payload = json!({"text":"one"});
        let mut store = Receipts::open(&path).unwrap();
        assert!(store
            .claim("a", "ws://one", "send", &payload)
            .unwrap()
            .is_none());
        drop(store);
        let mut store = Receipts::open(&path).unwrap();
        assert_eq!(
            store
                .claim("a", "ws://one", "send", &payload)
                .unwrap()
                .unwrap()["receipt"]["status"],
            "uncertain"
        );
        assert!(store
            .claim("a", "ws://one", "send", &json!({"text":"two"}))
            .is_err());
        assert!(store.claim("a", "ws://two", "send", &payload).is_err());
        let accepted = json!({"ok":true,"receipt":{"status":"accepted"}});
        store.finish("a", "accepted", &accepted).unwrap();
        assert_eq!(
            store
                .claim("a", "ws://one", "send", &payload)
                .unwrap()
                .unwrap(),
            accepted
        );
        assert!(store.finish("not-claimed", "accepted", &accepted).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(store);
        crate::remove_test_scratch(&root);
    }
}
