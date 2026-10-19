//! Runtime event reduction and bounded browser subscriptions.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};

const EVENT_CAPACITY: usize = 128;

/// A fresh directory decision made at native ingress, before locking/reducing state.
#[derive(Clone, Debug)]
pub(super) enum DirectoryScope {
    Unrestricted,
    Allowed { source: String },
    Rejected { cause: String },
}
impl DirectoryScope {
    fn permits(&self, source: &str) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Allowed { source: checked } => checked == source,
            Self::Rejected { .. } => false,
        }
    }
    fn reason(&self) -> &str {
        match self {
            Self::Rejected { cause } => cause,
            _ => "Execution directory changed during scope validation",
        }
    }
}

#[derive(Clone, Copy)]
struct LiveItem {
    epoch: u64,
    completed: bool,
}

struct ProvisionalItem {
    epoch: u64,
    sequence: u64,
    item: Value,
}

struct LivePlanUpdate {
    epoch: u64,
    sequence: u64,
    params: Value,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Connection {
    #[default]
    Connecting,
    Connected,
    Reconnecting,
}

impl Connection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    pub stopping: bool,
    pub shutdown_warnings: Vec<String>,
    pub request_namespace: String,
    pub connection: Connection,
    pub connection_error: Option<String>,
    pub epoch: u64,
    pub sequence: u64,
    pub threads: HashMap<String, Value>,
    pub settings: HashMap<String, Value>,
    pub settings_sequence: HashMap<String, u64>,
    pub queue_sequences: HashMap<String, u64>,
    pub pending: HashMap<String, Value>,
    pub subscribed: HashSet<String>,
    pub archived_threads: HashSet<String>,
    pub allowed_roots: Vec<PathBuf>,
    turn_sequences: HashMap<(String, String), u64>,
    scope_sequences: HashMap<String, u64>,
    // Reopening clears the closed-state marker, but cannot erase an older read's
    // proof that execution scope changed while it was in flight.
    scope_change_sequences: HashMap<String, u64>,
    status_sequences: HashMap<String, u64>,
    access_sequences: HashMap<String, u64>,
    archive_sequence: u64,
    resumes_in_flight: HashMap<String, usize>,
    live_items: HashMap<(String, String, String), LiveItem>,
    provisional_items: HashMap<(String, String, String), ProvisionalItem>,
    legacy_history_read_sequences: HashMap<String, u64>,
    legacy_history_dirty_sequences: HashMap<String, u64>,
    plan_updates: HashMap<(String, String), LivePlanUpdate>,
    pending_plan_updates: HashMap<String, LivePlanUpdate>,
    peers: Vec<SyncSender<Value>>,
}

pub(super) fn id_key(id: &Value) -> String {
    id.to_string()
}

pub(super) fn can_respond(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "item/tool/requestUserInput"
    )
}

fn legacy_alias_type(item_type: Option<&str>) -> bool {
    matches!(
        item_type,
        Some("userMessage" | "agentMessage" | "reasoning" | "contextCompaction")
    )
}

