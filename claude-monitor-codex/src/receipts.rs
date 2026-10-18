//! Gateway-owned durable submission receipts. Codex stores are never opened here.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) struct Receipts(Connection);

const RETAIN_SECONDS: i64 = 30 * 24 * 60 * 60;
const MAX_IDENTITIES: i64 = 100_000;

#[derive(Debug, thiserror::Error)]
#[error("Native receipt identity capacity reached; preserve this database and reconcile it before enabling more actions")]
pub(super) struct ReceiptCapacity;

/// Persist only bounded status and identity fields, never native history or queued input.
fn compact(response: &Value) -> Value {
    let mut result = json!({"ok":response["ok"] == true});
    for key in [
        "receipt",
        "error",
        "queuedSubmission",
        "thread",
        "turn",
        "project",
    ] {
        if let Some(object) = response[key].as_object() {
            let mut fields = serde_json::Map::new();
            for field in [
                "id",
                "actionId",
                "status",
                "code",
                "message",
                "clientUserMessageId",
                "threadId",
                "turnId",
            ] {
                if let Some(value) = object.get(field).and_then(Value::as_str) {
                    fields.insert(
                        field.into(),
                        json!(value
                            .chars()
                            .take(if field == "message" { 1024 } else { 256 })
                            .collect::<String>()),
                    );
                }
            }
            if !fields.is_empty() {
                result[key] = Value::Object(fields);
            }
        }
    }
    for key in [
        "threadId",
        "turnId",
        "queuedSubmissionId",
        "clientUserMessageId",
        "projectId",
    ] {
        if let Some(value) = response[key].as_str() {
            result[key] = json!(value.chars().take(256).collect::<String>());
        }
    }
    for key in ["deleted", "delivered"] {
        if let Some(value) = response[key].as_bool() {
            result[key] = json!(value);
        }
    }
    if let Some(id) = response["snapshot"]["thread"]["id"].as_str() {
        result["threadId"] = json!(id.chars().take(256).collect::<String>());
    }
    result
}

fn prune(connection: &Connection) -> Result<()> {
    // Use one cutoff for both statements: a clock tick between INSERT and DELETE must
    // never delete an identity that was not copied to the retired table.
    let cutoff: i64 = connection.query_row("SELECT unixepoch() - ?1", [RETAIN_SECONDS], |row| {
        row.get(0)
    })?;
    connection.execute(
        "INSERT OR IGNORE INTO retired_actions(action_id,fingerprint)
        SELECT action_id,fingerprint FROM actions WHERE status != 'uncertain' AND updated_at < ?1",
        [cutoff],
    )?;
    connection.execute(
        "DELETE FROM actions WHERE status != 'uncertain' AND updated_at < ?1",
        [cutoff],
    )?;
    Ok(())
}

#[derive(Debug)]
pub(super) struct ActionConflict;

impl std::fmt::Display for ActionConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("actionId already belongs to a different request or runtime endpoint")
    }
}
impl std::error::Error for ActionConflict {}

impl Receipts {
    /// Count both stores in one SQLite read snapshot, including retired action identities.
    pub fn usage(&self) -> Result<crate::ReceiptUsage> {
        let (actions, queue_steers): (i64, i64) = self
            .0
            .query_row(
                "SELECT (SELECT count(*) FROM actions)+(SELECT count(*) FROM retired_actions),
                    (SELECT count(*) FROM queue_steers)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("Count active/retired action and queue-steer identities")?;
        Ok(crate::ReceiptUsage {
            action_identities: actions
                .try_into()
                .context("Negative native action identity count")?,
            queue_steer_identities: queue_steers
                .try_into()
                .context("Negative native queue-steer identity count")?,
            limit_per_store: MAX_IDENTITIES as u64,
            warning_at: (MAX_IDENTITIES * 9 / 10) as u64,
        })
    }

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
        let mut connection = Connection::open(path).context("open Gateway action receipts")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .context("restrict Gateway receipt database to its owner")?;
        }
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 1 {
            bail!("Unsupported native receipt schema version {version}");
        }
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA secure_delete=ON;")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS actions (
                action_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,
                status TEXT NOT NULL, response TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS queue_steers (
                endpoint TEXT NOT NULL, thread_id TEXT NOT NULL,
                submission_id TEXT NOT NULL, action_id TEXT NOT NULL,
                PRIMARY KEY(endpoint,thread_id,submission_id));
            CREATE TABLE IF NOT EXISTS retired_actions (
                action_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL);",
        )?;
        // Recheck under the write transaction: another Monitor may just have migrated it.
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            transaction.execute_batch(
                "ALTER TABLE actions ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
                UPDATE actions SET updated_at=unixepoch(); PRAGMA user_version=1;",
            )?;
            let mut query = transaction.prepare("SELECT action_id,response FROM actions")?;
            let rows = query.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, response) = row?;
                let response: Value =
                    serde_json::from_str(&response).context("Migrate native receipt JSON")?;
                transaction.execute(
                    "UPDATE actions SET response=?2 WHERE action_id=?1",
                    params![id, compact(&response).to_string()],
                )?;
            }
        }
        transaction.execute_batch("CREATE INDEX IF NOT EXISTS actions_expiry ON actions(updated_at) WHERE status != 'uncertain';")?;
        prune(&transaction)?;
        transaction.commit()?;
        // Reclaim pages formerly occupied by conversation snapshots during the one-time migration.
        if version == 0 {
            connection.execute_batch(
                "PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);",
            )?;
        }
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
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune(&transaction)?;
        let retired: Option<String> = transaction
            .query_row(
                "SELECT fingerprint FROM retired_actions WHERE action_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(previous) = retired {
            if previous != fingerprint {
                return Err(ActionConflict.into());
            }
            transaction.commit()?;
            return Ok(Some(json!({"ok":false,"error":{"code":"receipt_expired",
                "message":"The receipt expired. Reconcile native history; this action will not be delivered again."},
                "receipt":{"actionId":id,"status":"expired"}})));
        }
        let prior: Option<(String, String)> = transaction
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
            let response = serde_json::from_str(&response)?;
            transaction.commit()?;
            return Ok(Some(response));
        }
        let count: i64 = transaction.query_row(
            "SELECT (SELECT count(*) FROM actions)+(SELECT count(*) FROM retired_actions)",
            [],
            |row| row.get(0),
        )?;
        if count >= MAX_IDENTITIES {
            transaction.commit()?;
            return Err(ReceiptCapacity.into());
        }
        let response = json!({"ok":false,"error":{"code":"uncertain",
            "message":"Delivery has not been confirmed. Reconcile this action with runtime history; do not resend it with another action ID."},
            "receipt":{"actionId":id,"status":"uncertain"}});
        transaction.execute("INSERT INTO actions(action_id,fingerprint,status,response,updated_at) VALUES(?1,?2,'uncertain',?3,unixepoch())",
            params![id, fingerprint, response.to_string()])?;
        transaction.commit()?;
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
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM queue_steers WHERE endpoint=?1 AND thread_id=?2 AND submission_id=?3)",
            params![endpoint, thread_id, submission_id], |row| row.get(0))?;
        if exists {
            return Ok(false);
        }
        let count: i64 =
            transaction.query_row("SELECT count(*) FROM queue_steers", [], |row| row.get(0))?;
        if count >= MAX_IDENTITIES {
            return Err(ReceiptCapacity.into());
        }
        transaction.execute(
            "INSERT INTO queue_steers(endpoint,thread_id,submission_id,action_id) VALUES(?1,?2,?3,?4)",
            params![endpoint, thread_id, submission_id, action_id],
        ).context("claim native queued message for steering")?;
        transaction.commit()?;
        Ok(true)
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
            "UPDATE actions SET status=?2,response=?3,updated_at=unixepoch() WHERE action_id=?1",
            params![id, status, compact(response).to_string()],
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
    #[test]
    fn compact_receipts_expire_without_replaying_and_uncertain_delivery_is_retained() {
        let root = std::env::temp_dir().join(format!("receipt-retention-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        let mut store = Receipts::open(&root.join("receipts.sqlite3")).unwrap();
        let payload = json!({"threadId":"t"});
        store
            .claim("finished", "endpoint", "settings", &payload)
            .unwrap();
        store
            .finish(
                "finished",
                "accepted",
                &json!({"ok":true,
            "receipt":{"actionId":"finished","status":"accepted"},
            "snapshot":{"thread":{"id":"t","turns":[{"text":"PRIVATE HISTORY".repeat(10000)}]}}}),
            )
            .unwrap();
        let stored = store
            .claim("finished", "endpoint", "settings", &payload)
            .unwrap()
            .unwrap();
        assert_eq!(stored["threadId"], "t");
        assert!(stored.get("snapshot").is_none());
        assert!(stored.to_string().len() < 256);
        store
            .claim("unknown", "endpoint", "send", &payload)
            .unwrap();
        store
            .0
            .execute("UPDATE actions SET updated_at=0", [])
            .unwrap();
        assert_eq!(
            store
                .claim("finished", "endpoint", "settings", &payload)
                .unwrap()
                .unwrap()["receipt"]["status"],
            "expired"
        );
        assert_eq!(
            store
                .0
                .query_row(
                    "SELECT count(*) FROM actions WHERE action_id='finished'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .claim("unknown", "endpoint", "send", &payload)
                .unwrap()
                .unwrap()["receipt"]["status"],
            "uncertain"
        );
        drop(store);
        let mut store = Receipts::open(&root.join("receipts.sqlite3")).unwrap();
        assert_eq!(
            store
                .claim("finished", "endpoint", "settings", &payload)
                .unwrap()
                .unwrap()["receipt"]["status"],
            "expired"
        );
        assert!(store
            .claim("finished", "endpoint", "settings", &json!({}))
            .is_err());
        drop(store);
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn two_connections_claim_one_delivery_and_migrate_legacy_snapshots() {
        let root = std::env::temp_dir().join(format!("receipt-concurrent-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("receipts.sqlite3");
        let legacy = Connection::open(&path).unwrap();
        legacy.execute_batch("CREATE TABLE actions(action_id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,status TEXT NOT NULL,response TEXT NOT NULL);").unwrap();
        legacy
            .execute(
                "INSERT INTO actions VALUES('legacy','hash','accepted',?1)",
                [
                    json!({"ok":true,"snapshot":{"thread":{"id":"t","turns":["PRIVATE HISTORY"]}}})
                        .to_string(),
                ],
            )
            .unwrap();
        drop(legacy);
        let mut one = Receipts::open(&path).unwrap();
        assert_eq!(
            one.0
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let response: String = one
            .0
            .query_row(
                "SELECT response FROM actions WHERE action_id='legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!response.contains("PRIVATE HISTORY"));
        let mut two = Receipts::open(&path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let second = barrier.clone();
        let worker = std::thread::spawn(move || {
            second.wait();
            two.claim("shared", "endpoint", "send", &json!({}))
                .unwrap()
                .is_none()
        });
        barrier.wait();
        let first = one
            .claim("shared", "endpoint", "send", &json!({}))
            .unwrap()
            .is_none();
        assert_ne!(first, worker.join().unwrap());
        one.0.pragma_update(None, "user_version", 2).unwrap();
        drop(one);
        assert!(Receipts::open(&path).is_err());
        crate::remove_test_scratch(&root);
    }
    #[test]
    fn identity_capacity_refuses_new_delivery_but_keeps_existing_receipts() {
        let root = std::env::temp_dir().join(format!("receipt-capacity-{}", std::process::id()));
        crate::remove_test_scratch(&root);
        let mut store = Receipts::open(&root.join("receipts.sqlite3")).unwrap();
        store
            .claim("existing", "endpoint", "send", &json!({}))
            .unwrap();
        let initial = store.usage().unwrap();
        assert_eq!(
            (initial.action_identities, initial.queue_steer_identities),
            (1, 0)
        );
        assert_eq!(
            (initial.warning_at, initial.limit_per_store),
            (90_000, 100_000)
        );
        store.0.execute("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n < 89998)
            INSERT INTO retired_actions SELECT 'old-' || n, 'hash' FROM numbers", []).unwrap();
        assert_eq!(store.usage().unwrap().action_identities, 89_999);
        assert!(!store.usage().unwrap().should_warn());
        store
            .0
            .execute("INSERT INTO retired_actions VALUES('old-89999','hash')", [])
            .unwrap();
        assert!(store.usage().unwrap().should_warn());
        store.0.execute("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n < ?1)
            INSERT OR IGNORE INTO retired_actions SELECT 'old-' || n, 'hash' FROM numbers", [MAX_IDENTITIES-1]).unwrap();
        assert_eq!(store.usage().unwrap().action_identities, 100_000);
        assert!(store
            .claim("new", "endpoint", "send", &json!({}))
            .unwrap_err()
            .is::<ReceiptCapacity>());
        assert_eq!(
            store
                .claim("existing", "endpoint", "send", &json!({}))
                .unwrap()
                .unwrap()["receipt"]["status"],
            "uncertain"
        );
        store.0.execute("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n < ?1)
            INSERT INTO queue_steers SELECT 'endpoint','t','submission-' || n,'action' FROM numbers", [MAX_IDENTITIES]).unwrap();
        assert!(!store
            .claim_queue_steer("endpoint", "t", "submission-1", "different-action")
            .unwrap());
        assert!(store
            .claim_queue_steer("endpoint", "t", "new", "new-action")
            .unwrap_err()
            .is::<ReceiptCapacity>());
        store.0.execute("DELETE FROM actions", []).unwrap();
        store.0.execute("DELETE FROM retired_actions", []).unwrap();
        let queued = store.usage().unwrap();
        assert_eq!(
            (queued.action_identities, queued.queue_steer_identities),
            (0, 100_000)
        );
        assert!(queued.should_warn());
        drop(store);
        crate::remove_test_scratch(&root);
    }
}