fn copy_turn_metadata(target: &mut Value, source: &Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            if key != "items" && key != "itemsView" {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

impl State {
    pub fn connecting(allowed_roots: Vec<PathBuf>, request_namespace: String) -> Self {
        Self {
            request_namespace,
            connection: Connection::Connecting,
            allowed_roots,
            ..Self::default()
        }
    }
    /// Read active work from native notifications and canonical snapshots, never a browser flag.
    pub fn has_active_work(&self) -> bool {
        !self.resumes_in_flight.is_empty()
            || !self.pending.is_empty()
            || self.threads.values().any(|thread| {
                thread["status"]["type"] == "active"
                    || thread["status"] == "active"
                    || thread["turns"].as_array().is_some_and(|turns| {
                        turns.iter().any(|turn| turn["status"] == "inProgress")
                    })
            })
    }

    pub fn deactivate(&mut self) {
        self.emit("gateway/disabled", json!({}), None);
        self.peers.clear();
        self.threads.clear();
        self.subscribed.clear();
        self.pending.clear();
    }

    pub fn status(&self) -> Value {
        let mut active_turns = Vec::new();
        for (id, thread) in &self.threads {
            let name: String = thread["name"]
                .as_str()
                .unwrap_or(id)
                .chars()
                .take(160)
                .collect();
            let mut found = false;
            if let Some(turns) = thread["turns"].as_array() {
                for turn in turns.iter().filter(|turn| turn["status"] == "inProgress") {
                    active_turns.push(json!({"threadId":id,"threadName":name,"turnId":turn["id"]}));
                    found = true;
                }
            }
            if !found && (thread["status"]["type"] == "active" || thread["status"] == "active") {
                active_turns.push(json!({"threadId":id,"threadName":name,"turnId":null}));
            }
        }
        active_turns.sort_by_key(|turn| {
            (
                turn["threadId"].as_str().unwrap_or("").to_owned(),
                turn["turnId"].as_str().unwrap_or("").to_owned(),
            )
        });
        json!({"connection":self.connection.as_str(),"connectionError":self.connection_error,"epoch":self.epoch,"sequence":self.sequence,
            "activeWork":self.has_active_work(),"activeTurns":active_turns})
    }

    pub fn subscribe(&mut self) -> (Value, Receiver<Value>) {
        let (sender, receiver) = mpsc::sync_channel(EVENT_CAPACITY);
        self.peers.push(sender);
        (self.status(), receiver)
    }

    pub fn emit(&mut self, method: &str, params: Value, request_id: Option<Value>) {
        self.sequence += 1;
        let request = request_id
            .as_ref()
            .and_then(|id| self.pending.get(&id_key(id)));
        let event = json!({"epoch":self.epoch,"sequence":self.sequence,"method":method,
            "params":params,"requestId":request_id,"request":request});
        // A full peer is disconnected. The browser reconnects and fetches an authoritative
        // snapshot; retaining an unbounded backlog could exhaust the monitor process.
        self.peers
            .retain(|peer| peer.try_send(event.clone()).is_ok());
    }

    pub fn connection(&mut self, connection: Connection) {
        let previous = self.connection;
        self.connection = connection;
        if connection != Connection::Connected || previous == Connection::Connected {
            self.resumes_in_flight.clear();
            // A replacement native connection has no replay guarantee for partial
            // items. Never project an earlier connection's transient text as current.
            for (thread_id, thread) in &mut self.threads {
                if let Some(turns) = thread["turns"].as_array_mut() {
                    for turn in turns {
                        let Some(turn_id) = turn["id"].as_str().map(str::to_owned) else {
                            continue;
                        };
                        if let Some(items) = turn["items"].as_array_mut() {
                            items.retain(|item| {
                                !item["id"].as_str().is_some_and(|item_id| {
                                    self.live_items.contains_key(&(
                                        thread_id.clone(),
                                        turn_id.clone(),
                                        item_id.to_owned(),
                                    ))
                                })
                            });
                        }
                    }
                }
            }
            self.live_items.clear();
            self.provisional_items.clear();
            self.legacy_history_read_sequences.clear();
            self.legacy_history_dirty_sequences.clear();
            // Updated Plan notifications are not replayable ThreadItems. A new
            // native connection cannot confirm an earlier connection's live plan.
            self.plan_updates.clear();
            self.pending_plan_updates.clear();
            self.turn_sequences.clear();
        }
        if connection == Connection::Connected {
            self.connection_error = None;
            // Bootstrap notifications and recovered snapshots already belong to
            // this socket. Publishing Connected must retain that new generation.
            // A direct Connected-to-Connected transition still replaces the old
            // socket and therefore clears its transient state above.
            self.epoch += 1;
        }
        self.emit("gateway/connection", self.status(), None);
    }

    /// Keep the exact transport/setup cause available even without a pending RPC caller.
    pub fn reconnect(&mut self, cause: String) {
        self.connection_error = Some(cause);
        self.connection(Connection::Reconnecting);
    }

    /// Native setup receives notifications before publishing its new epoch to
    /// browsers. Tag and merge those notifications as that upcoming generation.
    fn native_event_epoch(&self) -> u64 {
        match self.connection {
            Connection::Connected => self.epoch,
            Connection::Connecting | Connection::Reconnecting => self.epoch + 1,
        }
    }

    pub fn snapshot(&self, thread_id: &str, queue: Value) -> Value {
        let archived = self.archived_threads.contains(thread_id);
        let pending: Vec<&Value> = self
            .pending
            .values()
            .filter(|request| {
                !archived && request["params"]["threadId"].as_str() == Some(thread_id)
            })
            .collect();
        let settings = if archived {
            json!({})
        } else {
            json!(self.settings.get(thread_id))
        };
        let plan_updates: Vec<&Value> = self
            .threads
            .get(thread_id)
            .and_then(|thread| thread["turns"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|turn| {
                let turn_id = turn["id"].as_str()?;
                self.plan_updates
                    .get(&(thread_id.to_owned(), turn_id.to_owned()))
                    .filter(|update| update.epoch == self.native_event_epoch())
                    .map(|update| &update.params)
            })
            .collect();
        let mut provisional_items: Vec<_> = self
            .provisional_items
            .iter()
            .filter(|((id, turn_id, _), item)| {
                !archived
                    && id == thread_id
                    && item.epoch == self.native_event_epoch()
                    && self.threads.get(thread_id).is_some_and(|thread| {
                        thread["historyMode"] == "legacy"
                            && thread["turns"].as_array().is_some_and(|turns| {
                                turns.iter().any(|turn| {
                                    turn["id"] == *turn_id && turn["status"] == "inProgress"
                                })
                            })
                    })
            })
            .collect();
        provisional_items.sort_by_key(|(_, item)| item.sequence);
        let provisional_items: Vec<_> = provisional_items
            .into_iter()
            .map(|((_, turn_id, _), item)| json!({"turnId":turn_id,"item":item.item}))
            .collect();
        let legacy_history_dirty = !archived
            && self
                .threads
                .get(thread_id)
                .is_some_and(|thread| thread["historyMode"] == "legacy")
            && self
                .legacy_history_dirty_sequences
                .get(thread_id)
                .is_some_and(|dirty| {
                    *dirty
                        > self
                            .legacy_history_read_sequences
                            .get(thread_id)
                            .copied()
                            .unwrap_or(0)
                });
        json!({"thread":self.threads.get(thread_id),"settings":settings,
            "accessMode":if archived { "archived" } else { "active" },
            "planUpdates":plan_updates,"provisionalItems":provisional_items,
            "legacyHistoryDirty":legacy_history_dirty,
            "pending":pending,"queue":if archived { json!([]) } else { queue },"connection":self.connection.as_str(),
            "epoch":self.epoch,"sequence":self.sequence})
    }

    /// Store native-confirmed archived history only while its connection and scoped
    /// event watermark remain current. This never loads an execution session.
    pub fn remember_archived_scoped(
        &mut self,
        thread: Value,
        epoch: u64,
        after: u64,
        scope: &DirectoryScope,
    ) -> bool {
        let Some(id) = thread["id"].as_str().map(str::to_owned) else {
            return false;
        };
        let newer = [
            &self.scope_change_sequences,
            &self.status_sequences,
            &self.settings_sequence,
            &self.queue_sequences,
            &self.access_sequences,
        ]
        .iter()
        .any(|sequences| sequences.get(&id).is_some_and(|sequence| *sequence > after));
        if self.connection != Connection::Connected
            || self.epoch != epoch
            || self.archive_sequence > after
            || newer
            || self
                .turn_sequences
                .iter()
                .any(|((thread_id, _), sequence)| thread_id == &id && *sequence > after)
            || !thread["cwd"].as_str().is_some_and(|cwd| scope.permits(cwd))
        {
            return false;
        }
        self.threads.insert(id.clone(), thread);
        self.settings.remove(&id);
        self.archived_threads.insert(id.clone());
        self.pending
            .retain(|_, request| request["params"]["threadId"].as_str() != Some(&id));
        self.live_items
            .retain(|(thread_id, _, _), _| thread_id != &id);
        self.clear_provisional_streams(&id);
        self.retain_plans_for_history(&id, Some(after));
        self.subscribed.insert(id);
        true
    }

    /// Capture native request replay while keeping archived controls hidden until
    /// the corresponding resume reply has actually reopened the execution session.
    pub fn begin_resume(&mut self, thread_id: &str) {
        if self.archived_threads.contains(thread_id)
            && !self.resumes_in_flight.contains_key(thread_id)
        {
            self.pending
                .retain(|_, request| request["params"]["threadId"].as_str() != Some(thread_id));
        }
        *self
            .resumes_in_flight
            .entry(thread_id.to_owned())
            .or_default() += 1;
    }

    /// Close ephemeral Core streams and refresh fences when an attachment or
    /// recovery is rejected. Materialized native history remains separate.
    pub fn clear_provisional_streams(&mut self, thread_id: &str) {
        self.provisional_items
            .retain(|(id, _, _), _| id != thread_id);
        self.legacy_history_read_sequences.remove(thread_id);
        self.legacy_history_dirty_sequences.remove(thread_id);
    }

    pub fn finish_resume(&mut self, thread_id: &str) {
        if let Some(count) = self.resumes_in_flight.get_mut(thread_id) {
            *count -= 1;
            if *count == 0 {
                self.resumes_in_flight.remove(thread_id);
            }
        }
        if self.archived_threads.contains(thread_id)
            && !self.resumes_in_flight.contains_key(thread_id)
        {
            self.pending
                .retain(|_, request| request["params"]["threadId"].as_str() != Some(thread_id));
        }
    }

    /// Reject history read across a scope closure or archive membership change,
    /// including an archive/restore cycle that returns to its original state.
    pub fn snapshot_invalidated_after(&self, thread_id: &str, after: u64) -> bool {
        [&self.scope_change_sequences, &self.access_sequences]
            .iter()
            .any(|sequences| sequences.get(thread_id).is_some_and(|value| *value > after))
    }

    /// Merge a native snapshot without discarding events that arrived after that
    /// snapshot request began. Responses and notifications share a socket, but a
    /// concurrent server task can enqueue a newer event before its older response.
    pub fn remember_scoped(&mut self, result: &Value, after: Option<u64>, scope: &DirectoryScope) {
        let Some(id) = result["thread"]["id"].as_str() else {
            return;
        };
        if after.is_some_and(|after| self.snapshot_invalidated_after(id, after)) {
            // An older in-flight snapshot cannot reopen a projection closed by a
            // later out-of-scope settings event.
            return;
        }
        if !self.allowed_roots.is_empty() {
            let cwd = result["cwd"]
                .as_str()
                .or_else(|| result["thread"]["cwd"].as_str());
            if !cwd.is_some_and(|cwd| scope.permits(cwd)) {
                self.forget_outside_scope(id, scope);
                return;
            }
        }
        let mut thread = result["thread"].clone();
        let legacy = thread["historyMode"] == "legacy";
        if after.is_some_and(|after| {
            self.status_sequences
                .get(id)
                .is_some_and(|sequence| *sequence > after)
        }) {
            if let Some(current) = self.threads.get(id) {
                thread["status"] = current["status"].clone();
            }
        }
        if let Some(after) = after {
            if let Some(turns) = thread["turns"].as_array_mut() {
                if let Some(existing) = self
                    .threads
                    .get(id)
                    .and_then(|thread| thread["turns"].as_array())
                {
                    for turn in existing {
                        if turn["id"].as_str().is_some_and(|turn_id| {
                            self.turn_sequences
                                .get(&(id.to_owned(), turn_id.to_owned()))
                                .is_some_and(|sequence| *sequence > after)
                        }) {
                            if let Some(snapshot_turn) =
                                turns.iter_mut().find(|item| item["id"] == turn["id"])
                            {
                                // A terminal persisted turn is authoritative over
                                // an older partial cache, even if a late delta raced
                                // this read. Completed native events still win over
                                // an earlier active snapshot.
                                if turn["status"] != "inProgress" {
                                    if legacy {
                                        // A newer terminal event confirms metadata,
                                        // not the aliases of an older cached view.
                                        copy_turn_metadata(snapshot_turn, turn);
                                    } else {
                                        *snapshot_turn = turn.clone();
                                    }
                                }
                            } else if legacy {
                                let mut metadata = turn.clone();
                                metadata["items"] = json!([]);
                                turns.push(metadata);
                            } else {
                                turns.push(turn.clone());
                            }
                        }
                    }
                }
            }
            // The installed runtime returns an active turn's persisted user item
            // while omitting its streaming agent item. Preserve native items seen on
            // this connection for the same active turn, including deltas received
            // before this resume request began. Item IDs prevent history duplication.
            if let Some(turns) = thread["turns"].as_array_mut() {
                if let Some(existing) = self
                    .threads
                    .get(id)
                    .and_then(|thread| thread["turns"].as_array())
                {
                    for snapshot_turn in turns
                        .iter_mut()
                        .filter(|turn| turn["status"] == "inProgress")
                    {
                        let Some(turn_id) = snapshot_turn["id"].as_str().map(str::to_owned) else {
                            continue;
                        };
                        let Some(cached_turn) = existing
                            .iter()
                            .find(|turn| turn["id"] == turn_id && turn["status"] == "inProgress")
                        else {
                            continue;
                        };
                        let Some(cached_items) = cached_turn["items"].as_array() else {
                            continue;
                        };
                        let Some(items) = snapshot_turn["items"].as_array_mut() else {
                            continue;
                        };
                        for cached_item in cached_items {
                            if legacy && legacy_alias_type(cached_item["type"].as_str()) {
                                // Legacy message IDs cannot be compared to Core
                                // event IDs. Their unfinished streams live separately.
                                continue;
                            }
                            let Some(item_id) = cached_item["id"].as_str() else {
                                continue;
                            };
                            let Some(live) = self
                                .live_items
                                .get(&(id.to_owned(), turn_id.clone(), item_id.to_owned()))
                                .filter(|item| item.epoch == self.native_event_epoch())
                            else {
                                continue;
                            };
                            if let Some(persisted_item) =
                                items.iter_mut().find(|item| item["id"] == item_id)
                            {
                                if !live.completed {
                                    // Until item/completed, native event text is the
                                    // transient source. Mixing persisted text with
                                    // subsequent deltas can duplicate output.
                                    *persisted_item = cached_item.clone();
                                }
                            } else {
                                items.push(cached_item.clone());
                            }
                        }
                    }
                }
            }
        }
        let mut settings = result.clone();
        if let Some(object) = settings.as_object_mut() {
            object.remove("thread");
            if let Some(effort) = object.remove("reasoningEffort") {
                object.insert("effort".to_owned(), effort);
            }
        }
        let has_newer_settings = after.is_some_and(|after| {
            self.settings_sequence
                .get(id)
                .is_some_and(|sequence| *sequence > after)
        });
        if has_newer_settings {
            if let Some(current) = self.settings.get(id) {
                if current["cwd"].is_string() {
                    thread["cwd"] = current["cwd"].clone();
                }
            }
        } else {
            self.settings.insert(id.to_owned(), settings);
        }
        // A native snapshot can replace an event item with persisted history.
        // Retire that item's transient provenance only when the snapshot won:
        // completed events yield to persisted items, while an active partial
        // event still owns its text until native item/completed arrives.
        self.live_items
            .retain(|(thread_id, turn_id, item_id), live| {
                if thread_id != id {
                    return true;
                }
                !result["thread"]["turns"]
                    .as_array()
                    .and_then(|turns| turns.iter().find(|turn| turn["id"] == *turn_id))
                    .is_some_and(|turn| {
                        (after.is_none() || turn["status"] != "inProgress" || live.completed)
                            && turn["items"].as_array().is_some_and(|items| {
                                items.iter().any(|item| item["id"] == *item_id)
                            })
                    })
            });
        let event_epoch = self.native_event_epoch();
        self.provisional_items
            .retain(|(thread_id, turn_id, _), item| {
                thread_id != id
                    || (legacy
                        && item.epoch == event_epoch
                        && thread["turns"].as_array().is_some_and(|turns| {
                            turns.iter().any(|turn| {
                                turn["id"] == *turn_id && turn["status"] == "inProgress"
                            })
                        }))
            });
        if legacy {
            // A terminal notification can race an earlier full read. Its newer
            // metadata must not let the browser stop polling before final history
            // is read again. The original RPC watermark is conservative: one more
            // read after the completion clears it without guessing alias identity.
            self.legacy_history_read_sequences
                .insert(id.to_owned(), after.unwrap_or(self.sequence));
        } else {
            self.legacy_history_read_sequences.remove(id);
            self.legacy_history_dirty_sequences.remove(id);
        }
        self.threads.insert(id.to_owned(), thread);
        self.retain_plans_for_history(id, after);
        self.scope_sequences.remove(id);
        self.archived_threads.remove(id);
        self.subscribed.insert(id.to_owned());
    }

    pub fn reduce_scoped(&mut self, message: &Value, scope: &DirectoryScope) {
        let Some(method) = message["method"].as_str() else {
            return;
        };
        let params = &message["params"];
        let thread_id = params["threadId"]
            .as_str()
            .or_else(|| params["thread"]["id"].as_str());
        if thread_id.is_none() {
            // This stage consumes no global native payloads. Keep the allowlist empty;
            // add a specific sanitized projection only when a browser consumer needs it.
            // Request-resolution is the exception: it must name a request already owned here.
            if method != "serverRequest/resolved"
                || message.get("id").is_some()
                || !self.pending.contains_key(&id_key(&params["requestId"]))
            {
                return;
            }
        }
        if matches!(method, "thread/archived" | "thread/unarchived") {
            // A cold archived read has no loaded cache yet. Even an unscoped native
            // archive notification must invalidate its in-flight inventory proof.
            self.archive_sequence = self.sequence + 1;
            if let Some(id) = thread_id
                .filter(|id| self.threads.contains_key(*id) || self.subscribed.contains(*id))
            {
                self.access_sequences
                    .insert(id.to_owned(), self.sequence + 1);
                if method == "thread/archived" {
                    self.archived_threads.insert(id.to_owned());
                    self.clear_provisional_streams(id);
                    self.pending
                        .retain(|_, request| request["params"]["threadId"].as_str() != Some(id));
                }
                // An unarchive notification changes inventory. A fresh successful
                // native resume is still required before enabling browser writers.
            }
        }
        // The server also broadcasts metadata about threads opened by other native
        // clients. Browser devices see only threads deliberately attached by Gateway.
        // The worker reserves a subscription before sending resume, so replayed
        // requests preceding its response are retained.
        if thread_id.is_some_and(|id| !self.subscribed.contains(id)) {
            if method == "thread/started"
                || method == "thread/archived"
                || method == "thread/unarchived"
            {
                // Refresh the filtered registry without exposing another thread's
                // identifiers, paths or transcript on the global event stream.
                self.emit("gateway/historyChanged", json!({}), None);
            }
            return;
        }
        if method == "turn/plan/updated" {
            let Some((thread_id, turn_id)) = self.scoped_plan_ids(params, scope) else {
                return;
            };
            // Keep the actual notification separately from proposed-plan items;
            // native history does not promise to replay this update on resume.
            let update = LivePlanUpdate {
                epoch: self.native_event_epoch(),
                sequence: self.sequence + 1,
                params: params.clone(),
            };
            if self.threads[thread_id]["turns"]
                .as_array()
                .is_some_and(|turns| turns.iter().any(|turn| turn["id"] == turn_id))
            {
                if self
                    .pending_plan_updates
                    .get(thread_id)
                    .is_some_and(|pending| pending.params["turnId"] == turn_id)
                {
                    self.pending_plan_updates.remove(thread_id);
                }
                self.plan_updates
                    .insert((thread_id.to_owned(), turn_id.to_owned()), update);
            } else {
                // Native notification order has no verified turn/started
                // guarantee. Retain one latest unconfirmed turn per attached
                // thread without fabricating that turn in browser history.
                self.pending_plan_updates
                    .insert(thread_id.to_owned(), update);
            }
        }
        if method == "thread/settings/updated" && !self.allowed_roots.is_empty() {
            if let Some(id) = thread_id {
                if !params["threadSettings"]["cwd"]
                    .as_str()
                    .is_some_and(|cwd| scope.permits(cwd))
                {
                    self.forget_outside_scope(id, scope);
                    return;
                }
            }
        }
        if method == "serverRequest/resolved"
            && !self.pending.contains_key(&id_key(&params["requestId"]))
        {
            return;
        }
        if let Some(id) = message.get("id") {
            // Owner-scoped native ceremonies without an execution thread remain
            // with their selected executor; they are not browser answer controls.
            if thread_id.is_none() {
                return;
            }
            if thread_id.is_some_and(|id| {
                self.archived_threads.contains(id) && !self.resumes_in_flight.contains_key(id)
            }) {
                return;
            }
            // Native resume replays an unresolved request unchanged. Preserve its
            // token so a background history refresh cannot erase a question draft.
            // Bootstrap receives requests before publishing Connected; those
            // requests already belong to the upcoming native connection epoch.
            let request_epoch = self.native_event_epoch();
            let generation = format!("{}:{request_epoch}:", self.request_namespace);
            let key = id_key(id);
            let request_token = self
                .pending
                .get(&key)
                .filter(|request| {
                    request["id"] == *id
                        && request["method"] == method
                        && request["params"] == *params
                        && request["canRespond"] == can_respond(method)
                })
                .and_then(|request| request["requestToken"].as_str())
                .filter(|token| token.starts_with(&generation))
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{generation}{}", self.sequence + 1));
            self.pending.insert(
                key,
                json!({"id":id,"method":method,"params":params,
                "canRespond":can_respond(method),"requestToken":request_token}),
            );
        }
        if method == "thread/settings/updated" {
            if let Some(id) = params["threadId"].as_str() {
                self.settings
                    .insert(id.to_owned(), params["threadSettings"].clone());
                self.settings_sequence
                    .insert(id.to_owned(), self.sequence + 1);
                if let Some(thread) = self.threads.get_mut(id) {
                    thread["cwd"] = params["threadSettings"]["cwd"].clone();
                }
            }
        }
        if method == "thread/queue/changed" {
            if let Some(id) = thread_id {
                self.queue_sequences
                    .insert(id.to_owned(), self.sequence + 1);
            }
        }
        if method == "thread/status/changed" {
            if let Some(id) = thread_id {
                self.status_sequences
                    .insert(id.to_owned(), self.sequence + 1);
            }
        }
        if method == "serverRequest/resolved" {
            self.pending.remove(&id_key(&params["requestId"]));
        }
        // Dynamic tool calls are owned by their executor (usually Codex Desktop). They
        // complete through item/completed, without serverRequest/resolved.
        if method == "item/completed" && params["item"]["type"] == "dynamicToolCall" {
            self.pending.retain(|_, request| {
                !(request["method"] == "item/tool/call"
                    && request["params"]["threadId"] == params["threadId"]
                    && request["params"]["callId"] == params["item"]["id"])
            });
        }
        if method == "thread/started" {
            if let Some(id) = params["thread"]["id"].as_str() {
                self.threads.insert(id.to_owned(), params["thread"].clone());
            }
        }
        if thread_id.is_some_and(|id| self.reduce_legacy_projection(method, params, id)) {
            return;
        }
        if let Some(thread) = thread_id.and_then(|id| self.threads.get_mut(id)) {
            if method == "item/agentMessage/delta" {
                if let Some(items) = thread["turns"]
                    .as_array_mut()
                    .and_then(|turns| {
                        turns.iter_mut().find(|turn| {
                            turn["id"] == params["turnId"] && turn["status"] == "inProgress"
                        })
                    })
                    .and_then(|turn| turn["items"].as_array_mut())
                {
                    if let Some(item_id) = params["itemId"].as_str().filter(|id| !id.is_empty()) {
                        if !items.iter().any(|item| item["id"] == item_id) {
                            // A late native subscriber receives no item/started
                            // replay. Show subsequent text as partial until the
                            // canonical item/completed supplies its missing prefix.
                            items.push(json!({"id":item_id,"type":"agentMessage","text":"","partialFromJoin":true}));
                        }
                    }
                }
            }
            match method {
                "thread/status/changed" => thread["status"] = params["status"].clone(),
                "turn/started" | "turn/completed" => {
                    if let Some(turns) = thread["turns"].as_array_mut() {
                        if let Some(turn) = turns
                            .iter_mut()
                            .find(|turn| turn["id"] == params["turn"]["id"])
                        {
                            let items = turn["items"].clone();
                            *turn = params["turn"].clone();
                            if !turn["items"]
                                .as_array()
                                .is_some_and(|items| !items.is_empty())
                            {
                                turn["items"] = items;
                            }
                        } else {
                            turns.push(params["turn"].clone());
                        }
                    }
                }
                "item/started" | "item/completed" => {
                    if let Some(items) = thread["turns"]
                        .as_array_mut()
                        .and_then(|turns| {
                            turns.iter_mut().find(|turn| turn["id"] == params["turnId"])
                        })
                        .and_then(|turn| turn["items"].as_array_mut())
                    {
                        if let Some(item) = items
                            .iter_mut()
                            .find(|item| item["id"] == params["item"]["id"])
                        {
                            *item = params["item"].clone();
                        } else {
                            items.push(params["item"].clone());
                        }
                    }
                }
                "item/agentMessage/delta"
                | "item/plan/delta"
                | "item/commandExecution/outputDelta"
                | "item/fileChange/outputDelta" => {
                    if let Some(item) = thread["turns"]
                        .as_array_mut()
                        .and_then(|turns| {
                            turns.iter_mut().find(|turn| turn["id"] == params["turnId"])
                        })
                        .and_then(|turn| turn["items"].as_array_mut())
                        .and_then(|items| {
                            items.iter_mut().find(|item| item["id"] == params["itemId"])
                        })
                    {
                        let field = if matches!(
                            method,
                            "item/commandExecution/outputDelta" | "item/fileChange/outputDelta"
                        ) {
                            "aggregatedOutput"
                        } else {
                            "text"
                        };
                        let text = format!(
                            "{}{}",
                            item[field].as_str().unwrap_or_default(),
                            params["delta"].as_str().unwrap_or_default()
                        );
                        item[field] = json!(text);
                    }
                }
                "item/fileChange/patchUpdated" => {
                    if let Some(item) = thread["turns"]
                        .as_array_mut()
                        .and_then(|turns| {
                            turns.iter_mut().find(|turn| turn["id"] == params["turnId"])
                        })
                        .and_then(|turn| turn["items"].as_array_mut())
                        .and_then(|items| {
                            items.iter_mut().find(|item| item["id"] == params["itemId"])
                        })
                    {
                        item["changes"] = params["changes"].clone();
                    }
                }
                "item/reasoning/summaryTextDelta"
                | "item/reasoning/summaryPartAdded"
                | "item/reasoning/textDelta" => {
                    if let Some(item) = thread["turns"]
                        .as_array_mut()
                        .and_then(|turns| {
                            turns.iter_mut().find(|turn| turn["id"] == params["turnId"])
                        })
                        .and_then(|turn| turn["items"].as_array_mut())
                        .and_then(|items| {
                            items.iter_mut().find(|item| item["id"] == params["itemId"])
                        })
                    {
                        let (field, index) = if matches!(
                            method,
                            "item/reasoning/summaryTextDelta" | "item/reasoning/summaryPartAdded"
                        ) {
                            ("summary", params["summaryIndex"].as_u64().unwrap_or(0))
                        } else {
                            ("content", params["contentIndex"].as_u64().unwrap_or(0))
                        };
                        // Native reasoning chunks use small contiguous indices. A
                        // malformed frame must not allocate an unbounded vector.
                        if index <= 4096 {
                            if !item[field].is_array() {
                                item[field] = json!([]);
                            }
                            let parts = item[field].as_array_mut().unwrap();
                            parts.resize_with(parts.len().max(index as usize + 1), || json!(""));
                            let part = &mut parts[index as usize];
                            *part = json!(format!(
                                "{}{}",
                                part.as_str().unwrap_or_default(),
                                params["delta"].as_str().unwrap_or_default()
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        let turn_id = params["turnId"]
            .as_str()
            .or_else(|| params["turn"]["id"].as_str());
        if let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) {
            if matches!(
                method,
                "turn/started"
                    | "turn/completed"
                    | "turn/plan/updated"
                    | "item/started"
                    | "item/completed"
                    | "item/agentMessage/delta"
                    | "item/plan/delta"
                    | "item/commandExecution/outputDelta"
                    | "item/fileChange/outputDelta"
                    | "item/fileChange/patchUpdated"
                    | "item/reasoning/summaryTextDelta"
                    | "item/reasoning/summaryPartAdded"
                    | "item/reasoning/textDelta"
            ) && (method != "turn/plan/updated"
                || self
                    .plan_updates
                    .contains_key(&(thread_id.to_owned(), turn_id.to_owned())))
            {
                self.turn_sequences.insert(
                    (thread_id.to_owned(), turn_id.to_owned()),
                    self.sequence + 1,
                );
                let item_id = params["itemId"]
                    .as_str()
                    .or_else(|| params["item"]["id"].as_str());
                if let Some(item_id) = item_id {
                    let event_epoch = self.native_event_epoch();
                    let live = self
                        .live_items
                        .entry((thread_id.to_owned(), turn_id.to_owned(), item_id.to_owned()))
                        .or_insert(LiveItem {
                            epoch: event_epoch,
                            completed: false,
                        });
                    live.epoch = event_epoch;
                    if method == "item/completed" {
                        live.completed = true;
                    } else if method == "item/started" {
                        live.completed = false;
                    }
                }
            }
            if method == "turn/completed" {
                self.live_items
                    .retain(|(id, turn, _), _| id != thread_id || turn != turn_id);
                self.provisional_items
                    .retain(|(id, turn, _), _| id != thread_id || turn != turn_id);
                if !self.threads.contains_key(thread_id)
                    && !self.archived_threads.contains(thread_id)
                {
                    self.legacy_history_dirty_sequences
                        .insert(thread_id.to_owned(), self.sequence + 1);
                }
            }
            if matches!(method, "turn/started" | "turn/completed") {
                self.retain_plans_for_history(thread_id, Some(self.sequence + 1));
            }
        }
        let visible = if thread_id.is_none() {
            json!({"requestId":params["requestId"]})
        } else {
            params.clone()
        };
        self.emit(method, visible, message.get("id").cloned());
    }

    /// Legacy history aliases and Core event IDs have no identity mapping.
    /// Keep unfinished message streams separate and let full reads own history.
    fn reduce_legacy_projection(&mut self, method: &str, params: &Value, thread_id: &str) -> bool {
        let legacy = self
            .threads
            .get(thread_id)
            .is_some_and(|thread| thread["historyMode"] == "legacy");
        let unknown = !self.threads.contains_key(thread_id);
        if !legacy && !unknown {
            return false;
        }
        let turn_id = params["turnId"]
            .as_str()
            .or_else(|| params["turn"]["id"].as_str())
            .filter(|id| !id.is_empty());
        let Some(turn_id) = turn_id else {
            return false;
        };
        if method == "turn/completed" && legacy {
            if !self.archived_threads.contains(thread_id) {
                self.legacy_history_dirty_sequences
                    .insert(thread_id.to_owned(), self.sequence + 1);
            }
            if let Some(turns) = self
                .threads
                .get_mut(thread_id)
                .and_then(|thread| thread["turns"].as_array_mut())
            {
                if let Some(turn) = turns.iter_mut().find(|turn| turn["id"] == turn_id) {
                    copy_turn_metadata(turn, &params["turn"]);
                } else {
                    let mut turn = params["turn"].clone();
                    turn["items"] = json!([]);
                    turns.push(turn);
                }
            }
            self.provisional_items
                .retain(|(id, turn, _), _| id != thread_id || turn != turn_id);
            self.live_items
                .retain(|(id, turn, _), _| id != thread_id || turn != turn_id);
            self.turn_sequences.insert(
                (thread_id.to_owned(), turn_id.to_owned()),
                self.sequence + 1,
            );
            self.retain_plans_for_history(thread_id, Some(self.sequence + 1));
            let mut forwarded = params.clone();
            forwarded["turn"]["items"] = json!([]);
            self.emit(method, forwarded, None);
            return true;
        }
        let delta_type = match method {
            "item/agentMessage/delta" => Some("agentMessage"),
            "item/reasoning/summaryTextDelta"
            | "item/reasoning/summaryPartAdded"
            | "item/reasoning/textDelta" => Some("reasoning"),
            _ => None,
        };
        let lifecycle = matches!(method, "item/started" | "item/completed")
            && legacy_alias_type(params["item"]["type"].as_str());
        if !lifecycle && delta_type.is_none() {
            return false;
        }
        let item_id = params["itemId"]
            .as_str()
            .or_else(|| params["item"]["id"].as_str())
            .filter(|id| !id.is_empty());
        let Some(item_id) = item_id else {
            return false;
        };
        // Archive closes the live projection even when a previously started
        // Native turn continues producing output. Only completion refreshes its
        // read-only materialized history; starts and deltas do not reopen streams.
        if legacy && self.archived_threads.contains(thread_id) && method != "item/completed" {
            return true;
        }
        let key = (thread_id.to_owned(), turn_id.to_owned(), item_id.to_owned());
        self.turn_sequences.insert(
            (thread_id.to_owned(), turn_id.to_owned()),
            self.sequence + 1,
        );
        if method == "item/completed" {
            if !self.archived_threads.contains(thread_id) {
                self.legacy_history_dirty_sequences
                    .insert(thread_id.to_owned(), self.sequence + 1);
            }
            self.provisional_items.remove(&key);
        } else if !self.archived_threads.contains(thread_id) {
            let item_type = delta_type.or_else(|| params["item"]["type"].as_str());
            if matches!(item_type, Some("agentMessage" | "reasoning")) {
                let epoch = self.native_event_epoch();
                let sequence = self.sequence + 1;
                let item = self.provisional_items.entry(key).or_insert_with(|| ProvisionalItem {
                    epoch,
                    sequence,
                    item: if method == "item/started" {
                        params["item"].clone()
                    } else if item_type == Some("agentMessage") {
                        json!({"id":item_id,"type":"agentMessage","text":"","phase":null,"partialFromJoin":true})
                    } else {
                        json!({"id":item_id,"type":"reasoning","summary":[],"content":[],"partialFromJoin":true})
                    },
                });
                item.epoch = epoch;
                if method == "item/started" {
                    item.item = params["item"].clone();
                } else if method == "item/agentMessage/delta" {
                    if let Some(delta) = params["delta"].as_str() {
                        item.item["text"] = json!(format!(
                            "{}{}",
                            item.item["text"].as_str().unwrap_or_default(),
                            delta
                        ));
                    }
                } else {
                    let (field, index) = if method == "item/reasoning/textDelta" {
                        ("content", params["contentIndex"].as_u64().unwrap_or(0))
                    } else {
                        ("summary", params["summaryIndex"].as_u64().unwrap_or(0))
                    };
                    if index <= 4096 {
                        if !item.item[field].is_array() {
                            item.item[field] = json!([]);
                        }
                        let parts = item.item[field].as_array_mut().unwrap();
                        parts.resize_with(parts.len().max(index as usize + 1), || json!(""));
                        if let Some(delta) = params["delta"].as_str() {
                            let part = &mut parts[index as usize];
                            *part =
                                json!(format!("{}{}", part.as_str().unwrap_or_default(), delta));
                        }
                    }
                }
            }
        }
        if legacy
            && (method == "item/completed"
                || lifecycle
                    && !matches!(
                        params["item"]["type"].as_str(),
                        Some("agentMessage" | "reasoning")
                    ))
        {
            self.emit("gateway/legacy-history-changed", json!({"threadId":thread_id,"turnId":turn_id,"itemId":item_id,"reason":if method == "item/completed" { "itemCompleted" } else { "itemStarted" }}), None);
        } else {
            self.emit(method, params.clone(), None);
        }
        true
    }

    fn forget_outside_scope(&mut self, thread_id: &str, scope: &DirectoryScope) {
        self.scope_change_sequences
            .insert(thread_id.to_owned(), self.sequence + 1);
        self.scope_sequences
            .insert(thread_id.to_owned(), self.sequence + 1);
        self.subscribed.remove(thread_id);
        self.threads.remove(thread_id);
        self.settings.remove(thread_id);
        self.settings_sequence.remove(thread_id);
        self.queue_sequences.remove(thread_id);
        self.status_sequences.remove(thread_id);
        self.archived_threads.remove(thread_id);
        self.access_sequences.remove(thread_id);
        self.resumes_in_flight.remove(thread_id);
        self.pending
            .retain(|_, request| request["params"]["threadId"].as_str() != Some(thread_id));
        self.turn_sequences.retain(|(id, _), _| id != thread_id);
        self.live_items.retain(|(id, _, _), _| id != thread_id);
        self.clear_provisional_streams(thread_id);
        self.plan_updates.retain(|(id, _), _| id != thread_id);
        self.pending_plan_updates.remove(thread_id);
        self.emit(
            "gateway/recoveryError",
            json!({"threadId":thread_id,"message":scope.reason()}),
            None,
        );
    }

    pub fn scope_closed(&self, thread_id: &str) -> bool {
        self.scope_sequences.contains_key(thread_id)
    }

    /// Validate the installed native notification schema and its already attached
    /// thread. An early update may refer to a turn not yet in cached native history.
    fn scoped_plan_ids<'a>(
        &self,
        params: &'a Value,
        scope: &DirectoryScope,
    ) -> Option<(&'a str, &'a str)> {
        let thread_id = params["threadId"].as_str().filter(|id| !id.is_empty())?;
        let turn_id = params["turnId"].as_str().filter(|id| !id.is_empty())?;
        if !self.subscribed.contains(thread_id) || self.scope_closed(thread_id) {
            return None;
        }
        let thread = self.threads.get(thread_id)?;
        if !self.allowed_roots.is_empty()
            && !thread["cwd"].as_str().is_some_and(|cwd| scope.permits(cwd))
        {
            return None;
        }
        if params
            .get("explanation")
            .is_some_and(|value| !value.is_null() && !value.is_string())
            || !params["plan"].as_array()?.iter().all(|step| {
                step["step"].is_string()
                    && matches!(
                        step["status"].as_str(),
                        Some("pending" | "inProgress" | "completed")
                    )
            })
        {
            return None;
        }
        Some((thread_id, turn_id))
    }

    fn retain_plans_for_history(&mut self, thread_id: &str, after: Option<u64>) {
        let event_epoch = self.native_event_epoch();
        let known_turns: HashSet<&str> = self
            .threads
            .get(thread_id)
            .and_then(|thread| thread["turns"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|turn| turn["id"].as_str())
            .collect();
        self.plan_updates.retain(|(id, turn_id), update| {
            id != thread_id
                || (update.epoch == event_epoch
                    && (known_turns.contains(turn_id.as_str())
                        || after.is_some_and(|after| update.sequence > after)))
        });
        if let Some(update) = self.pending_plan_updates.get(thread_id) {
            let turn_id = update.params["turnId"].as_str().unwrap();
            if update.epoch != event_epoch {
                self.pending_plan_updates.remove(thread_id);
            } else if known_turns.contains(turn_id) {
                let turn_id = turn_id.to_owned();
                let update = self.pending_plan_updates.remove(thread_id).unwrap();
                let key = (thread_id.to_owned(), turn_id);
                self.turn_sequences
                    .entry(key.clone())
                    .and_modify(|sequence| *sequence = (*sequence).max(update.sequence))
                    .or_insert(update.sequence);
                self.plan_updates.insert(key, update);
            }
            // A history read can precede persistence of a native live turn even
            // after its plan was emitted. Until a native turn confirms the ID,
            // keep this single pending update across reads instead of assuming
            // an absent turn proves the plan obsolete. Replacement, scope closure,
            // or a newer unknown-turn update bounds its lifetime and memory.
        }
    }
}

/// Test fixtures classify their input before invoking the deterministic transition.
#[cfg(test)]
pub(super) mod scope_fixture {
    use super::*;
    pub(crate) trait ScopeFixture {
        fn remember(&mut self, result: &Value);
        fn remember_after(&mut self, result: &Value, after: Option<u64>);
        fn reduce(&mut self, message: &Value);
    }
    impl ScopeFixture for State {
        fn remember(&mut self, result: &Value) {
            self.remember_after(result, None);
        }
        fn remember_after(&mut self, result: &Value, after: Option<u64>) {
            let scope = crate::directory_scope(
                &self.allowed_roots,
                result["cwd"]
                    .as_str()
                    .or_else(|| result["thread"]["cwd"].as_str()),
            );
            self.remember_scoped(result, after, &scope);
        }
        fn reduce(&mut self, message: &Value) {
            let cwd = crate::notification_directory(self, message);
            let scope = crate::directory_scope(&self.allowed_roots, cwd.as_deref());
            self.reduce_scoped(message, &scope);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scope_fixture::ScopeFixture;
    use super::*;

    #[test]
    fn status_names_canonical_active_turns_without_including_history_content() {
        let mut state = State::connecting(vec![], "active-work-test".into());
        state.threads.insert(
            "b".into(),
            json!({"id":"b","name":"Build fixture","status":{"type":"active"},"turns":[]}),
        );
        state.threads.insert(
            "a".into(),
            json!({"id":"a","name":"Review fixture","turns":[
                {"id":"old","status":"completed","items":[{"text":"private sentinel"}]},
                {"id":"current","status":"inProgress","items":[{"text":"private sentinel"}]}
            ]}),
        );
        state.threads.insert(
            "idle".into(),
            json!({"id":"idle","status":{"type":"idle"},"turns":[]}),
        );
        let status = state.status();
        assert_eq!(status["activeWork"], true);
        assert_eq!(
            status["activeTurns"],
            json!([
                {"threadId":"a","threadName":"Review fixture","turnId":"current"},
                {"threadId":"b","threadName":"Build fixture","turnId":null}
            ])
        );
        assert!(!status.to_string().contains("private sentinel"));
        state.threads.clear();
        assert_eq!(state.status()["activeWork"], false);
        assert_eq!(state.status()["activeTurns"], json!([]));
        state
            .pending
            .insert("approval".into(), json!({"params":{"threadId":"a"}}));
        assert_eq!(state.status()["activeWork"], true);
        assert_eq!(state.status()["activeTurns"], json!([]));
    }

    #[test]
    fn validated_scope_reduction_is_independent_of_later_filesystem_changes() {
        let root = std::env::temp_dir().join(format!(
            "native-pure-scope-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cwd = root.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let root = root.canonicalize().unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let result = json!({"thread":{"id":"pure","cwd":cwd,"turns":[]},"cwd":cwd});
        let scope = crate::directory_scope(std::slice::from_ref(&root), cwd.to_str());
        let mut before = State::connecting(vec![root.clone()], "fixed".to_owned());
        before.connection(Connection::Connected);
        before.remember_scoped(&result, None, &scope);
        std::fs::remove_dir(&cwd).unwrap();
        let mut after = State::connecting(vec![root.clone()], "fixed".to_owned());
        after.connection(Connection::Connected);
        after.remember_scoped(&result, None, &scope);
        assert_eq!(
            before.snapshot("pure", json!([])),
            after.snapshot("pure", json!([]))
        );
        let (_, events) = after.subscribe();
        let fresh = crate::directory_scope(std::slice::from_ref(&root), cwd.to_str());
        after.remember_scoped(&result, None, &fresh);
        assert!(after.scope_closed("pure"));
        assert!(events.recv().unwrap()["params"]["message"]
            .as_str()
            .unwrap()
            .contains("Resolve execution directory"));
        crate::remove_test_scratch(&root);
    }

    fn legacy_fixture() -> (State, Value) {
        let mut state = State::default();
        state.connection(Connection::Connected);
        let native = json!({"thread":{"id":"t","cwd":"/tmp","historyMode":"legacy","turns":[
            {"id":"past","status":"completed","itemsView":"full","items":[
                {"id":"item-1","type":"userMessage","content":[{"type":"text","text":"same prompt"}]},
                {"id":"item-2","type":"agentMessage","text":"same reply"}
            ]},
            {"id":"current","status":"inProgress","itemsView":"full","items":[
                {"id":"item-3","type":"userMessage","content":[{"type":"text","text":"same prompt"}]}
            ]}
        ]},"model":"native","reasoningEffort":"high"});
        state.remember(&native);
        (state, native)
    }

    fn legacy_twins(native: &Value) -> Value {
        let mut native = native.clone();
        native["thread"]["turns"][1]["items"]
            .as_array_mut()
            .unwrap()
            .extend([
                json!({"id":"item-4","type":"agentMessage","text":"same reply"}),
                json!({"id":"item-5","type":"agentMessage","text":"same reply"}),
            ]);
        native
    }

    fn legacy_item(method: &str, item: Value) -> Value {
        json!({"method":method,"params":{"threadId":"t","turnId":"current","item":item}})
    }

    #[test]
    fn legacy_full_history_and_unfinished_core_streams_have_separate_identity_spaces() {
        let (mut state, native) = legacy_fixture();
        let canonical = legacy_twins(&native);
        let before = state.sequence;
        // This actual Core ID happens to equal an independently materialized alias.
        state.reduce(&legacy_item(
            "item/started",
            json!({"id":"item-4","type":"agentMessage","text":"a new "}),
        ));
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"current","itemId":"item-4","delta":"partial"}}));
        state.reduce(&legacy_item(
            "item/started",
            json!({"id":"reason-core","type":"reasoning","summary":[],"content":[]}),
        ));
        state.reduce(&json!({"method":"item/reasoning/summaryPartAdded","params":{"threadId":"t","turnId":"current","itemId":"reason-core","summaryIndex":0}}));
        state.reduce(&json!({"method":"item/reasoning/summaryTextDelta","params":{"threadId":"t","turnId":"current","itemId":"reason-core","summaryIndex":0,"delta":"thinking"}}));
        state.reduce(&json!({"method":"item/reasoning/textDelta","params":{"threadId":"t","turnId":"current","itemId":"reason-core","contentIndex":0,"delta":"details"}}));
        for _ in 0..3 {
            state.remember_after(&canonical, Some(before));
            let snapshot = state.snapshot("t", json!([]));
            assert_eq!(snapshot["thread"], canonical["thread"]);
            assert_eq!(
                snapshot["provisionalItems"],
                json!([
                    {"turnId":"current","item":{"id":"item-4","type":"agentMessage","text":"a new partial"}},
                    {"turnId":"current","item":{"id":"reason-core","type":"reasoning","summary":["thinking"],"content":["details"]}}
                ])
            );
        }
        state.reduce(&legacy_item(
            "item/completed",
            json!({"id":"item-4","type":"agentMessage","text":"a new partial"}),
        ));
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(
            snapshot["thread"], canonical["thread"],
            "Retiring a provisional ID must never delete the independent native alias."
        );
        assert_eq!(snapshot["provisionalItems"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["provisionalItems"][0]["item"]["id"], "reason-core");
    }

    #[test]
    fn legacy_completion_persistence_gap_never_unions_core_ids_or_loses_newer_partial() {
        let (mut state, native) = legacy_fixture();
        let (_, events) = state.subscribe();
        state.reduce(&legacy_item(
            "item/started",
            json!({"id":"core-a","type":"agentMessage","text":"same reply"}),
        ));
        let started = events.recv().unwrap();
        assert_eq!(started["method"], "item/started");
        let before = state.sequence;
        state.reduce(&legacy_item(
            "item/completed",
            json!({"id":"core-a","type":"agentMessage","text":"same reply"}),
        ));
        let completed = events.recv().unwrap();
        assert_eq!(completed["method"], "gateway/legacy-history-changed");
        assert_eq!(
            completed["params"],
            json!({"threadId":"t","turnId":"current","itemId":"core-a","reason":"itemCompleted"})
        );
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"current","itemId":"core-b","delta":"a later suffix"}}));
        // Core completion precedes persistence of its legacy equivalent. This read
        // legitimately still contains only the user alias.
        state.remember_after(&native, Some(before));
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(snapshot["thread"], native["thread"]);
        assert_eq!(
            snapshot["provisionalItems"],
            json!([
                {"turnId":"current","item":{"id":"core-b","type":"agentMessage","text":"a later suffix","phase":null,"partialFromJoin":true}}
            ])
        );
        let canonical = legacy_twins(&native);
        state.remember_after(&canonical, Some(before));
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(snapshot["thread"], canonical["thread"]);
        assert_eq!(
            snapshot["thread"]["turns"][1]["items"][1]["text"],
            snapshot["thread"]["turns"][1]["items"][2]["text"]
        );
        assert_ne!(
            snapshot["thread"]["turns"][1]["items"][1]["id"],
            snapshot["thread"]["turns"][1]["items"][2]["id"]
        );
        assert_eq!(snapshot["provisionalItems"][0]["item"]["id"], "core-b");
        assert!(!state.live_items.keys().any(|(_, _, id)| id == "core-a"));
    }

    #[test]
    fn legacy_user_and_compaction_lifecycle_only_signal_full_history_refresh() {
        let (mut state, native) = legacy_fixture();
        let (_, events) = state.subscribe();
        for (item_type, item_id) in [
            ("userMessage", "user-core"),
            ("contextCompaction", "compaction-core"),
        ] {
            for (method, reason) in [
                ("item/started", "itemStarted"),
                ("item/completed", "itemCompleted"),
            ] {
                state.reduce(&legacy_item(
                    method,
                    json!({"id":item_id,"type":item_type,"content":[]}),
                ));
                let event = events.recv().unwrap();
                assert_eq!(event["method"], "gateway/legacy-history-changed");
                assert_eq!(
                    event["params"],
                    json!({"threadId":"t","turnId":"current","itemId":item_id,"reason":reason})
                );
                assert_eq!(state.snapshot("t", json!([]))["thread"], native["thread"]);
                assert_eq!(
                    state.snapshot("t", json!([]))["provisionalItems"],
                    json!([])
                );
            }
        }
    }

    #[test]
    fn newer_legacy_terminal_metadata_preserves_full_read_aliases_and_retires_provisionals() {
        let (mut state, native) = legacy_fixture();
        state.reduce(&legacy_item(
            "item/started",
            json!({"id":"core-a","type":"agentMessage","text":"partial"}),
        ));
        let (_, events) = state.subscribe();
        let before = state.sequence;
        let terminal = json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"current","status":"failed","error":{"message":"native failure"},"completedAt":101,"itemsView":"summary","items":[{"id":"core-summary","type":"agentMessage","text":"same reply"}]}}});
        state.reduce(&terminal);
        let forwarded = events.recv().unwrap();
        assert_eq!(forwarded["method"], "turn/completed");
        assert_eq!(forwarded["params"]["turn"]["items"], json!([]));
        assert_eq!(
            forwarded["params"]["turn"]["error"],
            terminal["params"]["turn"]["error"]
        );
        assert_eq!(
            state.snapshot("t", json!([]))["thread"]["turns"][1]["items"],
            native["thread"]["turns"][1]["items"]
        );
        assert_eq!(
            state.snapshot("t", json!([]))["provisionalItems"],
            json!([])
        );
        let canonical = legacy_twins(&native);
        state.remember_after(&canonical, Some(before));
        let turn = state.snapshot("t", json!([]))["thread"]["turns"][1].clone();
        assert_eq!(turn["items"], canonical["thread"]["turns"][1]["items"]);
        assert_eq!(turn["itemsView"], "full");
        assert_eq!(turn["status"], "failed");
        assert_eq!(turn["completedAt"], 101);
        assert_eq!(turn["error"]["message"], "native failure");
    }

    #[test]
    fn terminal_completion_racing_a_legacy_read_stays_dirty_until_fresh_full_history() {
        let (mut state, native) = legacy_fixture();
        let read_started = state.sequence;
        state.reduce(&legacy_item(
            "item/completed",
            json!({"id":"core-finished","type":"agentMessage","text":"same reply"}),
        ));
        state.reduce(&json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"current","status":"completed","itemsView":"summary","items":[{"id":"core-summary","type":"agentMessage","text":"same reply"}]}}}));
        state.remember_after(&native, Some(read_started));
        let stale = state.snapshot("t", json!([]));
        assert_eq!(stale["thread"]["turns"][1]["status"], "completed");
        assert_eq!(
            stale["thread"]["turns"][1]["items"],
            native["thread"]["turns"][1]["items"]
        );
        assert_eq!(
            stale["legacyHistoryDirty"], true,
            "A terminal metadata merge must not stop the browser's final-history retry."
        );
        // A failed/missing snapshot cannot advance the accepted full-read fence.
        state.remember_after(&json!({"error":"native read failed"}), Some(state.sequence));
        assert_eq!(state.snapshot("t", json!([]))["legacyHistoryDirty"], true);
        let mut final_history = legacy_twins(&native);
        final_history["thread"]["turns"][1]["status"] = json!("completed");
        state.remember_after(&final_history, Some(state.sequence));
        let fresh = state.snapshot("t", json!([]));
        assert_eq!(fresh["thread"], final_history["thread"]);
        assert_eq!(fresh["legacyHistoryDirty"], false);
        assert_eq!(fresh["provisionalItems"], json!([]));
    }

    #[test]
    fn accepted_active_read_after_core_completion_clears_dirty_despite_native_persistence_gap() {
        let (mut state, native) = legacy_fixture();
        state.reduce(&legacy_item(
            "item/completed",
            json!({"id":"core-finished","type":"agentMessage","text":"same reply"}),
        ));
        assert_eq!(state.snapshot("t", json!([]))["legacyHistoryDirty"], true);
        // The native legacy equivalent can lag Core completion. Active polling
        // still runs, but a successful read after the event must not spin dirty
        // refreshes indefinitely or invent the missing alias.
        state.remember_after(&native, Some(state.sequence));
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(snapshot["thread"], native["thread"]);
        assert_eq!(snapshot["legacyHistoryDirty"], false);
        let final_history = legacy_twins(&native);
        state.remember_after(&final_history, Some(state.sequence));
        assert_eq!(
            state.snapshot("t", json!([]))["thread"],
            final_history["thread"]
        );
        assert_eq!(state.snapshot("t", json!([]))["legacyHistoryDirty"], false);
    }

    #[test]
    fn legacy_provisionals_are_epoch_bound_hidden_for_archives_and_closed_by_scope() {
        let (mut state, native) = legacy_fixture();
        state.reduce(&legacy_item(
            "item/started",
            json!({"id":"old-core","type":"agentMessage","text":"old"}),
        ));
        state.connection(Connection::Reconnecting);
        assert_eq!(
            state.snapshot("t", json!([]))["provisionalItems"],
            json!([])
        );
        assert_eq!(state.snapshot("t", json!([]))["thread"], native["thread"]);
        state.reduce(&json!({"method":"item/reasoning/textDelta","params":{"threadId":"t","turnId":"current","itemId":"new-core","contentIndex":0,"delta":"fresh suffix"}}));
        state.remember_after(&native, Some(state.sequence));
        assert_eq!(
            state.snapshot("t", json!([]))["provisionalItems"][0]["item"],
            json!({"id":"new-core","type":"reasoning","summary":[],"content":["fresh suffix"],"partialFromJoin":true})
        );
        state.connection(Connection::Connected);
        assert_eq!(
            state.snapshot("t", json!([]))["provisionalItems"][0]["item"]["id"],
            "new-core"
        );
        state.reduce(&json!({"method":"thread/archived","params":{"threadId":"t"}}));
        assert_eq!(
            state.snapshot("t", json!([]))["provisionalItems"],
            json!([])
        );
        assert!(state.provisional_items.is_empty());
        state.reduce(&legacy_item(
            "item/started",
            json!({"id":"archived-core","type":"agentMessage","text":"hidden"}),
        ));
        assert!(state.provisional_items.is_empty());
        state.archived_threads.remove("t");
        state.reduce(&legacy_item(
            "item/started",
            json!({"id":"scoped-core","type":"agentMessage","text":"fresh"}),
        ));
        state.forget_outside_scope(
            "t",
            &DirectoryScope::Rejected {
                cause: "Fixture scope closed".to_owned(),
            },
        );
        assert!(state.provisional_items.is_empty());
        assert!(state.snapshot("t", json!([]))["thread"].is_null());
    }

    #[test]
    fn rejected_subscription_candidates_cannot_resurrect_after_a_later_legacy_read() {
        let (_, native) = legacy_fixture();
        let mut state = State::default();
        state.connection(Connection::Connected);
        state.subscribed.insert("t".to_owned());
        let before = state.sequence;
        state.reduce(&legacy_item(
            "item/started",
            json!({"id":"old-core","type":"agentMessage","text":"old prefix"}),
        ));
        state.reduce(&legacy_item(
            "item/completed",
            json!({"id":"finished-core","type":"agentMessage","text":"old completion"}),
        ));
        assert!(!state.provisional_items.is_empty());
        assert!(!state.legacy_history_dirty_sequences.is_empty());
        state.clear_provisional_streams("t");
        state.remember_after(&native, Some(before));
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(snapshot["thread"], native["thread"]);
        assert_eq!(snapshot["provisionalItems"], json!([]));
        assert_eq!(snapshot["legacyHistoryDirty"], false);
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"current","itemId":"old-core","delta":"fresh suffix"}}));
        assert_eq!(
            state.snapshot("t", json!([]))["provisionalItems"],
            json!([
                {"turnId":"current","item":{"id":"old-core","type":"agentMessage","text":"fresh suffix","phase":null,"partialFromJoin":true}}
            ])
        );
    }

    #[test]
    fn pre_snapshot_stream_candidates_are_exported_only_for_validated_legacy_history() {
        let (_, native) = legacy_fixture();
        for mode in ["legacy", "paginated"] {
            let mut state = State::default();
            state.connection(Connection::Connected);
            state.subscribed.insert("t".to_owned());
            state.reduce(&legacy_item(
                "item/started",
                json!({"id":"core-a","type":"agentMessage","text":"prefix"}),
            ));
            state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"current","itemId":"core-a","delta":" suffix"}}));
            assert_eq!(
                state.snapshot("t", json!([]))["provisionalItems"],
                json!([])
            );
            let mut native = native.clone();
            native["thread"]["historyMode"] = json!(mode);
            state.remember_after(&native, Some(1));
            let provisional = state.snapshot("t", json!([]))["provisionalItems"].clone();
            if mode == "legacy" {
                assert_eq!(
                    provisional,
                    json!([{ "turnId":"current","item":{"id":"core-a","type":"agentMessage","text":"prefix suffix"} }])
                );
            } else {
                assert_eq!(provisional, json!([]));
                assert!(state.provisional_items.is_empty());
            }
            assert_eq!(state.snapshot("t", json!([]))["thread"], native["thread"]);
        }
    }

    #[test]
    fn pending_snapshot_and_stream_share_the_exact_request_generation_token() {
        let mut state = State::connecting(vec![], "gateway-instance".to_owned());
        state.connection(Connection::Connected);
        state.remember(&json!({"thread":{"id":"t","cwd":"/tmp","turns":[]}}));
        let (_, receiver) = state.subscribe();
        state.reduce(&json!({"id":7,"method":"item/tool/requestUserInput","params":{"threadId":"t","questions":[]}}));
        let event = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(event["request"], snapshot["pending"][0]);
        assert_eq!(event["request"]["requestToken"], "gateway-instance:1:2");
        assert_eq!(event["params"], json!({"threadId":"t","questions":[]}));
        assert_eq!(event["requestId"], 7);
    }

    #[test]
    fn identical_pending_replay_preserves_token_only_in_the_same_native_generation() {
        let mut state = State::connecting(vec![], "instance".to_owned());
        state.remember(&json!({"thread":{"id":"t","cwd":"/tmp","turns":[]}}));
        let request = json!({"id":7,"method":"item/tool/requestUserInput",
            "params":{"threadId":"t","questions":[{"id":"q","question":"Choose"}]}});
        state.reduce(&request);
        let bootstrap_token = state.pending["7"]["requestToken"].clone();
        assert_eq!(bootstrap_token, "instance:1:1");
        state.connection(Connection::Connected);
        let (_, events) = state.subscribe();
        state.reduce(&request);
        assert_eq!(state.pending["7"]["requestToken"], bootstrap_token);
        assert_eq!(
            events.recv().unwrap()["request"]["requestToken"],
            bootstrap_token
        );
        assert_eq!(
            state.snapshot("t", json!([]))["pending"][0]["requestToken"],
            bootstrap_token
        );

        let mut changed = request.clone();
        changed["params"]["questions"][0]["question"] = json!("New question");
        state.reduce(&changed);
        let changed_token = state.pending["7"]["requestToken"].clone();
        assert_ne!(changed_token, bootstrap_token);
        state.reduce(&changed);
        assert_eq!(state.pending["7"]["requestToken"], changed_token);
        state.reduce(&json!({"method":"serverRequest/resolved","params":{"requestId":7}}));
        state.reduce(&changed);
        let replacement_token = state.pending["7"]["requestToken"].clone();
        assert_ne!(
            replacement_token, changed_token,
            "Resolved requests cannot retain an earlier lifetime's token"
        );

        state.connection(Connection::Reconnecting);
        state.reduce(&changed);
        let reconnect_token = state.pending["7"]["requestToken"].clone();
        assert_ne!(reconnect_token, replacement_token);
        assert!(reconnect_token.as_str().unwrap().starts_with("instance:2:"));
        state.connection(Connection::Connected);
        state.reduce(&changed);
        assert_eq!(state.pending["7"]["requestToken"], reconnect_token);
        state.request_namespace = "replacement-instance".to_owned();
        state.reduce(&changed);
        assert_ne!(state.pending["7"]["requestToken"], reconnect_token);
    }

    fn plan_fixture() -> (State, Value) {
        let mut state = State::default();
        state.connection(Connection::Connected);
        let native = json!({"thread":{"id":"t","cwd":"/tmp","turns":[
            {"id":"first","status":"completed","items":[]},
            {"id":"second","status":"inProgress","items":[]}
        ]},"model":"native","reasoningEffort":"high"});
        state.remember(&native);
        (state, native)
    }

    fn native_plan(turn_id: &str, explanation: Value) -> Value {
        json!({"method":"turn/plan/updated","params":{
            "threadId":"t","turnId":turn_id,"explanation":explanation,
            "plan":[
                {"step":"PENDING <script>sentinel</script>","status":"pending"},
                {"step":"IN PROGRESS SENTINEL","status":"inProgress"},
                {"step":"COMPLETED SENTINEL","status":"completed"}
            ]
        }})
    }

    #[test]
    fn plan_notifications_preserve_latest_original_payload_per_turn_in_native_order() {
        let (mut state, mut other_thread) = plan_fixture();
        let second = native_plan("second", Value::Null);
        state.reduce(&second);
        state.reduce(&native_plan("first", json!("old explanation")));
        let mut first = native_plan("first", json!("EXPLANATION <b>sentinel</b>"));
        first["params"]["futureMetadata"] = json!({"label":"PRESERVED SENTINEL"});
        state.reduce(&first);
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(
            snapshot["planUpdates"],
            json!([first["params"], second["params"]])
        );
        assert_eq!(state.plan_updates.len(), 2);
        assert!(snapshot["thread"]["turns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|turn| turn["items"] == json!([])));
        assert_eq!(state.snapshot("other", json!([]))["planUpdates"], json!([]));
        other_thread["thread"]["id"] = json!("other");
        state.remember(&other_thread);
        let mut other = native_plan("second", json!("OTHER THREAD PLAN"));
        other["params"]["threadId"] = json!("other");
        state.reduce(&other);
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([first["params"], second["params"]])
        );
        assert_eq!(
            state.snapshot("other", json!([]))["planUpdates"],
            json!([other["params"]])
        );
        state.reduce(&json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"second","status":"completed","items":[]}}}));
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"][1],
            second["params"]
        );
    }

    #[test]
    fn foreign_unconfirmed_and_malformed_plan_notifications_are_neither_stored_nor_published() {
        let (mut state, _) = plan_fixture();
        let (_, receiver) = state.subscribe();
        let valid = native_plan("second", Value::Null);
        for (field, value) in [
            ("threadId", json!("foreign")),
            ("threadId", Value::Null),
            ("threadId", json!("")),
            ("turnId", json!("")),
            ("explanation", json!(17)),
            ("plan", Value::Null),
            ("plan", json!([{"step":"x","status":"invented"}])),
            ("plan", json!([{"step":17,"status":"pending"}])),
        ] {
            let mut invalid = valid.clone();
            invalid["params"][field] = value;
            state.reduce(&invalid);
        }
        state.subscribed.insert("unconfirmed".to_owned());
        let mut unconfirmed = valid.clone();
        unconfirmed["params"]["threadId"] = json!("unconfirmed");
        state.reduce(&unconfirmed);
        assert!(state.plan_updates.is_empty());
        assert!(state.pending_plan_updates.is_empty());
        assert!(receiver.try_recv().is_err());
        let mut absent_explanation = valid.clone();
        absent_explanation["params"]
            .as_object_mut()
            .unwrap()
            .remove("explanation");
        state.reduce(&absent_explanation);
        assert_eq!(
            receiver.try_recv().unwrap()["params"],
            absent_explanation["params"]
        );
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([absent_explanation["params"]])
        );
        let mut empty_plan = valid;
        empty_plan["params"]["plan"] = json!([]);
        state.reduce(&empty_plan);
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([empty_plan["params"]])
        );
    }

    #[test]
    fn early_plan_update_is_not_fabricated_as_a_turn_and_catches_up_with_native_history() {
        let (mut state, mut history) = plan_fixture();
        let after = state.sequence;
        let early = native_plan("future", json!("EARLY UPDATE SENTINEL"));
        state.reduce(&early);
        assert_eq!(state.pending_plan_updates.len(), 1);
        assert_eq!(state.pending_plan_updates["t"].params, early["params"]);
        assert_eq!(state.snapshot("t", json!([]))["planUpdates"], json!([]));
        assert_eq!(state.threads["t"]["turns"].as_array().unwrap().len(), 2);
        // An older read must not discard a notification emitted after it began.
        state.remember_after(&history, Some(after));
        assert_eq!(state.pending_plan_updates["t"].params, early["params"]);
        history["thread"]["turns"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"future","status":"inProgress","items":[]}));
        state.remember_after(&history, Some(state.sequence));
        assert!(state.pending_plan_updates.is_empty());
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([early["params"]])
        );
        assert_eq!(state.threads["t"]["turns"][2]["items"], json!([]));
    }

    #[test]
    fn only_latest_unknown_turn_plan_is_retained_until_native_turn_notification_confirms_it() {
        let (mut state, history) = plan_fixture();
        for turn in 0..16 {
            state.reduce(&native_plan(
                &format!("future-{turn}"),
                json!(turn.to_string()),
            ));
        }
        assert!(state.plan_updates.is_empty());
        assert!(state.turn_sequences.is_empty());
        assert_eq!(state.pending_plan_updates.len(), 1);
        assert_eq!(
            state.pending_plan_updates["t"].params["turnId"],
            "future-15"
        );
        state.remember_after(&history, Some(state.sequence));
        assert_eq!(state.pending_plan_updates.len(), 1);
        assert_eq!(state.snapshot("t", json!([]))["planUpdates"], json!([]));
        let expected = state.pending_plan_updates["t"].params.clone();
        state.reduce(&json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"future-15","status":"inProgress","items":[]}}}));
        assert!(state.pending_plan_updates.is_empty());
        assert_eq!(state.plan_updates.len(), 1);
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([expected])
        );
        state.reduce(&json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"future-0","status":"inProgress","items":[]}}}));
        assert_eq!(state.plan_updates.len(), 1);
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([expected])
        );
    }

    #[test]
    fn plan_updates_newer_than_a_history_read_survive_until_an_authoritative_exclusion() {
        let (mut state, mut history) = plan_fixture();
        state.reduce(&native_plan("first", json!("COMPLETED PLAN")));
        let after = state.sequence;
        let newer = native_plan("second", json!("NEWER PLAN"));
        state.reduce(&newer);
        history["thread"]["turns"].as_array_mut().unwrap().pop();
        state.remember_after(&history, Some(after));
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"][1],
            newer["params"]
        );
        assert_eq!(state.threads["t"]["turns"][1]["id"], "second");
        let after = state.sequence;
        state.remember_after(&history, Some(after));
        assert_eq!(state.plan_updates.len(), 1);
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"][0]["turnId"],
            "first"
        );
        // Repeated native history reads do not erase completed live updates for
        // a turn that remains in that same connection's authoritative history.
        state.remember_after(&history, Some(state.sequence));
        assert_eq!(state.plan_updates.len(), 1);
    }

    #[test]
    fn replacement_native_connection_clears_ephemeral_plan_updates_without_relabeling() {
        let (mut state, history) = plan_fixture();
        state.reduce(&native_plan("second", json!("OLD CONNECTION PLAN")));
        state.reduce(&native_plan("future", json!("OLD EARLY CONNECTION PLAN")));
        state.connection(Connection::Reconnecting);
        assert!(state.plan_updates.is_empty());
        assert!(state.pending_plan_updates.is_empty());
        assert_eq!(state.snapshot("t", json!([]))["planUpdates"], json!([]));
        state.connection(Connection::Connected);
        state.remember_after(&history, Some(0));
        assert_eq!(state.epoch, 2);
        assert_eq!(state.snapshot("t", json!([]))["planUpdates"], json!([]));
        let current = native_plan("second", json!("NEW CONNECTION PLAN"));
        state.reduce(&current);
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([current["params"]])
        );
    }

    #[test]
    fn closing_project_scope_forgets_plan_payload_and_rejects_later_notifications() {
        let root = std::env::temp_dir().join(format!("gateway-plan-scope-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let (mut state, mut history) = plan_fixture();
        state.allowed_roots = vec![root.canonicalize().unwrap()];
        history["thread"]["cwd"] = json!(root);
        state.remember(&history);
        state.reduce(&native_plan("second", json!("PRIVATE PLAN SENTINEL")));
        state.reduce(&native_plan("future", json!("PRIVATE EARLY PLAN SENTINEL")));
        assert_eq!(state.plan_updates.len(), 1);
        assert_eq!(state.pending_plan_updates.len(), 1);
        let (_, receiver) = state.subscribe();
        state.reduce(&json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"cwd":"/"}}}));
        assert_eq!(
            receiver.try_recv().unwrap()["method"],
            "gateway/recoveryError"
        );
        assert!(state.plan_updates.is_empty());
        assert!(state.pending_plan_updates.is_empty());
        assert_eq!(state.snapshot("t", json!([]))["planUpdates"], json!([]));
        state.reduce(&native_plan("second", json!("OUTSIDE PLAN SENTINEL")));
        assert!(receiver.try_recv().is_err());
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn archive_and_restore_close_pending_controls_until_a_fresh_native_resume() {
        let mut state = State::default();
        let native = json!({"thread":{"id":"t","cwd":"/tmp","turns":[]},"model":"native","reasoningEffort":"high"});
        state.remember(&native);
        state
            .pending
            .insert("approval".to_owned(), json!({"params":{"threadId":"t"}}));
        state.reduce(&json!({"method":"thread/archived","params":{"threadId":"t"}}));
        let archived = state.snapshot("t", json!([{"id":"stale-queue"}]));
        assert_eq!(archived["accessMode"], "archived");
        assert_eq!(archived["settings"], json!({}));
        assert_eq!(archived["queue"], json!([]));
        assert!(state.pending.is_empty());
        state.reduce(&json!({"id":"stale-approval","method":"item/commandExecution/requestApproval","params":{"threadId":"t"}}));
        assert!(state.pending.is_empty());
        state.remember_after(&native, Some(0));
        assert_eq!(
            state.snapshot("t", json!([]))["accessMode"],
            "archived",
            "late pre-archive resume must not enable writes"
        );
        state.reduce(&json!({"method":"thread/unarchived","params":{"threadId":"t"}}));
        assert_eq!(state.snapshot("t", json!([]))["accessMode"], "archived");
        state.remember_after(&native, Some(state.sequence));
        let active = state.snapshot("t", json!([]));
        assert_eq!(active["accessMode"], "active");
        assert_eq!(active["settings"]["effort"], "high");
    }

    fn active_stream_fixture() -> (State, Value) {
        let mut state = State::default();
        state.connection(Connection::Connected);
        let persisted = json!({"thread":{"id":"t","cwd":"/tmp","turns":[{"id":"turn","status":"inProgress","items":[{"id":"user","type":"userMessage","content":[{"type":"text","text":"run"}]}]}]},"model":"one","reasoningEffort":"high"});
        state.remember(&persisted);
        state.reduce(&json!({"method":"item/started","params":{"threadId":"t","turnId":"turn","item":{"id":"reply","type":"agentMessage","text":""}}}));
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"reply","delta":"partial"}}));
        (state, persisted)
    }

    #[test]
    fn repeated_active_resume_keeps_pre_request_stream_and_continues_without_duplication() {
        let (mut state, persisted) = active_stream_fixture();
        for _ in 0..3 {
            let after = state.sequence;
            state.remember_after(&persisted, Some(after));
            let snapshot = state.snapshot("t", json!([]));
            let items = snapshot["thread"]["turns"][0]["items"].as_array().unwrap();
            assert_eq!(items.len(), 2);
            assert_eq!(items[1]["id"], "reply");
            assert_eq!(items[1]["text"], "partial");
        }
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"reply","delta":" continued"}}));
        assert_eq!(
            state.snapshot("t", json!([]))["thread"]["turns"][0]["items"][1]["text"],
            "partial continued"
        );
    }

    #[test]
    fn completed_persisted_item_and_terminal_turn_replace_transient_cache_by_id() {
        let (mut state, mut persisted) = active_stream_fixture();
        state.reduce(&json!({"method":"item/completed","params":{"threadId":"t","turnId":"turn","item":{"id":"reply","type":"agentMessage","text":"event completion"}}}));
        persisted["thread"]["turns"][0]["items"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"reply","type":"agentMessage","text":"persisted completion"}));
        state.remember_after(&persisted, Some(state.sequence));
        let items = &state.threads["t"]["turns"][0]["items"];
        assert_eq!(items.as_array().unwrap().len(), 2);
        assert_eq!(items[1]["text"], "persisted completion");
        // A stale streaming event racing a terminal persisted snapshot must not
        // resurrect its active status or duplicate its completed assistant item.
        let after = state.sequence;
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"reply","delta":" stale"}}));
        persisted["thread"]["turns"][0]["status"] = json!("completed");
        state.remember_after(&persisted, Some(after));
        assert_eq!(state.threads["t"]["turns"][0]["status"], "completed");
        assert_eq!(
            state.threads["t"]["turns"][0]["items"][1]["text"],
            "persisted completion"
        );
        assert_eq!(
            state.threads["t"]["turns"][0]["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn transient_items_are_not_carried_into_another_turn() {
        let (mut state, mut persisted) = active_stream_fixture();
        persisted["thread"]["turns"][0]["id"] = json!("next-turn");
        state.remember_after(&persisted, Some(state.sequence));
        let turns = state.threads["t"]["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["id"], "next-turn");
        assert_eq!(turns[0]["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn transient_items_are_not_carried_into_another_native_connection_epoch() {
        let (mut state, persisted) = active_stream_fixture();
        let after = state.sequence;
        state.connection(Connection::Reconnecting);
        state.connection(Connection::Connected);
        assert_eq!(state.epoch, 2);
        state.remember_after(&persisted, Some(after));
        assert_eq!(
            state.threads["t"]["turns"][0]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(state.live_items.is_empty());
    }

    #[test]
    fn bootstrap_completed_item_replaced_by_canonical_history_survives_connected_and_reconnect() {
        let (mut state, mut history) = active_stream_fixture();
        state.connection(Connection::Reconnecting);
        let after = state.sequence;
        state.reduce(&json!({"method":"item/completed","params":{
            "threadId":"t","turnId":"turn","item":{
                "id":"reply","type":"agentMessage","text":"new socket completed event"
            }
        }}));
        history["thread"]["turns"][0]["items"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"reply","type":"agentMessage","text":"canonical persisted reply"}));
        state.remember_after(&history, Some(after));
        state.connection(Connection::Connected);
        assert_eq!(state.epoch, 2);
        let items = &state.snapshot("t", json!([]))["thread"]["turns"][0]["items"];
        assert_eq!(items.as_array().unwrap().len(), 2);
        assert_eq!(items[1]["id"], "reply");
        assert_eq!(items[1]["text"], "canonical persisted reply");
        assert!(state.live_items.is_empty());
        state.connection(Connection::Reconnecting);
        assert_eq!(
            state.threads["t"]["turns"][0]["items"][1]["text"], "canonical persisted reply",
            "A later reconnect cannot remove an item already confirmed by native history"
        );
    }

    #[test]
    fn bootstrap_partial_item_survives_connected_and_continues_without_old_prefix() {
        let (mut state, history) = active_stream_fixture();
        state.connection(Connection::Reconnecting);
        state.remember_after(&history, Some(state.sequence));
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{
            "threadId":"t","turnId":"turn","itemId":"reply","delta":"new socket suffix"
        }}));
        state.remember_after(&history, Some(state.sequence));
        state.connection(Connection::Connected);
        state.remember_after(&history, Some(state.sequence));
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{
            "threadId":"t","turnId":"turn","itemId":"reply","delta":" continued"
        }}));
        let items = &state.snapshot("t", json!([]))["thread"]["turns"][0]["items"];
        assert_eq!(items.as_array().unwrap().len(), 2);
        assert_eq!(items[1]["id"], "reply");
        assert_eq!(items[1]["text"], "new socket suffix continued");
        assert_eq!(items[1]["partialFromJoin"], true);
        assert_eq!(state.epoch, 2);
    }

    #[test]
    fn first_socket_bootstrap_items_and_plans_survive_epoch_publication() {
        let mut state = State::default();
        let history = json!({"thread":{"id":"t","cwd":"/tmp","turns":[
            {"id":"turn","status":"inProgress","items":[]}
        ]}});
        state.remember_after(&history, Some(0));
        let plan = native_plan("turn", json!("FIRST SOCKET PLAN"));
        state.reduce(&plan);
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{
            "threadId":"t","turnId":"turn","itemId":"reply","delta":"first socket suffix"
        }}));
        state.remember_after(&history, Some(state.sequence));
        state.connection(Connection::Connected);
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(snapshot["epoch"], 1);
        assert_eq!(snapshot["planUpdates"], json!([plan["params"]]));
        assert_eq!(
            snapshot["thread"]["turns"][0]["items"][0]["text"],
            "first socket suffix"
        );
    }

    #[test]
    fn bootstrap_plans_keep_only_new_generation_and_wait_for_native_turn_confirmation() {
        let (mut state, mut history) = plan_fixture();
        state.reduce(&native_plan("first", json!("OLD COMPLETED PLAN")));
        state.reduce(&native_plan("old-future", json!("OLD UNCONFIRMED PLAN")));
        state.connection(Connection::Reconnecting);
        let current = native_plan("second", json!("BOOTSTRAP KNOWN PLAN"));
        let early = native_plan("future", json!("BOOTSTRAP EARLY PLAN"));
        state.reduce(&current);
        state.reduce(&early);
        state.remember_after(&history, Some(state.sequence));
        state.connection(Connection::Connected);
        assert_eq!(state.epoch, 2);
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([current["params"]])
        );
        assert_eq!(state.pending_plan_updates["t"].params, early["params"]);
        history["thread"]["turns"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"future","status":"inProgress","items":[]}));
        state.remember_after(&history, Some(state.sequence));
        assert_eq!(
            state.snapshot("t", json!([]))["planUpdates"],
            json!([current["params"], early["params"]])
        );
        assert!(state.pending_plan_updates.is_empty());
        state.reduce(&json!({"method":"turn/started","params":{
            "threadId":"t","turn":{"id":"old-future","status":"inProgress","items":[]}
        }}));
        assert_eq!(
            state.plan_updates.len(),
            2,
            "An old socket's pending plan must never reappear"
        );
    }

    #[test]
    fn failed_bootstrap_and_direct_connected_replacement_clear_transient_generation() {
        let (mut state, history) = active_stream_fixture();
        state.reduce(&native_plan("turn", json!("OLD PLAN")));
        state.connection(Connection::Reconnecting);
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{
            "threadId":"t","turnId":"turn","itemId":"reply","delta":"failed bootstrap suffix"
        }}));
        state.reduce(&native_plan("turn", json!("FAILED BOOTSTRAP PLAN")));
        state.connection(Connection::Reconnecting);
        assert_eq!(
            state.threads["t"]["turns"][0]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(state.live_items.is_empty());
        assert!(state.plan_updates.is_empty());
        state.connection(Connection::Connected);
        state.remember_after(&history, Some(state.sequence));
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{
            "threadId":"t","turnId":"turn","itemId":"reply","delta":"replacement suffix"
        }}));
        state.reduce(&native_plan("turn", json!("REPLACEMENT PLAN")));
        state.connection(Connection::Connected);
        assert_eq!(state.epoch, 3);
        assert_eq!(
            state.threads["t"]["turns"][0]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(state.live_items.is_empty());
        assert_eq!(state.snapshot("t", json!([]))["planUpdates"], json!([]));
    }

    #[test]
    fn orphan_agent_delta_materializes_partial_item_until_canonical_completion() {
        let mut state = State::default();
        state.connection(Connection::Connected);
        let persisted = json!({"thread":{"id":"t","cwd":"/tmp","turns":[{"id":"turn","status":"inProgress","items":[{"id":"user","type":"userMessage","content":[]}]}]},"model":"one","reasoningEffort":"high"});
        state.remember(&persisted);
        // Native resume replays no item/started or prior text to a late subscriber.
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"reply","delta":"observed suffix"}}));
        state.remember_after(&persisted, Some(state.sequence));
        let snapshot = state.snapshot("t", json!([]));
        let item = &snapshot["thread"]["turns"][0]["items"][1];
        assert_eq!(item["type"], "agentMessage");
        assert_eq!(item["id"], "reply");
        assert_eq!(item["text"], "observed suffix");
        assert_eq!(item["partialFromJoin"], true);
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"reply","delta":" continued"}}));
        state.reduce(&json!({"method":"item/completed","params":{"threadId":"t","turnId":"turn","item":{"id":"reply","type":"agentMessage","text":"missing prefix observed suffix continued"}}}));
        let items = state.threads["t"]["turns"][0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1]["text"], "missing prefix observed suffix continued");
        assert!(items[1].get("partialFromJoin").is_none());
    }

    #[test]
    fn orphan_delta_after_reconnect_cannot_append_to_old_epoch_text() {
        let (mut state, _) = active_stream_fixture();
        state.connection(Connection::Reconnecting);
        assert_eq!(
            state.threads["t"]["turns"][0]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        state.connection(Connection::Connected);
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"reply","delta":"new connection suffix"}}));
        let item = &state.snapshot("t", json!([]))["thread"]["turns"][0]["items"][1];
        assert_eq!(item["text"], "new connection suffix");
        assert_eq!(item["partialFromJoin"], true);
        assert_eq!(state.epoch, 2);
    }

    #[test]
    fn ordinary_resolution_and_dynamic_completion_have_distinct_contracts() {
        let mut state = State::default();
        state.subscribed.insert("t".to_owned());
        state.reduce(
            &json!({"id":1,"method":"item/tool/call","params":{"threadId":"t","callId":"c"}}),
        );
        state.reduce(
            &json!({"id":2,"method":"item/tool/requestUserInput","params":{"threadId":"t"}}),
        );
        assert_eq!(state.pending.len(), 2);
        assert!(!state.pending["1"]["canRespond"].as_bool().unwrap());
        state.reduce(&json!({"method":"serverRequest/resolved","params":{"requestId":2}}));
        assert_eq!(state.pending.len(), 1);
        state.reduce(&json!({"method":"item/completed","params":{"threadId":"t","item":{"type":"dynamicToolCall","id":"c"}}}));
        assert!(state.pending.is_empty());
    }

    #[test]
    fn slow_subscriber_is_disconnected_and_epochs_advance() {
        let mut state = State::default();
        let (_, receiver) = state.subscribe();
        state.connection(Connection::Connected);
        state.connection(Connection::Reconnecting);
        state.connection(Connection::Connected);
        assert_eq!(state.epoch, 2);
        for _ in 0..EVENT_CAPACITY {
            state.emit("fixture", Value::Null, None);
        }
        assert!(state.peers.is_empty());
        while receiver.try_recv().is_ok() {}
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn snapshot_response_cannot_overwrite_events_received_after_request_started() {
        let mut state = State::default();
        let original = json!({"thread":{"id":"t","cwd":"/tmp","turns":[{"id":"turn","status":"inProgress","items":[{"id":"message","type":"agentMessage","text":"old"}]}]},"model":"one","reasoningEffort":"low"});
        state.remember(&original);
        let request_sequence = state.sequence;
        state.reduce(&json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"model":"two","effort":"high","cwd":"/other"}}}));
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"message","delta":" new"}}));
        state.reduce(&json!({"method":"thread/status/changed","params":{"threadId":"t","status":{"type":"active","activeFlags":["waitingOnApproval"]}}}));
        state.remember_after(&original, Some(request_sequence));
        let snapshot = state.snapshot("t", json!([]));
        assert_eq!(snapshot["settings"]["model"], "two");
        assert_eq!(snapshot["settings"]["effort"], "high");
        assert_eq!(snapshot["thread"]["cwd"], "/other");
        assert_eq!(
            snapshot["thread"]["status"]["activeFlags"][0],
            "waitingOnApproval"
        );
        assert_eq!(
            snapshot["thread"]["turns"][0]["items"][0]["text"],
            "old new"
        );
        state.reduce(&json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"turn","status":"completed","items":[]}}}));
        assert_eq!(state.threads["t"]["turns"][0]["status"], "completed");
        assert_eq!(
            state.threads["t"]["turns"][0]["items"][0]["text"],
            "old new"
        );
    }

    #[test]
    fn unrelated_threads_and_owner_only_requests_are_not_broadcast_to_browser() {
        let mut state = State::default();
        let (_, receiver) = state.subscribe();
        state.reduce(&json!({"method":"thread/started","params":{"thread":{"id":"outside","cwd":"/private"}}}));
        state.reduce(&json!({"id":5,"method":"item/tool/call","params":{"threadId":"outside","callId":"secret"}}));
        state.reduce(&json!({"id":6,"method":"account/login","params":{"verification":"secret"}}));
        assert!(state.pending.is_empty());
        assert!(state.threads.is_empty());
        let changed = receiver.try_recv().unwrap();
        assert_eq!(changed["method"], "gateway/historyChanged");
        assert_eq!(changed["params"], json!({}));
        assert!(receiver.try_recv().is_err());
        state.subscribed.insert("allowed".to_owned());
        state.reduce(
            &json!({"id":7,"method":"item/tool/requestUserInput","params":{"threadId":"allowed"}}),
        );
        assert!(state.pending.contains_key("7"));
        assert_eq!(receiver.try_recv().unwrap()["requestId"], 7);
    }

    #[test]
    fn changing_shared_cwd_outside_allowed_roots_closes_browser_projection() {
        let root = std::env::temp_dir().join(format!("gateway-scope-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut state = State {
            allowed_roots: vec![root.canonicalize().unwrap()],
            ..State::default()
        };
        state.remember(&json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root,"model":"one","reasoningEffort":"low"}));
        let (_, receiver) = state.subscribe();
        let request_sequence = state.sequence;
        state.reduce(&json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"model":"one","effort":"low","cwd":"/"}}}));
        assert!(!state.subscribed.contains("t"));
        assert!(!state.threads.contains_key("t"));
        state.remember_after(&json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root,"model":"one","reasoningEffort":"low"}), Some(request_sequence));
        assert!(
            !state.subscribed.contains("t"),
            "older snapshot cannot reopen an out-of-scope thread"
        );
        let event = receiver.try_recv().unwrap();
        assert_eq!(event["method"], "gateway/recoveryError");
        assert!(event["params"].get("threadSettings").is_none());
        state.reduce(&json!({"method":"item/agentMessage/delta","params":{"threadId":"t","delta":"private"}}));
        assert!(receiver.try_recv().is_err());
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn streamed_reasoning_plan_command_and_patch_survive_snapshot_fence() {
        let mut state = State::default();
        let original = json!({"thread":{"id":"t","cwd":"/tmp","turns":[{"id":"turn","status":"inProgress","items":[{"id":"thought","type":"reasoning","summary":[],"content":[]},{"id":"plan","type":"plan","text":""},{"id":"command","type":"commandExecution","aggregatedOutput":""},{"id":"patch","type":"fileChange","changes":[]}]}]},"model":"one","reasoningEffort":"high"});
        state.remember(&original);
        let after = state.sequence;
        for (method, params) in [
            (
                "item/reasoning/summaryPartAdded",
                json!({"itemId":"thought","summaryIndex":1}),
            ),
            (
                "item/reasoning/summaryTextDelta",
                json!({"itemId":"thought","summaryIndex":1,"delta":"summary"}),
            ),
            (
                "item/reasoning/textDelta",
                json!({"itemId":"thought","contentIndex":0,"delta":"thinking"}),
            ),
            ("item/plan/delta", json!({"itemId":"plan","delta":"plan"})),
            (
                "item/commandExecution/outputDelta",
                json!({"itemId":"command","delta":"output"}),
            ),
            (
                "item/fileChange/patchUpdated",
                json!({"itemId":"patch","changes":[{"path":"/tmp/file","diff":"+new"}]}),
            ),
        ] {
            let mut params = params;
            params["threadId"] = json!("t");
            params["turnId"] = json!("turn");
            state.reduce(&json!({"method":method,"params":params}));
        }
        state.remember_after(&original, Some(after));
        let snapshot = state.snapshot("t", json!([]));
        let items = &snapshot["thread"]["turns"][0]["items"];
        assert_eq!(items[0]["summary"][1], "summary");
        assert_eq!(items[0]["content"][0], "thinking");
        assert_eq!(items[1]["text"], "plan");
        assert_eq!(items[2]["aggregatedOutput"], "output");
        assert_eq!(items[3]["changes"][0]["diff"], "+new");
        assert_eq!(snapshot["sequence"], 6);
    }
    #[test]
    fn global_notifications_are_private_and_owned_resolution_is_sanitized() {
        let mut state = State::connecting(vec![], "global-test".into());
        state.connection = Connection::Connected;
        state.subscribed.insert("owned".into());
        let (_, events) = state.subscribe();
        for method in [
            "account/updated",
            "account/login/completed",
            "fuzzyFileSearch/sessionUpdated",
            "command/exec/outputDelta",
            "future/global",
            "serverRequest/resolved",
        ] {
            state.reduce(
                &json!({"method":method,"params":{"requestId":99,"secret":"private sentinel"}}),
            );
        }
        assert!(events.try_recv().is_err());
        state
            .pending
            .insert(id_key(&json!(7)), json!({"params":{"threadId":"owned"}}));
        state.reduce(&json!({"method":"serverRequest/resolved","params":{"requestId":7,"secret":"private sentinel"}}));
        let event = events.try_recv().unwrap();
        assert_eq!(event["params"], json!({"requestId":7}));
        assert!(!state.pending.contains_key(&id_key(&json!(7))));
        state.reduce(&json!({"method":"thread/name/updated","params":{"threadId":"owned","threadName":"visible"}}));
        assert_eq!(
            events.try_recv().unwrap()["params"]["threadName"],
            "visible"
        );
    }
}
