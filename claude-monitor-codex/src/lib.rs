//! Authenticated browser actions over one shared native `codex app-server` connection.
//!
//! The Gateway owns only its action-receipt database. The Codex process owns execution,
//! persisted history, project registry and pending requests. Browser writes use the
//! monitor's existing POST/Origin/pairing gate before reaching the native actor.

mod client;
mod history;
mod input;
mod receipts;
pub mod runtime;
mod state;

use anyhow::{bail, Context, Result};
use base64::Engine;
use claude_replay_html::{query_get, HttpResponse, Request};
use client::{Command, Failure};
use receipts::Receipts;
use serde_json::{json, Value};
use state::{can_respond, id_key, Connection, State};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock, TryLockError};
use std::time::{Duration, Instant};

/// Maximum native send body, including encoded image attachments.
pub const SEND_BODY_LIMIT: usize = 12 * 1024 * 1024;

/// Trusted loopback endpoint, canonical project scope and durable receipt location.
pub struct Config {
    pub endpoint: String,
    pub allowed_roots: Vec<PathBuf>,
    pub state_path: PathBuf,
}

/// A clonable process-local owner accessed by HTTP request handlers and the SSE writer.
pub struct Gateway {
    lifecycle: RwLock<GatewayLifecycle>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    endpoint: String,
    allowed_roots: Vec<PathBuf>,
    state: Arc<Mutex<State>>,
    commands: Sender<Command>,
    receipts: Mutex<Receipts>,
}

#[derive(Default, PartialEq)]
enum GatewayLifecycle {
    #[default]
    Active,
    Disconnected,
}

/// A disconnect either preserves active work or reports a worker failure explicitly.
#[derive(Debug, thiserror::Error)]
pub enum DisconnectError {
    #[error("{0}")]
    Busy(String),
    #[error("Disconnect native RPC worker: {0}")]
    Worker(String),
}

#[derive(Debug)]
enum RouteError {
    Invalid(String),
    Conflict(String),
    Runtime(Failure),
    Internal(String),
}

impl From<Failure> for RouteError {
    fn from(error: Failure) -> Self {
        Self::Runtime(error)
    }
}
impl From<anyhow::Error> for RouteError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(format!("{error:#}"))
    }
}
type RouteResult = std::result::Result<Value, RouteError>;

const QUEUE_PAGE_LIMIT: usize = 25;
const QUEUE_MAX_PAGES: usize = 100;
const QUEUE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// A complete native queue and the connection/event fence that produced it.
struct QueueRead {
    entries: Vec<Value>,
    epoch: u64,
    after: u64,
    queue_sequence: Option<u64>,
}

impl QueueRead {
    fn current(&self, state: &State, thread_id: &str) -> Result<bool, RouteError> {
        if state.connection != Connection::Connected || state.epoch != self.epoch {
            return Err(Failure::Unavailable(format!(
                "Native connection changed while reading the queue for thread {thread_id}; refresh its snapshot"
            )).into());
        }
        if state.scope_closed(thread_id)
            || state.archived_threads.contains(thread_id)
            || state.snapshot_invalidated_after(thread_id, self.after)
        {
            return Err(RouteError::Conflict(format!(
                "Execution scope or archive access changed while reading the queue for thread {thread_id}; refresh the project history"
            )));
        }
        Ok(state.queue_sequences.get(thread_id).copied() == self.queue_sequence)
    }
}

fn queue_submission_id(entry: &Value) -> Option<&str> {
    entry["id"]
        .as_str()
        .or_else(|| entry["queuedSubmissionId"].as_str())
        .filter(|id| !id.is_empty() && !id.contains('\0'))
}

fn queue_page_error(thread_id: &str, page: usize, detail: &str) -> RouteError {
    RouteError::Internal(format!(
        "Runtime thread/queue/list for thread {thread_id}, page {page}: {detail}"
    ))
}

// The installed native server reports explicit standard speed as "default" after
// accepting serviceTier: null. Both represent Fast off; other tiers remain exact.
fn service_tier_matches(actual: &Value, requested: &Value) -> bool {
    if requested.is_null() {
        actual.is_null() || actual == "default"
    } else {
        actual == requested
    }
}

fn archived_rejection(failure: &Failure, thread_id: &str) -> bool {
    failure.is_rpc_rejection(-32600, &format!("session {thread_id} is archived. Run `codex unarchive {thread_id}` to unarchive it first."))
}

impl RouteError {
    fn details(&self) -> (&'static str, &str) {
        match self {
            Self::Invalid(message) => ("invalid_input", message),
            Self::Conflict(message) => ("action_conflict", message),
            Self::Runtime(error) => (error.code(), error.message()),
            Self::Internal(message) => ("gateway_error", message),
        }
    }
    fn response(&self) -> Value {
        let (code, message) = self.details();
        json!({"ok":false,"error":{"code":code,"message":message}})
    }
    fn http_code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "400 Bad Request",
            Self::Conflict(_)
            | Self::Runtime(Failure::Rejected(_) | Failure::RpcRejected { .. }) => "409 Conflict",
            Self::Runtime(Failure::Unavailable(_) | Failure::Uncertain(_)) => {
                "503 Service Unavailable"
            }
            Self::Internal(_) => "500 Internal Server Error",
        }
    }
}

impl Gateway {
    /// Read current native connection metadata without issuing RPC or opening history.
    pub fn status(&self) -> Value {
        let mut status = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .status();
        status["allowedRoots"] = json!(self.allowed_roots);
        status["ok"] = json!(true);
        status
    }
    /// Start an independently reconnecting RPC worker without starting another Codex
    /// writer. This returns while the configured server is unreachable; status exposes
    /// the connection and every write is refused until initialization/recovery completes.
    pub fn start(config: Config) -> Result<Arc<Self>> {
        validate_endpoint(&config.endpoint)?;
        if config.allowed_roots.is_empty() {
            bail!("Codex Gateway requires at least one allowed project root");
        }
        let roots = config
            .allowed_roots
            .iter()
            .map(|root| {
                root.canonicalize().with_context(|| {
                    format!("canonicalize allowed project root {}", root.display())
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if roots.iter().any(|root| !root.is_dir()) {
            bail!("Every allowed project root must be a directory");
        }
        let receipts = Receipts::open(&config.state_path).with_context(|| {
            format!(
                "initialize Gateway action receipts at {}",
                config.state_path.display()
            )
        })?;
        // Native server request IDs restart at zero. A new Gateway instance must
        // also invalidate old browser controls even if its epoch restarts at one.
        let mut namespace = [0_u8; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut namespace))
            .context("generate Codex Gateway request namespace from /dev/urandom")?;
        let namespace = namespace.iter().map(|byte| format!("{byte:02x}")).collect();
        let initial_state = State::connecting(roots.clone(), namespace);
        let state = Arc::new(Mutex::new(initial_state));
        let (commands, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let endpoint = config.endpoint.clone();
        let worker = std::thread::Builder::new()
            .name("codex-gateway-rpc".to_owned())
            .spawn(move || client::run(endpoint, worker_state, receiver))
            .context("start Codex native RPC worker")?;
        Ok(Arc::new(Self {
            lifecycle: RwLock::new(GatewayLifecycle::Active),
            worker: Mutex::new(Some(worker)),
            endpoint: config.endpoint,
            allowed_roots: roots,
            state,
            commands,
            receipts: Mutex::new(receipts),
        }))
    }

    /// Match only attached native threads and their exact Monitor rollout aliases.
    pub fn owns_thread(&self, session_id: &str) -> bool {
        if *self.lifecycle.read().unwrap_or_else(|e| e.into_inner()) != GatewayLifecycle::Active {
            return false;
        }
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.subscribed.contains(session_id)
            || state.threads.iter().any(|(id, thread)| {
                id == session_id
                    || format!("native:{id}") == session_id
                    || thread["path"]
                        .as_str()
                        .and_then(|path| Path::new(path).file_stem())
                        .and_then(|stem| stem.to_str())
                        == Some(session_id)
            })
    }

    /// Serialize disconnect against every HTTP action. An idle disconnect closes only
    /// this gateway's WebSocket worker; it never terminates the shared Codex process.
    pub fn disconnect(&self) -> std::result::Result<(), DisconnectError> {
        let mut lifecycle = match self.lifecycle.try_write() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err(DisconnectError::Busy(
                    "A native request is still running; retry after it completes.".to_owned(),
                ))
            }
        };
        if *lifecycle == GatewayLifecycle::Disconnected {
            return Ok(());
        }
        let owned = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.has_active_work() {
                return Err(DisconnectError::Busy("A native turn or approval is active; finish or interrupt it before disconnecting.".to_owned()));
            }
            if state.connection != Connection::Connected && !state.threads.is_empty() {
                return Err(DisconnectError::Busy(
                    "Reconnect and confirm owned threads are idle before disconnecting.".to_owned(),
                ));
            }
            state
                .threads
                .keys()
                .filter(|id| !state.archived_threads.contains(*id))
                .cloned()
                .collect::<Vec<_>>()
        };
        for id in owned {
            let thread = self.ensure_thread(&id).map_err(|error| {
                DisconnectError::Busy(format!(
                    "Cannot confirm thread {id} is idle: {}",
                    error.details().1
                ))
            })?;
            if thread["status"]["type"] == "active" || thread["status"] == "active" {
                return Err(DisconnectError::Busy(format!(
                    "Thread {id} is active; finish or interrupt it before disconnecting."
                )));
            }
            let queue = self.read_queue(&id).map_err(|error| {
                DisconnectError::Busy(format!(
                    "Cannot confirm thread {id} queue is empty: {}",
                    error.details().1
                ))
            })?;
            if !queue.entries.is_empty() {
                return Err(DisconnectError::Busy(format!(
                    "Thread {id} has queued input; finish or cancel it before disconnecting."
                )));
            }
        }
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.has_active_work() {
                return Err(DisconnectError::Busy("A native turn, queued request or approval is still active; finish or interrupt it before disconnecting.".to_owned()));
            }
            if state.connection != Connection::Connected && !state.threads.is_empty() {
                return Err(DisconnectError::Busy("The native connection is recovering; reconnect and confirm that owned threads are idle before disconnecting.".to_owned()));
            }
            *lifecycle = GatewayLifecycle::Disconnected;
            state.deactivate();
        }
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            worker.join().map_err(|_| {
                DisconnectError::Worker("RPC worker panicked while disconnecting".to_owned())
            })?;
        }
        Ok(())
    }

    /// The bounded receiver disconnects when a browser falls behind. Its HTTP writer
    /// must authenticate before subscribing and the browser must snapshot on reconnect.
    pub fn subscribe(&self) -> (Value, Receiver<Value>) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .subscribe()
    }

    /// Dispatch the Codex API prefix. Every POST clears the monitor pairing and Origin
    /// gate before parsing; reads require pairing because they contain private history.
    pub fn route(&self, request: &Request<'_>) -> Option<HttpResponse> {
        let route = request.name.strip_prefix("api/codex/")?;
        let write = matches!(
            route,
            "open"
                | "create"
                | "settings"
                | "send"
                | "interrupt"
                | "respond"
                | "queue/cancel"
                | "queue/steer"
                | "archive"
                | "project/create"
        );
        if write {
            if let Some(deny) = request.deny_write() {
                return Some(deny);
            }
        } else {
            if request.method != "GET" {
                return Some(HttpResponse::method_not_allowed(
                    "Codex read route requires GET",
                ));
            }
            if !request.authenticated {
                return Some(HttpResponse::unauthorized(
                    "Codex workspace requires pairing",
                ));
            }
            if !request.origin_ok {
                return Some(HttpResponse::forbidden("cross-origin request refused"));
            }
        }
        let lifecycle = self.lifecycle.read().unwrap_or_else(|e| e.into_inner());
        if *lifecycle == GatewayLifecycle::Disconnected {
            let mut response = HttpResponse::json(json!({"ok":false,"error":{"code":"native_not_enabled","message":"Native mode was disconnected; enable it again before sending."}}).to_string());
            response.code = "409 Conflict";
            return Some(response);
        }
        let result = if write {
            self.write_route(route, request.body)
        } else {
            self.read_route(route, request.query)
        };
        let mut response = match result {
            Ok(value) => {
                let mut response = HttpResponse::json(value.to_string());
                if value["ok"] == false {
                    response.code = match value["error"]["code"].as_str() {
                        Some("invalid_input") => "400 Bad Request",
                        Some("rejected" | "action_conflict") => "409 Conflict",
                        Some("unavailable" | "uncertain") => "503 Service Unavailable",
                        _ => "500 Internal Server Error",
                    };
                }
                response
            }
            Err(error) => {
                let mut response = HttpResponse::json(error.response().to_string());
                response.code = error.http_code();
                response
            }
        };
        response.headers.push("Cache-Control: no-store".to_owned());
        Some(response)
    }

    fn rpc(&self, method: &str, params: Value) -> RouteResult {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.connection != Connection::Connected {
            let context = state
                .connection_error
                .as_deref()
                .unwrap_or("No native connection has completed initialization");
            return Err(Failure::Unavailable(format!("Shared Codex server is initializing or reconnecting: {context}; refresh its snapshot before retrying")).into());
        }
        let epoch = state.epoch;
        drop(state);
        client::call(&self.commands, method, params, epoch).map_err(Into::into)
    }

    /// Follow every native cursor without publishing a partial or mixed queue.
    /// Queue notifications contain no entries, so any change restarts all pages.
    fn read_queue(&self, thread_id: &str) -> Result<QueueRead, RouteError> {
        valid_identifier(thread_id, "threadId")?;
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut queue = QueueRead {
            entries: Vec::new(),
            epoch: state.epoch,
            after: state.sequence,
            queue_sequence: state.queue_sequences.get(thread_id).copied(),
        };
        queue.current(&state, thread_id)?;
        drop(state);
        'attempt: for _ in 0..4 {
            queue.entries.clear();
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            queue.current(&state, thread_id)?;
            queue.queue_sequence = state.queue_sequences.get(thread_id).copied();
            drop(state);
            let mut params = json!({"threadId":thread_id,"limit":QUEUE_PAGE_LIMIT});
            let mut cursors = HashSet::new();
            let mut submission_ids = HashSet::new();
            let mut bytes = 0_usize;
            for page_number in 1..=QUEUE_MAX_PAGES {
                {
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !queue.current(&state, thread_id)? {
                        continue 'attempt;
                    }
                }
                // Keep one native epoch for the whole traversal, including retries.
                let page = client::call(
                    &self.commands,
                    "thread/queue/list",
                    params.clone(),
                    queue.epoch,
                )
                .map_err(|failure| {
                    let details = format!(
                        "Read native queue for thread {thread_id}, page {page_number}: {}",
                        failure.message()
                    );
                    match failure {
                        Failure::Rejected(_) => Failure::Rejected(details),
                        Failure::RpcRejected { error, .. } => {
                            Failure::RpcRejected { details, error }
                        }
                        Failure::Unavailable(_) => Failure::Unavailable(details),
                        Failure::Uncertain(_) => Failure::Uncertain(details),
                    }
                })?;
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if !queue.current(&state, thread_id)? {
                    continue 'attempt;
                }
                drop(state);
                bytes = bytes.saturating_add(page.to_string().len());
                if bytes > QUEUE_MAX_BYTES {
                    return Err(queue_page_error(
                        thread_id,
                        page_number,
                        "complete queue exceeds the 64 MiB recovery limit",
                    ));
                }
                let entries = page["data"].as_array().ok_or_else(|| {
                    queue_page_error(thread_id, page_number, "data must be an array")
                })?;
                if entries.len() > QUEUE_PAGE_LIMIT {
                    return Err(queue_page_error(
                        thread_id,
                        page_number,
                        "data exceeds the requested page limit",
                    ));
                }
                for entry in entries {
                    let id = queue_submission_id(entry).ok_or_else(|| {
                        queue_page_error(
                            thread_id,
                            page_number,
                            "queue entry has no nonempty native submission ID",
                        )
                    })?;
                    if !entry["clientUserMessageId"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty() && !id.contains('\0'))
                    {
                        return Err(queue_page_error(
                            thread_id,
                            page_number,
                            "queue entry has no nonempty clientUserMessageId",
                        ));
                    }
                    if !entry["input"].as_array().is_some_and(|input| {
                        input.iter().all(|value| {
                            value["type"].as_str().is_some_and(|kind| !kind.is_empty())
                                && (value["type"] != "text" || value["text"].is_string())
                        })
                    }) {
                        return Err(queue_page_error(
                            thread_id,
                            page_number,
                            "queue entry input must contain typed objects with valid text payloads",
                        ));
                    }
                    if !submission_ids.insert(id.to_owned()) {
                        return Err(queue_page_error(
                            thread_id,
                            page_number,
                            "native submission ID repeats across queue pages",
                        ));
                    }
                    queue.entries.push(entry.clone());
                }
                // Older compatible servers omit a terminal null cursor.
                let cursor = match page.get("nextCursor") {
                    None | Some(Value::Null) => return Ok(queue),
                    Some(Value::String(cursor))
                        if !cursor.is_empty() && cursor.len() <= 4096 && !cursor.contains('\0') =>
                    {
                        cursor
                    }
                    _ => {
                        return Err(queue_page_error(
                            thread_id,
                            page_number,
                            "nextCursor must be a nonempty string or null",
                        ))
                    }
                };
                if !cursors.insert(cursor.to_owned()) {
                    return Err(queue_page_error(
                        thread_id,
                        page_number,
                        "native pagination cursor repeated",
                    ));
                }
                params["cursor"] = json!(cursor);
            }
            return Err(queue_page_error(
                thread_id,
                QUEUE_MAX_PAGES,
                "queue pagination exceeded the 100-page recovery limit",
            ));
        }
        Err(Failure::Unavailable(format!(
            "The shared queue for thread {thread_id} kept changing during snapshot recovery; refresh its snapshot"
        )).into())
    }

    fn directory(&self, value: &str) -> std::result::Result<PathBuf, RouteError> {
        canonical_directory(&self.allowed_roots, value)
            .map_err(|error| RouteError::Invalid(format!("{error:#}")))
    }

    fn read_route(&self, route: &str, query: &str) -> RouteResult {
        let decoded = |key| query_get(query, key).map(decode_query);
        let result = match route {
            "status" => {
                let mut status = self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .status();
                status["allowedRoots"] = json!(self.allowed_roots);
                status["ok"] = json!(true);
                return Ok(status);
            }
            "models" => {
                let mut params = json!({"limit":40});
                if let Some(cursor) = decoded("cursor") {
                    params["cursor"] = json!(cursor);
                }
                self.rpc("model/list", params)?
            }
            "projects" => {
                let mut params = json!({"limit":40});
                if let Some(cursor) = decoded("cursor") {
                    params["cursor"] = json!(cursor);
                }
                let mut result = self.rpc("project/list", params)?;
                if let Some(projects) = result["data"].as_array_mut() {
                    projects.retain(|project| {
                        project["roots"].as_array().is_some_and(|roots| {
                            !roots.is_empty()
                                && roots.iter().all(|root| {
                                    root["path"]
                                        .as_str()
                                        .is_some_and(|path| self.directory(path).is_ok())
                                })
                        })
                    });
                }
                result
            }
            "directories" => {
                let path = decoded("path")
                    .unwrap_or_else(|| self.allowed_roots[0].to_string_lossy().into_owned());
                let path = self.directory(&path)?;
                let mut result = self.rpc("fs/readDirectory", json!({"path":path}))?;
                // Symlinks that escape an allowed root are omitted from the picker.
                if let Some(entries) = result["entries"].as_array_mut() {
                    entries.retain_mut(|entry| {
                        let candidate = entry["path"].as_str().map(PathBuf::from).or_else(|| {
                            entry["fileName"]
                                .as_str()
                                .or_else(|| entry["name"].as_str())
                                .map(|name| path.join(name))
                        });
                        if let Some(candidate) = candidate {
                            if let Ok(canonical) = self.directory(&candidate.to_string_lossy()) {
                                entry["path"] = json!(canonical);
                                entry["name"] = entry["fileName"].clone();
                                return true;
                            }
                        }
                        false
                    });
                }
                result["path"] = json!(path);
                result["allowedRoots"] = json!(self.allowed_roots);
                result
            }
            "threads" => {
                let mut params = json!({"limit":40,"archived":decoded("archived").as_deref() == Some("true"),
                    "modelProviders":[],"sourceKinds":["cli","vscode","exec","appServer","subAgent","subAgentReview","subAgentCompact","subAgentThreadSpawn","subAgentOther","unknown"],
                    "sortKey":"updated_at","sortDirection":"desc","useStateDbOnly":false});
                if let Some(cursor) = decoded("cursor") {
                    params["cursor"] = json!(cursor);
                }
                if let Some(project) = decoded("projectId") {
                    params["projectId"] = json!(project);
                }
                let mut result = self.rpc("thread/list", params)?;
                if let Some(threads) = result["data"].as_array_mut() {
                    threads.retain(|thread| {
                        thread["cwd"]
                            .as_str()
                            .is_some_and(|cwd| self.directory(cwd).is_ok())
                    });
                }
                result
            }
            "turns" | "items" => {
                let id = decoded("threadId")
                    .ok_or_else(|| RouteError::Invalid("threadId is required".to_owned()))?;
                self.ensure_thread(&id)?;
                let mut params = json!({"threadId":id,"limit":40,"sortDirection":"asc"});
                if let Some(cursor) = decoded("cursor") {
                    params["cursor"] = json!(cursor);
                }
                self.rpc(
                    if route == "turns" {
                        "thread/turns/list"
                    } else {
                        "thread/items/list"
                    },
                    params,
                )?
            }
            _ => {
                return Err(RouteError::Invalid(format!(
                    "Unknown Codex API route: {route}"
                )))
            }
        };
        Ok(success(result))
    }

    fn ensure_thread(&self, thread_id: &str) -> RouteResult {
        valid_identifier(thread_id, "threadId")?;
        // Desktop can change the shared cwd while this browser is idle. A cached
        // thread's original cwd is therefore never authority for a new browser action.
        let thread = self.rpc(
            "thread/read",
            json!({"threadId":thread_id,"includeTurns":false}),
        )?["thread"]
            .clone();
        let cwd = thread["cwd"].as_str().ok_or_else(|| {
            RouteError::Invalid("Runtime thread has no execution directory".to_owned())
        })?;
        self.directory(cwd)?;
        Ok(thread)
    }

    fn open(&self, thread_id: &str) -> RouteResult {
        let (epoch, sequence) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.epoch, state.sequence)
        };
        self.ensure_thread(thread_id)?;
        match self.rpc("thread/resume", json!({"threadId":thread_id})) {
            // The native worker ingests the resume result before reading the next
            // socket event. HTTP handlers must not overwrite newer event state here.
            Ok(result) => {
                self.directory(
                    result["cwd"]
                        .as_str()
                        .or_else(|| result["thread"]["cwd"].as_str())
                        .ok_or_else(|| {
                            RouteError::Invalid(
                                "Runtime resume returned no execution directory".to_owned(),
                            )
                        })?,
                )?;
            }
            Err(error) => {
                if matches!(&error, RouteError::Runtime(failure) if archived_rejection(failure, thread_id))
                {
                    return self.open_archived(thread_id, epoch, sequence);
                }
                // The creating native client owns an unmaterialized empty draft. Keep
                // that state, but do not disguise any other resume failure as success.
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let empty = state
                    .threads
                    .get(thread_id)
                    .is_some_and(|thread| thread["turns"].as_array().is_some_and(Vec::is_empty));
                let no_rollout = matches!(&error, RouteError::Runtime(failure)
                    if failure.code() == "rejected" && failure.message().contains("no rollout"));
                if !empty || !no_rollout {
                    return Err(error);
                }
            }
        }
        let queue = self.read_queue(thread_id)?;
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !queue.current(&state, thread_id)? {
            return Err(Failure::Unavailable(
                "The shared queue changed before snapshot publication; refresh its snapshot"
                    .to_owned(),
            )
            .into());
        }
        let snapshot = state.snapshot(thread_id, json!(queue.entries));
        if !snapshot["thread"].is_object() {
            return Err(RouteError::Conflict("The shared thread changed execution scope during attachment; refresh the project history".to_owned()));
        }
        Ok(json!({"ok":true,"snapshot":snapshot}))
    }

    /// The installed native server cannot resume archived sessions. Read them through
    /// its own inventory and history APIs without unarchiving or opening a queue.
    fn open_archived(&self, thread_id: &str, epoch: u64, sequence: u64) -> RouteResult {
        let mut params = json!({"limit":100,"archived":true,"modelProviders":[],
            "sourceKinds":["cli","vscode","exec","appServer","subAgent","subAgentReview","subAgentCompact","subAgentThreadSpawn","subAgentOther","unknown"],
            "sortKey":"updated_at","sortDirection":"desc","useStateDbOnly":false});
        let mut cursors = std::collections::HashSet::new();
        let inventory_cwd = loop {
            let inventory = self.rpc("thread/list", params.clone())?;
            let threads = inventory["data"].as_array().ok_or_else(|| {
                RouteError::Internal(
                    "Runtime returned an invalid archived thread inventory".to_owned(),
                )
            })?;
            if let Some(thread) = threads
                .iter()
                .find(|thread| thread["id"].as_str() == Some(thread_id))
            {
                break self.directory(thread["cwd"].as_str().ok_or_else(|| {
                    RouteError::Invalid(
                        "Archived runtime thread has no execution directory".to_owned(),
                    )
                })?)?;
            }
            let Some(cursor) = inventory["nextCursor"]
                .as_str()
                .filter(|cursor| !cursor.is_empty())
            else {
                return Err(RouteError::Conflict("The native archived thread inventory changed during attachment; refresh the project history".to_owned()));
            };
            if !cursors.insert(cursor.to_owned()) {
                return Err(RouteError::Internal(
                    "Runtime repeated an archived thread inventory cursor".to_owned(),
                ));
            }
            params["cursor"] = json!(cursor);
        };
        let history = self.rpc(
            "thread/read",
            json!({"threadId":thread_id,"includeTurns":true}),
        )?;
        let mut thread = history["thread"].clone();
        let cwd = thread["cwd"].as_str().ok_or_else(|| {
            RouteError::Invalid("Archived runtime history has no execution directory".to_owned())
        })?;
        let native_archive_path = thread["path"].as_str().is_some_and(|path| {
            let path = Path::new(path);
            path.is_absolute()
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == "archived_sessions")
        });
        if thread["id"].as_str() != Some(thread_id)
            || self.directory(cwd)? != inventory_cwd
            || !native_archive_path
            || !thread["turns"].is_array()
        {
            return Err(RouteError::Conflict("Native archived history no longer matches the requested thread and execution scope; refresh the project history".to_owned()));
        }
        let mut history_source = Value::Null;
        if thread["turns"].as_array().is_some_and(Vec::is_empty) {
            // A cold paginated rollout can exist before app-server has rebuilt its
            // SQLite item projection. Read only the native-confirmed archived path
            // through that same server; never unarchive, resume or modify its store.
            let path = thread["path"].as_str().expect("validated archived path");
            let file = self.rpc("fs/readFile", json!({"path":path}))?;
            let encoded = file["dataBase64"].as_str().ok_or_else(|| {
                RouteError::Internal(
                    "Native fs/readFile returned no archived rollout bytes".to_owned(),
                )
            })?;
            const MAX_BYTES: usize = 64 * 1024 * 1024;
            if encoded.len() > MAX_BYTES.div_ceil(3) * 4 {
                return Err(RouteError::Conflict("The archived rollout exceeds the 64 MiB read-only recovery limit; use a Codex server with its history index rebuilt".to_owned()));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| {
                    RouteError::Internal(format!("Decode native archived rollout: {error}"))
                })?;
            if bytes.len() > MAX_BYTES {
                return Err(RouteError::Conflict(
                    "The archived rollout exceeds the 64 MiB read-only recovery limit".to_owned(),
                ));
            }
            let recovered = history::project(&bytes, thread_id, cwd).map_err(|error| {
                RouteError::Conflict(format!(
                    "Recover native archived rollout history: {error:#}"
                ))
            })?;
            thread["turns"] = json!(recovered.turns);
            history_source = json!({"kind":"nativeArchivedRollout","historyMode":recovered.history_mode,"bytes":bytes.len()});
        }
        let scope = directory_scope(&self.allowed_roots, thread["cwd"].as_str());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.remember_archived_scoped(thread, epoch, sequence, &scope) {
            return Err(Failure::Unavailable("The native connection or archived thread changed during history attachment; refresh its snapshot".to_owned()).into());
        }
        let mut snapshot = state.snapshot(thread_id, json!([]));
        snapshot["historySource"] = history_source;
        Ok(json!({"ok":true,"snapshot":snapshot}))
    }

    fn ensure_writable(&self, thread_id: &str) -> std::result::Result<(), RouteError> {
        if self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .archived_threads
            .contains(thread_id)
        {
            return Err(RouteError::Conflict("Archived history is read-only; explicitly restore and reopen this thread before sending messages or changing settings".to_owned()));
        }
        Ok(())
    }

    fn model_effort(&self, payload: &Value) -> RouteResult {
        let model = string(payload, "model", 256)?;
        let effort = string(payload, "effort", 32)?;
        let mut params = json!({"limit":100});
        let mut cursors = std::collections::HashSet::new();
        let selected = loop {
            let catalog = self.rpc("model/list", params.clone())?;
            let models = catalog["data"].as_array().ok_or_else(|| {
                RouteError::Internal("Runtime returned an invalid model catalog page".to_owned())
            })?;
            if let Some(selected) = models.iter().find(|entry| {
                entry["model"].as_str() == Some(model) || entry["id"].as_str() == Some(model)
            }) {
                break selected.clone();
            }
            let Some(cursor) = catalog["nextCursor"]
                .as_str()
                .filter(|cursor| !cursor.is_empty())
            else {
                return Err(RouteError::Invalid(format!(
                    "Model {model} is not advertised by this runtime"
                )));
            };
            if !cursors.insert(cursor.to_owned()) {
                return Err(RouteError::Internal(
                    "Runtime repeated a model catalog cursor".to_owned(),
                ));
            }
            params["cursor"] = json!(cursor);
        };
        if !selected["supportedReasoningEfforts"]
            .as_array()
            .is_some_and(|values| {
                values.iter().any(|entry| {
                    entry["reasoningEffort"].as_str() == Some(effort)
                        || entry.as_str() == Some(effort)
                })
            })
        {
            return Err(RouteError::Invalid(format!(
                "Model {model} does not support effort {effort}"
            )));
        }
        let canonical_model = selected["model"].as_str().ok_or_else(|| {
            RouteError::Internal("Advertised runtime model has no canonical model name".to_owned())
        })?;
        let mut result = json!({"model":canonical_model,"effort":effort});
        if let Some(tier) = payload.get("serviceTier") {
            if !tier.is_null() {
                let tier_id = tier.as_str().filter(|id| !id.is_empty()).ok_or_else(|| {
                    RouteError::Invalid(
                        "serviceTier must be null or an advertised tier ID".to_owned(),
                    )
                })?;
                if !selected["serviceTiers"].as_array().is_some_and(|tiers| {
                    tiers
                        .iter()
                        .any(|entry| entry["id"].as_str() == Some(tier_id))
                }) {
                    return Err(RouteError::Invalid(format!(
                        "Model {model} does not support service tier {tier_id}"
                    )));
                }
            }
            result["serviceTier"] = tier.clone();
        }
        Ok(result)
    }

    fn write_route(&self, route: &str, body: &[u8]) -> RouteResult {
        let limit = if route == "send" {
            SEND_BODY_LIMIT
        } else {
            1024 * 1024
        };
        if body.len() > limit {
            return Err(RouteError::Invalid(format!(
                "Codex {route} body exceeds {limit} bytes"
            )));
        }
        let payload: Value = serde_json::from_slice(body)
            .map_err(|error| RouteError::Invalid(format!("Invalid JSON action body: {error}")))?;
        if !payload.is_object() {
            return Err(RouteError::Invalid(
                "Codex action body must be a JSON object".to_owned(),
            ));
        }
        if route == "open" {
            fields(&payload, &["threadId"])?;
            return self.open(string(&payload, "threadId", 256)?);
        }
        let action_id = string(&payload, "actionId", 256)?;
        valid_identifier(action_id, "actionId")?;
        // Claim/check before validation: a retry returns its original response and a
        // changed request conflicts even if the current catalog or thread has changed.
        let claim = self
            .receipts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .claim(action_id, &self.endpoint, route, &payload);
        match claim {
            Ok(Some(response)) => return Ok(response),
            Ok(None) => {}
            Err(error) if error.downcast_ref::<receipts::ActionConflict>().is_some() => {
                return Err(RouteError::Conflict(format!("{error:#}")))
            }
            Err(error) => return Err(error.into()),
        }
        let result = self.execute(route, &payload);
        let (status, mut response) = match result {
            Ok(response) => ("accepted", response),
            Err(error) => {
                let uncertain = matches!(error, RouteError::Runtime(Failure::Uncertain(_)));
                (
                    if uncertain { "uncertain" } else { "rejected" },
                    error.response(),
                )
            }
        };
        response["receipt"] = json!({"actionId":action_id,"status":status});
        // The database lock only covers short durable operations. An action awaiting
        // a native settings event must not block another device's approval/interrupt.
        self.receipts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finish(action_id, status, &response)
            .map_err(|error| {
                Failure::Uncertain(format!(
                    "Action completed, but its durable receipt could not be saved: {error:#}"
                ))
            })?;
        Ok(response)
    }

    fn execute(&self, route: &str, payload: &Value) -> RouteResult {
        match route {
            "create" => {
                fields(
                    payload,
                    &[
                        "cwd",
                        "projectId",
                        "model",
                        "effort",
                        "serviceTier",
                        "actionId",
                    ],
                )?;
                let settings = self.model_effort(payload)?;
                let cwd = self.directory(string(payload, "cwd", 4096)?)?;
                let mut params = json!({"cwd":cwd,"model":settings["model"],"config":{"model_reasoning_effort":settings["effort"]},"historyMode":"paginated"});
                if let Some(tier) = settings.get("serviceTier") {
                    params["serviceTier"] = tier.clone();
                }
                if let Some(project_id) = payload.get("projectId").filter(|value| !value.is_null())
                {
                    let project_id = project_id.as_str().ok_or_else(|| {
                        RouteError::Invalid("projectId must be a string".to_owned())
                    })?;
                    valid_identifier(project_id, "projectId")?;
                    let project = self.rpc("project/read", json!({"projectId":project_id}))?;
                    let roots = project["project"]["roots"]
                        .as_array()
                        .filter(|roots| !roots.is_empty())
                        .ok_or_else(|| {
                            RouteError::Invalid(
                                "Selected project has no allowed execution roots".to_owned(),
                            )
                        })?;
                    for root in roots {
                        self.directory(root["path"].as_str().ok_or_else(|| {
                            RouteError::Invalid(
                                "Selected project root is not an absolute directory path"
                                    .to_owned(),
                            )
                        })?)?;
                    }
                    params["projectId"] = json!(project_id);
                }
                let result = self.rpc("thread/start", params)?;
                let thread_id = result["thread"]["id"]
                    .as_str()
                    .ok_or_else(|| {
                        RouteError::Internal("thread/start returned no thread ID".to_owned())
                    })?
                    .to_owned();
                let snapshot = self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .snapshot(&thread_id, json!([]));
                Ok(json!({"ok":true,"snapshot":snapshot}))
            }
            "project/create" => {
                fields(payload, &["name", "roots", "actionId"])?;
                let name = string(payload, "name", 256)?;
                let roots = payload["roots"]
                    .as_array()
                    .filter(|roots| !roots.is_empty() && roots.len() <= 32)
                    .ok_or_else(|| {
                        RouteError::Invalid(
                            "roots must contain between 1 and 32 directory paths".to_owned(),
                        )
                    })?;
                let roots = roots
                    .iter()
                    .map(|root| {
                        root.as_str()
                            .ok_or_else(|| {
                                RouteError::Invalid("Project root must be a string".to_owned())
                            })
                            .and_then(|root| self.directory(root))
                            .map(|path| json!({"path":path}))
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(success(self.rpc(
                    "project/create",
                    json!({"name":name,"roots":roots,"idempotencyKey":payload["actionId"]}),
                )?))
            }
            "respond" => {
                fields(
                    payload,
                    &[
                        "threadId",
                        "requestId",
                        "requestToken",
                        "response",
                        "actionId",
                    ],
                )?;
                let thread_id = string(payload, "threadId", 256)?;
                let request_token = string(payload, "requestToken", 128)?;
                let id = payload
                    .get("requestId")
                    .filter(|id| id.is_string() || id.is_i64())
                    .ok_or_else(|| {
                        RouteError::Invalid(
                            "requestId must be a native string or integer ID".to_owned(),
                        )
                    })?;
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.connection != Connection::Connected {
                    return Err(Failure::Unavailable(
                        "Cannot answer during runtime reconnect".to_owned(),
                    )
                    .into());
                }
                let pending = state.pending.get(&id_key(id)).ok_or_else(|| {
                    RouteError::Conflict(
                        "This runtime request has already resolved or is stale".to_owned(),
                    )
                })?;
                if pending["requestToken"].as_str() != Some(request_token)
                    || pending["params"]["threadId"].as_str() != Some(thread_id)
                {
                    return Err(RouteError::Conflict(
                        "This runtime request belongs to another task or connection generation"
                            .to_owned(),
                    ));
                }
                let method = pending["method"].as_str().unwrap_or_default();
                if !can_respond(method) {
                    return Err(RouteError::Invalid(
                        "Dynamic tools are answered by their registered executor".to_owned(),
                    ));
                }
                validate_response(method, &payload["response"], &pending["params"])?;
                let epoch = state.epoch;
                drop(state);
                self.ensure_thread(thread_id)?;
                self.ensure_writable(thread_id)?;
                Ok(success(client::respond(
                    &self.commands,
                    id.clone(),
                    payload["response"].clone(),
                    epoch,
                    request_token.to_owned(),
                )?))
            }
            "settings" | "send" | "interrupt" | "queue/cancel" | "queue/steer" | "archive" => {
                let thread_id = string(payload, "threadId", 256)?;
                self.ensure_thread(thread_id)?;
                if route != "archive" {
                    self.ensure_writable(thread_id)?;
                }
                let subscribed = self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .subscribed
                    .contains(thread_id);
                if !subscribed && matches!(route, "settings" | "send" | "interrupt" | "queue/steer")
                {
                    self.open(thread_id)?;
                }
                if route != "archive" {
                    self.ensure_writable(thread_id)?;
                }
                match route {
                    "settings" => {
                        fields(
                            payload,
                            &["threadId", "model", "effort", "serviceTier", "actionId"],
                        )?;
                        let settings = self.model_effort(payload)?;
                        let (epoch, sequence) = {
                            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            (state.epoch, state.sequence)
                        };
                        let mut params = json!({"threadId":thread_id,"model":settings["model"],"effort":settings["effort"]});
                        if let Some(tier) = settings.get("serviceTier") {
                            params["serviceTier"] = tier.clone();
                        }
                        self.rpc("thread/settings/update", params)?;
                        // The empty RPC response only acknowledges a queued core op.
                        // Wait for its effective settings event before returning a
                        // snapshot or allowing the UI to display the requested value.
                        self.wait_settings(thread_id, &settings, epoch, sequence)?;
                        self.open(thread_id).map_err(|error| RouteError::Runtime(Failure::Uncertain(format!(
                            "Settings were accepted, but their effective snapshot could not be refreshed: {}", error.details().1))))
                    }
                    "send" => {
                        fields(
                            payload,
                            &[
                                "threadId",
                                "text",
                                "images",
                                "intent",
                                "expectedTurnId",
                                "actionId",
                            ],
                        )?;
                        let input = input::message(payload)?;
                        let result = match string(payload, "intent", 16)? {
                            "next" => self.enqueue_next(thread_id, &payload["actionId"], input)?,
                            "steer" => self.rpc("turn/steer", json!({"threadId":thread_id,"expectedTurnId":identifier(payload,"expectedTurnId")?,"input":input}))?,
                            _ => return Err(RouteError::Invalid("intent must be next or steer".to_owned())),
                        };
                        Ok(success(result))
                    }
                    "interrupt" => {
                        fields(payload, &["threadId", "turnId", "actionId"])?;
                        Ok(success(self.rpc(
                            "turn/interrupt",
                            json!({"threadId":thread_id,"turnId":identifier(payload,"turnId")?}),
                        )?))
                    }
                    "queue/steer" => self.steer_queued(thread_id, payload),
                    "queue/cancel" => {
                        fields(payload, &["threadId", "clientUserMessageId", "actionId"])?;
                        let client_id = identifier(payload, "clientUserMessageId")?;
                        let queue = self.read_queue(thread_id)?;
                        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if !queue.current(&state, thread_id)? {
                            return Err(Failure::Unavailable("The shared queue changed before cancellation; refresh its snapshot".to_owned()).into());
                        }
                        let mut matching = queue.entries.iter().filter(|entry| {
                            entry["clientUserMessageId"].as_str() == Some(client_id)
                        });
                        let queued = matching.next().ok_or_else(|| {
                            RouteError::Conflict(
                                "Queued message already started, resolved or was cancelled"
                                    .to_owned(),
                            )
                        })?;
                        if matching.next().is_some() {
                            return Err(RouteError::Conflict("More than one native queued submission has this clientUserMessageId; cancel by an unambiguous native task".to_owned()));
                        }
                        let submission_id = queue_submission_id(queued).ok_or_else(|| {
                            RouteError::Internal(
                                "Validated native queue entry lost its submission ID".to_owned(),
                            )
                        })?;
                        drop(state);
                        Ok(success(client::call(
                            &self.commands,
                            "thread/queue/delete",
                            json!({"threadId":thread_id,"queuedSubmissionId":submission_id}),
                            queue.epoch,
                        )?))
                    }
                    "archive" => {
                        fields(payload, &["threadId", "archived", "actionId"])?;
                        let archived = payload["archived"].as_bool().ok_or_else(|| {
                            RouteError::Invalid("archived must be a boolean".to_owned())
                        })?;
                        Ok(success(self.rpc(
                            if archived {
                                "thread/archive"
                            } else {
                                "thread/unarchive"
                            },
                            json!({"threadId":thread_id}),
                        )?))
                    }
                    _ => unreachable!(),
                }
            }
            _ => Err(RouteError::Invalid(format!(
                "Unknown Codex action: {route}"
            ))),
        }
    }

    /// Steer the server's original queued input, then remove its queued copy, matching
    /// Codex desktop's two native operations. This protocol has no atomic queue promotion.
    /// A durable submission binding prevents redelivery across actions, devices and crashes.
    fn steer_queued(&self, thread_id: &str, payload: &Value) -> RouteResult {
        fields(
            payload,
            &[
                "threadId",
                "clientUserMessageId",
                "expectedTurnId",
                "actionId",
            ],
        )?;
        let client_id = identifier(payload, "clientUserMessageId")?;
        let turn_id = identifier(payload, "expectedTurnId")?;
        let action_id = identifier(payload, "actionId")?;
        let queue = self.read_queue(thread_id)?;
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !queue.current(&state, thread_id)? {
            return Err(Failure::Unavailable(
                "The shared queue changed before steering; refresh its snapshot".to_owned(),
            )
            .into());
        }
        let mut matching = queue
            .entries
            .iter()
            .filter(|entry| entry["clientUserMessageId"].as_str() == Some(client_id));
        let queued = matching.next().ok_or_else(|| {
            RouteError::Conflict(
                "Queued message already started, resolved or was cancelled".to_owned(),
            )
        })?;
        if matching.next().is_some() {
            return Err(RouteError::Conflict("More than one native queued submission has this clientUserMessageId; refresh the queue before steering".to_owned()));
        }
        let submission_id = queue_submission_id(queued).ok_or_else(|| {
            RouteError::Internal("Validated native queue entry lost its submission ID".to_owned())
        })?;
        let input = queued["input"].clone();
        drop(state);
        if !self
            .receipts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .claim_queue_steer(&self.endpoint, thread_id, submission_id, action_id)?
        {
            return Err(RouteError::Conflict("This queued message was already steered or its delivery is unconfirmed. Check the conversation and cancel the remaining queued copy; do not steer it again.".to_owned()));
        }
        let result = match client::call(
            &self.commands,
            "turn/steer",
            json!({
                "threadId":thread_id,"clientUserMessageId":client_id,
                "expectedTurnId":turn_id,"input":input
            }),
            queue.epoch,
        ) {
            Ok(result) => result,
            Err(error) => {
                if !matches!(error, Failure::Uncertain(_)) {
                    self.receipts.lock().unwrap_or_else(|e| e.into_inner())
                        .release_queue_steer(&self.endpoint, thread_id, submission_id, action_id)
                        .map_err(|release| Failure::Uncertain(format!(
                            "Queued steering was rejected: {}; its retry binding could not be released: {release:#}", error.message())))?;
                }
                return Err(error.into());
            }
        };
        let removed = client::call(&self.commands, "thread/queue/delete",
            json!({"threadId":thread_id,"queuedSubmissionId":submission_id}), queue.epoch)
            .map_err(|error| Failure::Uncertain(format!(
                "Steer was accepted for turn {turn_id}, but removal of its queued copy is unconfirmed: {}. Check the conversation and cancel the remaining queued copy; do not steer it again.", error.message())))?;
        if removed["deleted"] != true {
            return Err(Failure::Uncertain(format!(
                "Steer was accepted for turn {turn_id}, but its queued copy was not deleted. Refresh the conversation and cancel any remaining copy; do not steer it again.")).into());
        }
        Ok(success(result))
    }

    /// Native queue additions wake ordinary idle threads, but deliberately preserve
    /// an interrupted thread's pause. An explicit Send starts its own queued message
    /// after interruption; another client's active turn remains a separate queue.
    fn enqueue_next(&self, thread_id: &str, action_id: &Value, input: Value) -> RouteResult {
        let mut queued = self.rpc(
            "thread/queue/add",
            json!({"threadId":thread_id,"clientUserMessageId":action_id,"input":input}),
        )?;
        let interrupted = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.threads.get(thread_id).is_some_and(|thread| {
                thread["turns"].as_array().is_some_and(|turns| {
                    !turns.iter().any(|turn| turn["status"] == "inProgress")
                        && turns
                            .last()
                            .is_some_and(|turn| turn["status"] == "interrupted")
                })
            })
        };
        if !interrupted {
            return Ok(queued);
        }
        let submission_id = queued["queuedSubmission"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Failure::Uncertain(
                "Message was queued, but the runtime returned no submission ID to start; keep this action ID when recovering".to_owned(),
            ))?;
        match self.rpc(
            "thread/queue/start",
            json!({"threadId":thread_id,"queuedSubmissionId":submission_id}),
        ) {
            Ok(started)
                if started["turn"]["id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty()) =>
            {
                queued["turn"] = started["turn"].clone();
                Ok(queued)
            }
            Err(RouteError::Runtime(error))
                if error
                    .is_rpc_rejection(-32600, "thread already has an active or pending turn") =>
            {
                // Native start-if-idle refuses without consuming the queued item.
                // This client race is still an accepted independent queued message.
                Ok(queued)
            }
            result => {
                let cause = match result {
                    Ok(_) => "Runtime start response omitted its turn ID".to_owned(),
                    Err(error) => error.details().1.to_owned(),
                };
                // queue/add already changed native state. A new action ID could
                // duplicate that message, even when the explicit start was rejected.
                Err(Failure::Uncertain(format!(
                    "Message is already queued as {submission_id}, but starting it could not be confirmed: {cause}. Keep this action ID and inspect the shared task before sending again"
                )).into())
            }
        }
    }

    fn wait_settings(
        &self,
        thread_id: &str,
        desired: &Value,
        epoch: u64,
        after: u64,
    ) -> RouteResult {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.epoch != epoch || state.connection != Connection::Connected {
                return Err(Failure::Uncertain("Settings were accepted, but the runtime disconnected before effective settings were confirmed".to_owned()).into());
            }
            if state.scope_closed(thread_id) {
                return Err(Failure::Uncertain("Settings were accepted, but the shared thread moved outside the Gateway's configured project roots".to_owned()).into());
            }
            if state
                .settings_sequence
                .get(thread_id)
                .is_some_and(|sequence| *sequence > after)
                && state.settings.get(thread_id).is_some_and(|settings| {
                    settings["model"] == desired["model"]
                        && settings["effort"] == desired["effort"]
                        && desired
                            .get("serviceTier")
                            .is_none_or(|tier| service_tier_matches(&settings["serviceTier"], tier))
                })
            {
                return Ok(json!({}));
            }
            drop(state);
            if Instant::now() >= deadline {
                return Err(Failure::Uncertain("Settings were accepted, but matching effective settings were not confirmed; another client may have changed them".to_owned()).into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

fn success(mut result: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("ok".to_owned(), json!(true));
        result
    } else {
        json!({"ok":true,"result":result})
    }
}

fn string<'a>(
    payload: &'a Value,
    key: &str,
    limit: usize,
) -> std::result::Result<&'a str, RouteError> {
    payload[key]
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= limit && !value.contains('\0'))
        .ok_or_else(|| {
            RouteError::Invalid(format!(
                "{key} must be a nonempty string of at most {limit} bytes"
            ))
        })
}

fn valid_identifier(value: &str, field: &str) -> std::result::Result<(), RouteError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(RouteError::Invalid(format!(
            "{field} contains unsupported characters"
        )));
    }
    Ok(())
}

fn identifier<'a>(payload: &'a Value, key: &str) -> std::result::Result<&'a str, RouteError> {
    let value = string(payload, key, 256)?;
    valid_identifier(value, key)?;
    Ok(value)
}

fn fields(payload: &Value, allowed: &[&str]) -> std::result::Result<(), RouteError> {
    let object = payload
        .as_object()
        .ok_or_else(|| RouteError::Invalid("Expected an object".to_owned()))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(RouteError::Invalid(format!("Unknown action field: {key}")));
    }
    Ok(())
}

/// Resolve the live filesystem once at an I/O boundary and preserve refusal causes.
fn directory_scope(roots: &[PathBuf], cwd: Option<&str>) -> state::DirectoryScope {
    if roots.is_empty() {
        return state::DirectoryScope::Unrestricted;
    }
    let Some(cwd) = cwd else {
        return state::DirectoryScope::Rejected {
            cause: "Native execution directory is missing".to_owned(),
        };
    };
    match canonical_directory(roots, cwd) {
        Ok(_) => state::DirectoryScope::Allowed {
            source: cwd.to_owned(),
        },
        Err(error) => state::DirectoryScope::Rejected {
            cause: format!("Validate native execution directory {cwd}: {error:#}"),
        },
    }
}

/// Select the directory an event observes without accessing files or changing state.
fn notification_directory(state: &state::State, message: &Value) -> Option<String> {
    let params = &message["params"];
    if message["method"] == "thread/settings/updated" {
        return params["threadSettings"]["cwd"].as_str().map(str::to_owned);
    }
    let id = params["threadId"]
        .as_str()
        .or_else(|| params["thread"]["id"].as_str())?;
    state.threads.get(id)?["cwd"].as_str().map(str::to_owned)
}

fn canonical_directory(roots: &[PathBuf], value: &str) -> Result<PathBuf> {
    let input = Path::new(value);
    if !input.is_absolute() {
        bail!("Execution directory must be an absolute host path");
    }
    let path = input
        .canonicalize()
        .with_context(|| format!("Resolve execution directory {value}"))?;
    if !path.is_dir() {
        bail!("Execution path is not a directory: {}", path.display());
    }
    if !roots.iter().any(|root| path.starts_with(root)) {
        bail!(
            "Execution directory is outside the configured project roots: {}",
            path.display()
        );
    }
    Ok(path)
}

fn validate_endpoint(endpoint: &str) -> Result<()> {
    let uri: tungstenite::http::Uri = endpoint
        .parse()
        .context("Parse shared Codex WebSocket endpoint")?;
    if uri.scheme_str() != Some("ws")
        || !matches!(
            uri.host(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        )
    {
        bail!("Shared Codex WebSocket endpoint must use ws:// on loopback");
    }
    if uri
        .path_and_query()
        .is_some_and(|path| path.as_str().contains('?'))
    {
        bail!("Codex endpoint cannot include query credentials");
    }
    Ok(())
}

fn validate_response(
    method: &str,
    response: &Value,
    params: &Value,
) -> std::result::Result<(), RouteError> {
    if !response.is_object() {
        return Err(RouteError::Invalid(
            "response must be a JSON object".to_owned(),
        ));
    }
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            fields(response, &["decision"])?;
            if !matches!(
                response["decision"].as_str(),
                Some("accept" | "acceptForSession" | "decline" | "cancel")
            ) {
                return Err(RouteError::Invalid(
                    "Approval decision must be accept, acceptForSession, decline or cancel"
                        .to_owned(),
                ));
            }
        }
        "item/permissions/requestApproval" => {
            fields(response, &["permissions", "scope"])?;
            if !response["permissions"].is_object()
                || !matches!(response["scope"].as_str(), Some("turn" | "session"))
            {
                return Err(RouteError::Invalid(
                    "Permission reply requires a permissions object and turn/session scope"
                        .to_owned(),
                ));
            }
            // A browser can grant the requested permissions or deny all. Arbitrary
            // permission expansion is refused at this API boundary.
            if response["permissions"] != json!({})
                && response["permissions"] != params["permissions"]
            {
                return Err(RouteError::Invalid(
                    "Permission response must equal the requested permissions or deny all"
                        .to_owned(),
                ));
            }
        }
        "item/tool/requestUserInput" => {
            fields(response, &["answers"])?;
            let answers = response["answers"].as_object().ok_or_else(|| {
                RouteError::Invalid("Question reply requires an answers object".to_owned())
            })?;
            let questions = params["questions"].as_array().ok_or_else(|| {
                RouteError::Invalid("Runtime question has no question list".to_owned())
            })?;
            for (id, answer) in answers {
                fields(answer, &["answers"])?;
                if !questions
                    .iter()
                    .any(|question| question["id"].as_str() == Some(id))
                    || !answer["answers"].as_array().is_some_and(|values| {
                        !values.is_empty()
                            && values.iter().all(|value| {
                                value.as_str().is_some_and(|value| value.len() <= 16 * 1024)
                            })
                    })
                {
                    return Err(RouteError::Invalid(
                        "Question reply contains an unknown question or invalid answer list"
                            .to_owned(),
                    ));
                }
            }
            if questions.iter().any(|question| {
                question["id"]
                    .as_str()
                    .is_some_and(|id| !answers.contains_key(id))
            }) {
                return Err(RouteError::Invalid(
                    "Question reply must answer every pending question".to_owned(),
                ));
            }
        }
        _ => {
            return Err(RouteError::Invalid(
                "Unsupported pending request method".to_owned(),
            ))
        }
    }
    Ok(())
}

#[cfg(test)]
fn remove_test_scratch(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!(
            "Remove Gateway test scratch directory {}: {error}",
            path.display()
        ),
    }
}

/// Decode a native API query value without depending on Monitor's session index.
fn decode_query(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (
                (bytes[at + 1] as char).to_digit(16),
                (bytes[at + 2] as char).to_digit(16),
            ) {
                decoded.push((high * 16 + low) as u8);
                at += 3;
                continue;
            }
        }
        decoded.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::scope_fixture::ScopeFixture;

    fn command_fixture(root: &Path) -> (Arc<Gateway>, Receiver<Command>) {
        std::fs::create_dir_all(root).unwrap();
        let mut state = State::connecting(vec![root.canonicalize().unwrap()], "fixture".to_owned());
        state.connection = Connection::Connected;
        state.epoch = 1;
        state.remember(&json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root,"model":"one","reasoningEffort":"low"}));
        let (commands, receiver) = mpsc::channel();
        (
            Arc::new(Gateway {
                lifecycle: RwLock::new(GatewayLifecycle::Active),
                worker: Mutex::new(None),
                endpoint: "ws://127.0.0.1:4500".to_owned(),
                allowed_roots: state.allowed_roots.clone(),
                state: Arc::new(Mutex::new(state)),
                commands,
                receipts: Mutex::new(Receipts::open(&root.join("receipts.sqlite3")).unwrap()),
            }),
            receiver,
        )
    }

    fn answer_call(receiver: &Receiver<Command>, expected: &str, result: Value) {
        match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
            Command::Call { method, answer, .. } => {
                assert_eq!(method, expected);
                answer.send(Ok(result)).unwrap();
            }
            _ => panic!("expected native call {expected}"),
        }
    }

    #[test]
    fn disconnected_status_events_and_http_reads_keep_the_native_failure_cause() {
        let root = std::env::temp_dir().join(format!("gateway-cause-{}", std::process::id()));
        let (gateway, _) = command_fixture(&root);
        let cause = "Initialize Codex RPC at ws://127.0.0.1:4500: Invalid Codex RPC JSON";
        gateway.state.lock().unwrap().reconnect(cause.to_owned());
        assert_eq!(gateway.status()["connectionError"], cause);
        let response = gateway
            .route(&Request {
                method: "GET",
                name: "api/codex/models",
                query: "",
                body: b"",
                authenticated: true,
                origin_ok: true,
            })
            .unwrap();
        assert_eq!(response.code, "503 Service Unavailable");
        let result: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(result["error"]["code"], "unavailable");
        assert!(result["error"]["message"].as_str().unwrap().contains(cause));
        gateway
            .state
            .lock()
            .unwrap()
            .connection(Connection::Connected);
        assert!(gateway.status()["connectionError"].is_null());
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn responses_reject_reused_native_ids_across_requests_connections_and_gateway_instances() {
        let root = std::env::temp_dir().join(format!(
            "gateway-response-generation-{}",
            std::process::id()
        ));
        let (gateway, receiver) = command_fixture(&root);
        let request = json!({"id":7,"method":"item/commandExecution/requestApproval",
            "params":{"threadId":"t","command":"old command","availableDecisions":["accept","decline"]}});
        let old_token = {
            let mut state = gateway.state.lock().unwrap();
            state.reduce(&request);
            state.pending["7"]["requestToken"].clone()
        };
        for case in ["replacement", "connection", "gateway"] {
            {
                let mut state = gateway.state.lock().unwrap();
                match case {
                    "connection" => state.connection(Connection::Connected),
                    "gateway" => state.request_namespace = "replacement-gateway".to_owned(),
                    _ => {}
                }
                let mut replacement = request.clone();
                replacement["params"]["command"] = json!("new command");
                state.reduce(&replacement);
            }
            let payload = json!({"threadId":"t","requestId":7,"requestToken":old_token,
                "response":{"decision":"accept"},"actionId":format!("stale-{case}")});
            let error = gateway
                .write_route("respond", &serde_json::to_vec(&payload).unwrap())
                .unwrap();
            assert_eq!(
                error["error"]["code"], "action_conflict",
                "Reject stale {case} at HTTP boundary"
            );
            assert!(
                receiver.try_recv().is_err(),
                "Stale approval must cause no native calls"
            );
        }
        let token = gateway.state.lock().unwrap().pending["7"]["requestToken"].clone();
        for (index, (id, thread_id)) in [(json!("7"), "t"), (json!(7), "other")]
            .into_iter()
            .enumerate()
        {
            let payload = json!({"threadId":thread_id,"requestId":id,"requestToken":token,
                "response":{"decision":"accept"},"actionId":format!("wrong-request-{index}")});
            assert_eq!(
                gateway
                    .write_route("respond", &serde_json::to_vec(&payload).unwrap())
                    .unwrap()["error"]["code"],
                "action_conflict"
            );
            assert!(receiver.try_recv().is_err());
        }
        let payload = json!({"threadId":"t","requestId":7,"requestToken":token,
            "response":{"decision":"accept"},"actionId":"current-generation"});
        let target = Arc::clone(&gateway);
        let response = std::thread::spawn(move || {
            target.write_route("respond", &serde_json::to_vec(&payload).unwrap())
        });
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root}}),
        );
        match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
            Command::Respond {
                id,
                result,
                epoch,
                request_token,
                answer,
            } => {
                assert_eq!(id, json!(7));
                assert_eq!(result, json!({"decision":"accept"}));
                assert_eq!(epoch, 2);
                assert_eq!(json!(request_token), token);
                answer.send(Ok(json!({"delivered":true}))).unwrap();
            }
            _ => panic!("Current request must reach its native response executor"),
        }
        assert_eq!(response.join().unwrap().unwrap()["delivered"], true);
        drop(gateway);
        remove_test_scratch(&root);
    }

    fn queue_call(
        receiver: &Receiver<Command>,
        method: &str,
    ) -> (Value, Sender<std::result::Result<Value, Failure>>) {
        match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
            Command::Call {
                method: actual,
                params,
                answer,
                ..
            } => {
                assert_eq!(actual, method);
                (params, answer)
            }
            _ => panic!("expected native queue call {method}"),
        }
    }

    fn archived_error() -> Failure {
        Failure::rpc_rejection(
            "thread/resume",
            json!({"code":-32600,
            "message":"session t is archived. Run `codex unarchive t` to unarchive it first."}),
        )
    }

    fn archived_history(root: &Path) -> Value {
        json!({"thread":{"id":"t","cwd":root,"path":"/native/archived_sessions/rollout-t.jsonl",
            "turns":[{"id":"completed","status":"completed","items":[{"id":"answer","type":"agentMessage","text":"Archived native history"}]}]}})
    }

    fn reject_resume(receiver: &Receiver<Command>) {
        let (_, answer) = queue_call(receiver, "thread/resume");
        answer.send(Err(archived_error())).unwrap();
    }

    #[test]
    fn cold_archive_uses_native_file_bytes_and_preserves_attachment_fences() {
        for case in [
            "success",
            "bad-base64",
            "wrong-meta",
            "epoch",
            "restore",
            "scope",
        ] {
            let root = std::env::temp_dir().join(format!(
                "gateway-cold-archive-{case}-{}",
                std::process::id()
            ));
            let (gateway, receiver) = command_fixture(&root);
            let target = Arc::clone(&gateway);
            let open = std::thread::spawn(move || target.open("t"));
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            reject_resume(&receiver);
            answer_call(
                &receiver,
                "thread/list",
                json!({"data":[{"id":"t","cwd":root}],"nextCursor":null}),
            );
            let mut empty = archived_history(&root);
            empty["thread"]["turns"] = json!([]);
            answer_call(&receiver, "thread/read", empty);
            let (params, answer) = queue_call(&receiver, "fs/readFile");
            assert_eq!(
                params,
                json!({"path":"/native/archived_sessions/rollout-t.jsonl"}),
                "Only the verified native archived path may be read"
            );
            let mut records = [
                json!({"type":"session_meta","payload":{"id":if case == "wrong-meta" { "other" } else { "t" },"cwd":root,"history_mode":"paginated"}}),
                json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"original-turn","started_at":1}}),
                json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"t","turn_id":"original-turn","item":{"id":"original-item","type":"AgentMessage","content":[{"type":"Text","text":"Cold original output"}],"phase":"final_answer"}}}),
                json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"original-turn","completed_at":2}}),
            ];
            for (ordinal, record) in records.iter_mut().enumerate() {
                record["ordinal"] = json!(ordinal);
            }
            let bytes = records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            let encoded = if case == "bad-base64" {
                "invalid!".to_owned()
            } else {
                base64::engine::general_purpose::STANDARD.encode(bytes.as_bytes())
            };
            {
                let mut state = gateway.state.lock().unwrap();
                match case {
                    "epoch" => state.connection(Connection::Connected),
                    "restore" => state.reduce(&json!({"method":"thread/unarchived","params":{"threadId":"t"}})),
                    "scope" => state.reduce(&json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"cwd":"/"}}})),
                    _ => {}
                }
            }
            answer.send(Ok(json!({"dataBase64":encoded}))).unwrap();
            let result = open.join().unwrap();
            if case == "success" {
                let snapshot = result.unwrap()["snapshot"].clone();
                assert_eq!(snapshot["accessMode"], "archived");
                assert_eq!(
                    snapshot["historySource"],
                    json!({"kind":"nativeArchivedRollout","historyMode":"paginated","bytes":bytes.len()})
                );
                assert_eq!(snapshot["thread"]["turns"][0]["id"], "original-turn");
                assert_eq!(
                    snapshot["thread"]["turns"][0]["items"][0]["id"],
                    "original-item"
                );
                assert_eq!(
                    snapshot["thread"]["turns"][0]["items"][0]["text"],
                    "Cold original output"
                );
                assert_eq!(snapshot["pending"], json!([]));
                assert_eq!(snapshot["queue"], json!([]));
                assert!(gateway.state.lock().unwrap().archived_threads.contains("t"));
            } else {
                assert!(result.is_err(), "Cold history must reject {case}");
            }
            assert!(
                receiver.try_recv().is_err(),
                "Read-only recovery must not unarchive, resume again, or open a queue"
            );
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn archived_open_reads_native_paginated_history_without_resuming_or_opening_a_queue() {
        let root =
            std::env::temp_dir().join(format!("gateway-archived-open-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        {
            let mut state = gateway.state.lock().unwrap();
            state.threads.clear();
            state.settings.clear();
            state.subscribed.clear();
        }
        gateway.state.lock().unwrap().pending.insert(
            "old-approval".to_owned(),
            json!({"params":{"threadId":"t"}}),
        );
        let target = Arc::clone(&gateway);
        let open = std::thread::spawn(move || target.open("t"));
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root}}),
        );
        reject_resume(&receiver);
        let (params, answer) = queue_call(&receiver, "thread/list");
        assert_eq!(params["archived"], true);
        assert_eq!(params["useStateDbOnly"], false);
        answer
            .send(Ok(
                json!({"data":[{"id":"other","cwd":"/"}],"nextCursor":"archived-page-two"}),
            ))
            .unwrap();
        let (params, answer) = queue_call(&receiver, "thread/list");
        assert_eq!(params["cursor"], "archived-page-two");
        answer
            .send(Ok(
                json!({"data":[{"id":"t","cwd":root}],"nextCursor":null}),
            ))
            .unwrap();
        let (params, answer) = queue_call(&receiver, "thread/read");
        assert_eq!(params, json!({"threadId":"t","includeTurns":true}));
        answer.send(Ok(archived_history(&root))).unwrap();
        let result = open.join().unwrap().unwrap();
        assert_eq!(result["snapshot"]["accessMode"], "archived");
        assert_eq!(
            result["snapshot"]["thread"]["turns"][0]["items"][0]["text"],
            "Archived native history"
        );
        assert_eq!(result["snapshot"]["settings"], json!({}));
        assert_eq!(result["snapshot"]["pending"], json!([]));
        assert_eq!(result["snapshot"]["queue"], json!([]));
        assert_eq!(result["snapshot"]["epoch"], 1);
        assert!(gateway.state.lock().unwrap().pending.is_empty());
        assert!(
            receiver.try_recv().is_err(),
            "archived attachment must not queue/list or unarchive"
        );

        let target = Arc::clone(&gateway);
        let send = std::thread::spawn(move || {
            target.write_route("send", br#"{"threadId":"t","text":"do not execute","intent":"next","actionId":"archived-send"}"#)
        });
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root}}),
        );
        let refused = send.join().unwrap().unwrap();
        assert_eq!(refused["receipt"]["status"], "rejected");
        assert!(refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("read-only"));
        assert!(
            receiver.try_recv().is_err(),
            "read-only history must not deliver queue/add"
        );
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn archived_compatibility_requires_the_exact_typed_native_rejection() {
        let message = "session t is archived. Run `codex unarchive t` to unarchive it first.";
        let failures = [
            Failure::rpc_rejection("thread/resume", json!({"code":-32000,"message":message})),
            Failure::rpc_rejection(
                "thread/resume",
                json!({"code":-32600,"message":format!("{message} extra")}),
            ),
            Failure::rpc_rejection(
                "thread/resume",
                json!({"code":-32600,"message":"session other is archived. Run `codex unarchive other` to unarchive it first."}),
            ),
            Failure::Rejected(message.to_owned()),
            Failure::Unavailable(message.to_owned()),
            Failure::Uncertain(message.to_owned()),
        ];
        for (index, failure) in failures.into_iter().enumerate() {
            let root = std::env::temp_dir().join(format!(
                "gateway-archive-rejection-{index}-{}",
                std::process::id()
            ));
            let (gateway, receiver) = command_fixture(&root);
            let target = Arc::clone(&gateway);
            let open = std::thread::spawn(move || target.open("t"));
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            let (_, answer) = queue_call(&receiver, "thread/resume");
            let expected = failure.code();
            answer.send(Err(failure)).unwrap();
            let error = open.join().unwrap().unwrap_err();
            assert_eq!(error.details().0, expected);
            assert!(
                receiver.try_recv().is_err(),
                "unrelated rejection must not read archived history"
            );
            assert!(!gateway.state.lock().unwrap().archived_threads.contains("t"));
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn archived_history_rejects_identity_path_scope_and_connection_races() {
        for case in [
            "missing-inventory",
            "foreign-id",
            "foreign-cwd",
            "active-path",
            "missing-turns",
            "epoch",
            "restore-event",
            "cold-restore-event",
            "scope-event",
        ] {
            let root = std::env::temp_dir().join(format!(
                "gateway-archive-fence-{case}-{}",
                std::process::id()
            ));
            let (gateway, receiver) = command_fixture(&root);
            if case == "cold-restore-event" {
                let mut state = gateway.state.lock().unwrap();
                state.threads.clear();
                state.settings.clear();
                state.subscribed.clear();
            }
            let target = Arc::clone(&gateway);
            let open = std::thread::spawn(move || target.open("t"));
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            reject_resume(&receiver);
            answer_call(
                &receiver,
                "thread/list",
                if case == "missing-inventory" {
                    json!({"data":[],"nextCursor":null})
                } else {
                    json!({"data":[{"id":"t","cwd":root}]})
                },
            );
            if case != "missing-inventory" {
                let (_, answer) = queue_call(&receiver, "thread/read");
                let mut history = archived_history(&root);
                match case {
                    "foreign-id" => history["thread"]["id"] = json!("other"),
                    "foreign-cwd" => history["thread"]["cwd"] = json!("/"),
                    "active-path" => history["thread"]["path"] = json!("/native/sessions/rollout-t.jsonl"),
                    "missing-turns" => history["thread"]["turns"] = Value::Null,
                    "epoch" => gateway.state.lock().unwrap().connection(Connection::Connected),
                    "restore-event" | "cold-restore-event" => gateway.state.lock().unwrap().reduce(&json!({"method":"thread/unarchived","params":{"threadId":"t"}})),
                    "scope-event" => gateway.state.lock().unwrap().reduce(&json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"cwd":"/"}}})),
                    _ => unreachable!(),
                }
                answer.send(Ok(history)).unwrap();
            }
            assert!(
                open.join().unwrap().is_err(),
                "archive attachment must reject {case}"
            );
            assert!(receiver.try_recv().is_err());
            assert!(!gateway.state.lock().unwrap().archived_threads.contains("t"));
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn images_reach_queue_and_steer_unchanged_and_accepted_retry_never_replays() {
        for intent in ["next", "steer"] {
            let root =
                std::env::temp_dir().join(format!("gateway-image-{intent}-{}", std::process::id()));
            let (gateway, receiver) = command_fixture(&root);
            gateway
                .state
                .lock()
                .unwrap()
                .subscribed
                .insert("t".to_owned());
            let url = "data:image/png;base64,iVBORw0KGgo=";
            let payload = json!({"threadId":"t","text":"","images":[url],"intent":intent,"expectedTurnId":"active","actionId":"image-action"});
            let body = payload.to_string().into_bytes();
            let retry = body.clone();
            let target = Arc::clone(&gateway);
            let sending = std::thread::spawn(move || target.write_route("send", &body));
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            let (params, answer) = queue_call(
                &receiver,
                if intent == "next" {
                    "thread/queue/add"
                } else {
                    "turn/steer"
                },
            );
            assert_eq!(params["input"], json!([{"type":"image","url":url}]));
            if intent == "steer" {
                assert_eq!(params["expectedTurnId"], "active");
            }
            answer
                .send(Ok(json!({"queuedSubmission":{"id":"submission"}})))
                .unwrap();
            let accepted = sending.join().unwrap().unwrap();
            assert_eq!(accepted["receipt"]["status"], "accepted");
            assert_eq!(gateway.write_route("send", &retry).unwrap(), accepted);
            assert!(receiver.try_recv().is_err());
            let mut changed = payload;
            changed["images"] = json!(["data:image/gif;base64,R0lGODlh"]);
            assert!(matches!(
                gateway.write_route("send", changed.to_string().as_bytes()),
                Err(RouteError::Conflict(_))
            ));
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn explicit_send_starts_only_an_interrupted_idle_queue_without_replaying_the_action() {
        for status in ["completed", "inProgress", "interrupted"] {
            let root =
                std::env::temp_dir().join(format!("gateway-send-{status}-{}", std::process::id()));
            let (gateway, receiver) = command_fixture(&root);
            {
                let mut state = gateway.state.lock().unwrap();
                state.subscribed.insert("t".to_owned());
                state.threads.get_mut("t").unwrap()["turns"] =
                    json!([{"id":"previous","status":status,"items":[]}]);
            }
            let payload =
                br#"{"threadId":"t","text":"new task","intent":"next","actionId":"next-message"}"#;
            let target = Arc::clone(&gateway);
            let send = std::thread::spawn(move || target.write_route("send", payload));
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            let (params, answer) = queue_call(&receiver, "thread/queue/add");
            assert_eq!(
                params,
                json!({"threadId":"t","clientUserMessageId":"next-message","input":[{"type":"text","text":"new task","textElements":[]}]})
            );
            answer.send(Ok(json!({"queuedSubmission":{"id":"submission","clientUserMessageId":"next-message"}}))).unwrap();
            if status == "interrupted" {
                let (params, answer) = queue_call(&receiver, "thread/queue/start");
                assert_eq!(
                    params,
                    json!({"threadId":"t","queuedSubmissionId":"submission"})
                );
                answer
                    .send(Ok(
                        json!({"turn":{"id":"distinct-next-turn","status":"inProgress"}}),
                    ))
                    .unwrap();
            }
            let response = send.join().unwrap().unwrap();
            assert_eq!(response["receipt"]["status"], "accepted");
            if status == "interrupted" {
                assert_eq!(response["turn"]["id"], "distinct-next-turn");
            }
            assert_eq!(gateway.write_route("send", payload).unwrap(), response);
            assert!(
                receiver.try_recv().is_err(),
                "accepted receipt must not replay queue/add or queue/start"
            );
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn queued_message_start_races_and_failures_preserve_native_acceptance_and_retry_identity() {
        let busy = "thread already has an active or pending turn";
        for (label, failure, accepted) in [
            (
                "busy",
                Failure::rpc_rejection("thread/queue/start", json!({"code":-32600,"message":busy})),
                true,
            ),
            (
                "wrong-code",
                Failure::rpc_rejection("thread/queue/start", json!({"code":-32000,"message":busy})),
                false,
            ),
            (
                "consumed",
                Failure::rpc_rejection(
                    "thread/queue/start",
                    json!({"code":-32600,"message":"queued submission not found: submission"}),
                ),
                false,
            ),
            (
                "provider",
                Failure::rpc_rejection(
                    "thread/queue/start",
                    json!({"code":-32600,"message":"model provider unavailable"}),
                ),
                false,
            ),
            (
                "disconnected",
                Failure::Unavailable("native connection changed".to_owned()),
                false,
            ),
        ] {
            let root = std::env::temp_dir()
                .join(format!("gateway-queue-race-{label}-{}", std::process::id()));
            let (gateway, receiver) = command_fixture(&root);
            {
                let mut state = gateway.state.lock().unwrap();
                state.subscribed.insert("t".to_owned());
                state.threads.get_mut("t").unwrap()["turns"] =
                    json!([{"id":"stopped","status":"interrupted","items":[]}]);
            }
            let payload = br#"{"threadId":"t","text":"new task","intent":"next","actionId":"stable-message"}"#;
            let target = Arc::clone(&gateway);
            let send = std::thread::spawn(move || target.write_route("send", payload));
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            answer_call(
                &receiver,
                "thread/queue/add",
                json!({"queuedSubmission":{"id":"submission","clientUserMessageId":"stable-message"}}),
            );
            let (_, answer) = queue_call(&receiver, "thread/queue/start");
            let cause = failure.message().to_owned();
            answer.send(Err(failure)).unwrap();
            let response = send.join().unwrap().unwrap();
            assert_eq!(
                response["receipt"]["status"],
                if accepted { "accepted" } else { "uncertain" }
            );
            if !accepted {
                assert_eq!(response["error"]["code"], "uncertain");
                assert!(response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(&cause));
                assert!(response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("already queued as submission"));
            }
            assert_eq!(gateway.write_route("send", payload).unwrap(), response);
            assert!(
                receiver.try_recv().is_err(),
                "partial delivery must never enqueue a duplicate on retry"
            );
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn paths_resolve_symlinks_and_refuse_sibling_prefixes() {
        let root = std::env::temp_dir().join(format!("gateway-paths-{}", std::process::id()));
        let inside = root.join("allowed");
        let outside = root.join("allowed-other");
        remove_test_scratch(&root);
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, inside.join("escape")).unwrap();
        let roots = vec![inside.canonicalize().unwrap()];
        assert!(canonical_directory(&roots, &inside.to_string_lossy()).is_ok());
        assert!(canonical_directory(&roots, &outside.to_string_lossy()).is_err());
        #[cfg(unix)]
        assert!(canonical_directory(&roots, &inside.join("escape").to_string_lossy()).is_err());
        assert!(canonical_directory(&roots, "relative").is_err());
        remove_test_scratch(&root);
    }

    #[test]
    fn runtime_endpoint_cannot_escape_loopback_or_carry_credentials() {
        assert!(validate_endpoint("ws://127.0.0.1:4500").is_ok());
        assert!(validate_endpoint("ws://localhost:4500").is_ok());
        assert!(validate_endpoint("ws://remote.example:4500").is_err());
        assert!(validate_endpoint("ws://127.0.0.1:4500?token=secret").is_err());
        assert!(validate_endpoint("http://127.0.0.1:4500").is_err());
    }

    #[test]
    fn receipt_parent_file_failure_identifies_the_operation_path_and_io_cause() {
        let root =
            std::env::temp_dir().join(format!("gateway-receipt-parent-{}", std::process::id()));
        remove_test_scratch(&root);
        std::fs::create_dir_all(&root).unwrap();
        let parent = root.join("blocked-parent");
        std::fs::write(&parent, b"existing file").unwrap();
        let state_path = parent.join("receipts.sqlite3");
        let error = match Gateway::start(Config {
            endpoint: "ws://127.0.0.1:1".to_owned(),
            allowed_roots: vec![root.clone()],
            state_path: state_path.clone(),
        }) {
            Ok(_) => panic!("a file cannot be the receipt database's parent directory"),
            Err(error) => error,
        };
        let context = error.to_string();
        assert!(context.contains("initialize Gateway action receipts"));
        assert!(context.contains(state_path.to_str().unwrap()));
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|cause| cause.kind() == std::io::ErrorKind::AlreadyExists)
        }));
        assert_eq!(std::fs::read(&parent).unwrap(), b"existing file");
        remove_test_scratch(&root);
    }

    #[test]
    fn malformed_receipt_database_identifies_the_operation_path_and_sqlite_cause() {
        let root =
            std::env::temp_dir().join(format!("gateway-receipt-invalid-{}", std::process::id()));
        remove_test_scratch(&root);
        std::fs::create_dir_all(&root).unwrap();
        let state_path = root.join("receipts.sqlite3");
        std::fs::write(&state_path, b"not a SQLite database").unwrap();
        let error = match Gateway::start(Config {
            endpoint: "ws://127.0.0.1:1".to_owned(),
            allowed_roots: vec![root.clone()],
            state_path: state_path.clone(),
        }) {
            Ok(_) => panic!("an invalid SQLite file cannot store action receipts"),
            Err(error) => error,
        };
        let context = error.to_string();
        assert!(context.contains("initialize Gateway action receipts"));
        assert!(context.contains(state_path.to_str().unwrap()));
        assert!(error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(cause, _))
                    if cause.code == rusqlite::ErrorCode::NotADatabase
            )
        }));
        assert_eq!(
            std::fs::read(&state_path).unwrap(),
            b"not a SQLite database"
        );
        remove_test_scratch(&root);
    }

    #[test]
    fn routes_enforce_pairing_post_and_origin_before_parsing_actions() {
        let root = std::env::temp_dir().join(format!("gateway-auth-{}", std::process::id()));
        remove_test_scratch(&root);
        std::fs::create_dir_all(&root).unwrap();
        let gateway = Gateway::start(Config {
            endpoint: "ws://127.0.0.1:1".to_owned(),
            allowed_roots: vec![root.clone()],
            state_path: root.join("gateway.sqlite3"),
        })
        .unwrap();
        let request = |method, name, authenticated, origin_ok| Request {
            method,
            name,
            query: "",
            body: b"invalid JSON",
            authenticated,
            origin_ok,
        };
        assert_eq!(
            gateway
                .route(&request("GET", "api/codex/create", true, true))
                .unwrap()
                .code,
            "405 Method Not Allowed"
        );
        assert_eq!(
            gateway
                .route(&request("POST", "api/codex/create", false, true))
                .unwrap()
                .code,
            "401 Unauthorized"
        );
        assert_eq!(
            gateway
                .route(&request("POST", "api/codex/create", true, false))
                .unwrap()
                .code,
            "403 Forbidden"
        );
        assert_eq!(
            gateway
                .route(&request("GET", "api/codex/threads", false, true))
                .unwrap()
                .code,
            "401 Unauthorized"
        );
        assert_eq!(
            gateway
                .route(&request("POST", "api/codex/create", true, true))
                .unwrap()
                .code,
            "400 Bad Request"
        );
        for (method, authenticated, origin_ok, expected) in [
            ("GET", true, true, "405 Method Not Allowed"),
            ("POST", false, true, "401 Unauthorized"),
            ("POST", true, false, "403 Forbidden"),
            ("POST", true, true, "400 Bad Request"),
        ] {
            assert_eq!(
                gateway
                    .route(&request(
                        method,
                        "api/codex/queue/steer",
                        authenticated,
                        origin_ok
                    ))
                    .unwrap()
                    .code,
                expected
            );
        }
        let post = |payload: &[u8]| {
            gateway
                .route(&Request {
                    method: "POST",
                    name: "api/codex/create",
                    query: "",
                    body: payload,
                    authenticated: true,
                    origin_ok: true,
                })
                .unwrap()
        };
        let rejected = post(br#"{"actionId":"unique","unexpected":true}"#);
        assert_eq!(rejected.code, "400 Bad Request");
        assert_eq!(
            serde_json::from_slice::<Value>(&rejected.body).unwrap()["receipt"]["status"],
            "rejected"
        );
        assert_eq!(
            post(br#"{"actionId":"unique","unexpected":true}"#).body,
            rejected.body
        );
        assert_eq!(
            post(br#"{"actionId":"unique","unexpected":false}"#).code,
            "409 Conflict"
        );
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn browser_cannot_expand_permissions_or_answer_unrequested_questions() {
        let params = json!({"permissions":{"fileSystem":{"write":["/one"]}}});
        assert!(validate_response(
            "item/permissions/requestApproval",
            &json!({"permissions":{},"scope":"turn"}),
            &params
        )
        .is_ok());
        assert!(validate_response(
            "item/permissions/requestApproval",
            &json!({"permissions":{"fileSystem":{"write":["/two"]}},"scope":"session"}),
            &params
        )
        .is_err());
        let questions = json!({"questions":[{"id":"first"},{"id":"second"}]});
        assert!(validate_response(
            "item/tool/requestUserInput",
            &json!({"answers":{"first":{"answers":["a"]}}}),
            &questions
        )
        .is_err());
        assert!(validate_response(
            "item/tool/requestUserInput",
            &json!({"answers":{"first":{"answers":["a"]},"second":{"answers":["b"]}}}),
            &questions
        )
        .is_ok());
    }

    #[test]
    fn settings_wait_for_effective_event_without_blocking_other_action_receipts() {
        let root = std::env::temp_dir().join(format!("gateway-settings-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        let target = Arc::clone(&gateway);
        let settings = std::thread::spawn(move || {
            target.write_route(
                "settings",
                br#"{"threadId":"t","model":"one","effort":"high","actionId":"change-settings"}"#,
            )
        });
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root,"turns":[]}}),
        );
        answer_call(
            &receiver,
            "model/list",
            json!({"data":[{"id":"one","model":"one","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}]}),
        );
        answer_call(&receiver, "thread/settings/update", json!({}));
        assert!(
            gateway.receipts.try_lock().is_ok(),
            "awaiting settings cannot hold the receipt database lock"
        );
        let other = gateway
            .write_route("send", br#"{"actionId":"other-device","unexpected":true}"#)
            .unwrap();
        assert_eq!(other["receipt"]["status"], "rejected");
        assert!(
            matches!(
                receiver.recv_timeout(Duration::from_millis(80)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "an empty update ACK must not be rendered as applied settings"
        );
        gateway.state.lock().unwrap().reduce(&json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"model":"one","effort":"high","cwd":root}}}));
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root,"turns":[]}}),
        );
        let fresh = json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root,"model":"one","reasoningEffort":"high"});
        // The command fixture substitutes the native worker's ordered ingestion.
        gateway.state.lock().unwrap().remember(&fresh);
        answer_call(&receiver, "thread/resume", fresh);
        answer_call(&receiver, "thread/queue/list", json!({"data":[]}));
        let result = settings.join().unwrap().unwrap();
        assert_eq!(result["snapshot"]["settings"]["effort"], "high");
        assert_eq!(result["receipt"]["status"], "accepted");
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn desktop_changed_cwd_cannot_use_cached_allowed_thread_to_send() {
        let root = std::env::temp_dir().join(format!("gateway-fresh-cwd-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        let target = Arc::clone(&gateway);
        let send = std::thread::spawn(move || {
            target.write_route(
                "send",
                br#"{"threadId":"t","text":"execute","intent":"next","actionId":"outside-cwd"}"#,
            )
        });
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":"/","turns":[]}}),
        );
        let response = send.join().unwrap().unwrap();
        assert_eq!(response["error"]["code"], "invalid_input");
        assert_eq!(response["receipt"]["status"], "rejected");
        assert!(
            receiver.try_recv().is_err(),
            "outside cwd must not deliver queue/add"
        );
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn snapshot_reloads_queue_when_native_change_races_its_read() {
        let root =
            std::env::temp_dir().join(format!("gateway-queue-snapshot-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        let target = Arc::clone(&gateway);
        let snapshot = std::thread::spawn(move || target.open("t"));
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root,"turns":[]}}),
        );
        answer_call(
            &receiver,
            "thread/resume",
            json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root,"model":"one","reasoningEffort":"low"}),
        );
        match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
            Command::Call { method, answer, .. } => {
                assert_eq!(method, "thread/queue/list");
                gateway
                    .state
                    .lock()
                    .unwrap()
                    .reduce(&json!({"method":"thread/queue/changed","params":{"threadId":"t"}}));
                answer.send(Ok(json!({"data":[]}))).unwrap();
            }
            _ => panic!("expected queue snapshot read"),
        }
        answer_call(
            &receiver,
            "thread/queue/list",
            json!({"data":[{"id":"queued","clientUserMessageId":"message","input":[]}]}),
        );
        let result = snapshot.join().unwrap().unwrap();
        assert_eq!(result["snapshot"]["queue"][0]["id"], "queued");
        assert_eq!(result["snapshot"]["sequence"], 1);
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn later_model_catalog_page_validates_effort_and_uses_canonical_model_name() {
        let root = std::env::temp_dir().join(format!("gateway-model-pages-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        let target = Arc::clone(&gateway);
        let selection = std::thread::spawn(move || {
            target.model_effort(&json!({"model":"second-id","effort":"high"}))
        });
        answer_call(
            &receiver,
            "model/list",
            json!({"data":[{"id":"first","model":"first","supportedReasoningEfforts":[]}],"nextCursor":"catalog-page-2"}),
        );
        match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
            Command::Call {
                method,
                params,
                answer,
                ..
            } => {
                assert_eq!(method, "model/list");
                assert_eq!(params["cursor"], "catalog-page-2");
                answer.send(Ok(json!({"data":[{"id":"second-id","model":"second","supportedReasoningEfforts":[{"reasoningEffort":"high"}]}],"nextCursor":null}))).unwrap();
            }
            _ => panic!("expected the second model catalog page"),
        }
        assert_eq!(
            selection.join().unwrap().unwrap(),
            json!({"model":"second","effort":"high"})
        );
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn model_service_tier_validation_preserves_explicit_off_and_omission() {
        for (index, tier) in [
            None,
            Some(Value::Null),
            Some(json!("priority")),
            Some(json!("fake")),
            Some(json!(7)),
        ]
        .into_iter()
        .enumerate()
        {
            let root =
                std::env::temp_dir().join(format!("gateway-tiers-{}-{index}", std::process::id()));
            let (gateway, receiver) = command_fixture(&root);
            let mut input = json!({"model":"visible-id","effort":"high"});
            if let Some(tier) = tier.clone() {
                input["serviceTier"] = tier;
            }
            let target = Arc::clone(&gateway);
            let selection = std::thread::spawn(move || target.model_effort(&input));
            answer_call(
                &receiver,
                "model/list",
                json!({"data":[{"id":"visible-id","model":"canonical","supportedReasoningEfforts":[{"reasoningEffort":"high"}],"serviceTiers":[{"id":"priority","name":"Fast"}]}]}),
            );
            let result = selection.join().unwrap();
            if tier
                .as_ref()
                .is_some_and(|tier| !tier.is_null() && tier != "priority")
            {
                assert!(result.is_err());
            } else {
                let result = result.unwrap();
                assert_eq!(result["model"], "canonical");
                assert_eq!(result.get("serviceTier"), tier.as_ref());
            }
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn fast_off_accepts_native_standard_default_without_accepting_another_tier() {
        assert!(service_tier_matches(&json!("default"), &Value::Null));
        assert!(service_tier_matches(&Value::Null, &Value::Null));
        assert!(!service_tier_matches(&json!("priority"), &Value::Null));
        assert!(!service_tier_matches(&json!("flex"), &Value::Null));
        assert!(service_tier_matches(&json!("priority"), &json!("priority")));
        assert!(!service_tier_matches(&json!("default"), &json!("priority")));
    }

    fn queue_entries(start: usize, end: usize) -> Vec<Value> {
        (start..end)
            .map(|index| {
                json!({"id":format!("native-{index}"),
            "clientUserMessageId":format!("client-{index}"),
            "input":[{"type":"text","text":format!("Queued task {index}")},
                {"type":"futureAttachment","nativePayload":{"index":index}}]})
            })
            .collect()
    }

    fn answer_queue_page(receiver: &Receiver<Command>, cursor: Option<&str>, result: Value) {
        match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
            Command::Call {
                method,
                params,
                epoch,
                answer,
                ..
            } => {
                assert_eq!(method, "thread/queue/list");
                assert_eq!(params["threadId"], "t");
                assert_eq!(params["limit"], 25);
                assert_eq!(params.get("cursor").and_then(Value::as_str), cursor);
                assert_eq!(
                    epoch, 1,
                    "Every page belongs to the original native connection"
                );
                answer.send(Ok(result)).unwrap();
            }
            _ => panic!("Expected a native queue page"),
        }
    }

    #[test]
    fn snapshot_collects_every_native_queue_page_in_original_order() {
        let root = std::env::temp_dir().join(format!("gateway-queue-pages-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        let target = Arc::clone(&gateway);
        let open = std::thread::spawn(move || target.open("t"));
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root}}),
        );
        answer_call(
            &receiver,
            "thread/resume",
            json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root}),
        );
        for page in 0..4 {
            let start = page * 25;
            let cursor = (page > 0).then(|| start.to_string());
            let next = (page < 3).then(|| (start + 25).to_string());
            answer_queue_page(
                &receiver,
                cursor.as_deref(),
                json!({"data":queue_entries(start, start + 25),"nextCursor":next}),
            );
        }
        let result = open.join().unwrap().unwrap();
        assert_eq!(result["snapshot"]["queue"], json!(queue_entries(0, 100)));
        assert_eq!(result["snapshot"]["epoch"], 1);
        assert!(receiver.try_recv().is_err());
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn cancellation_finds_page_two_and_preserves_accepted_or_uncertain_receipts() {
        for uncertain in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "gateway-tail-cancel-{uncertain}-{}",
                std::process::id()
            ));
            let (gateway, receiver) = command_fixture(&root);
            let body =
                br#"{"threadId":"t","clientUserMessageId":"client-31","actionId":"tail-cancel"}"#;
            let target = Arc::clone(&gateway);
            let cancel = std::thread::spawn(move || target.write_route("queue/cancel", body));
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            answer_queue_page(
                &receiver,
                None,
                json!({"data":queue_entries(0, 25),"nextCursor":"25"}),
            );
            answer_queue_page(
                &receiver,
                Some("25"),
                json!({"data":queue_entries(25, 32),"nextCursor":null}),
            );
            match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
                Command::Call {
                    method,
                    params,
                    epoch,
                    answer,
                    ..
                } => {
                    assert_eq!(method, "thread/queue/delete");
                    assert_eq!(
                        params,
                        json!({"threadId":"t","queuedSubmissionId":"native-31"})
                    );
                    assert_eq!(epoch, 1);
                    answer
                        .send(if uncertain {
                            Err(Failure::Uncertain(
                                "Native socket closed after deletion delivery".to_owned(),
                            ))
                        } else {
                            Ok(json!({"deleted":true}))
                        })
                        .unwrap();
                }
                _ => panic!("Tail cancellation must delete the exact native submission"),
            }
            let result = cancel.join().unwrap().unwrap();
            assert_eq!(
                result["receipt"]["status"],
                if uncertain { "uncertain" } else { "accepted" }
            );
            if uncertain {
                assert_eq!(result["error"]["code"], "uncertain");
            } else {
                assert_eq!(result["deleted"], true);
            }
            assert_eq!(
                gateway.write_route("queue/cancel", body).unwrap(),
                result,
                "Receipt replay must not redeliver an accepted or uncertain deletion"
            );
            assert!(receiver.try_recv().is_err());
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn queued_steering_preserves_native_input_and_never_redelivers_after_cleanup_failure() {
        for cleanup in ["accepted", "rejected", "uncertain", "not-deleted"] {
            let root = std::env::temp_dir().join(format!(
                "gateway-queue-steer-{cleanup}-{}",
                std::process::id()
            ));
            let (gateway, receiver) = command_fixture(&root);
            gateway
                .state
                .lock()
                .unwrap()
                .subscribed
                .insert("t".to_owned());
            let body = br#"{"threadId":"t","clientUserMessageId":"client-31","expectedTurnId":"active","actionId":"steer-one"}"#;
            let mut tail = queue_entries(25, 32);
            tail[6]["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"image","url":"data:image/png;base64,exact-original"}));
            let input = tail[6]["input"].clone();
            let target = Arc::clone(&gateway);
            let steer = std::thread::spawn(move || target.write_route("queue/steer", body));
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            answer_queue_page(
                &receiver,
                None,
                json!({"data":queue_entries(0,25),"nextCursor":"25"}),
            );
            answer_queue_page(
                &receiver,
                Some("25"),
                json!({"data":tail,"nextCursor":null}),
            );
            match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
                Command::Call {
                    method,
                    params,
                    epoch,
                    answer,
                    ..
                } => {
                    assert_eq!(method, "turn/steer");
                    assert_eq!(
                        params,
                        json!({"threadId":"t","clientUserMessageId":"client-31","expectedTurnId":"active","input":input})
                    );
                    assert_eq!(epoch, 1);
                    answer.send(Ok(json!({"turnId":"active"}))).unwrap();
                }
                _ => panic!("Expected steering with the exact original queued input"),
            }
            match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
                Command::Call {
                    method,
                    params,
                    epoch,
                    answer,
                    ..
                } => {
                    assert_eq!(method, "thread/queue/delete");
                    assert_eq!(
                        params,
                        json!({"threadId":"t","queuedSubmissionId":"native-31"})
                    );
                    assert_eq!(epoch, 1);
                    answer
                        .send(match cleanup {
                            "accepted" => Ok(json!({"deleted":true})),
                            "not-deleted" => Ok(json!({"deleted":false})),
                            "rejected" => {
                                Err(Failure::Rejected("queue delete rejected".to_owned()))
                            }
                            _ => Err(Failure::Uncertain("delete response lost".to_owned())),
                        })
                        .unwrap();
                }
                _ => panic!("Expected deletion only after accepted steering"),
            }
            let result = steer.join().unwrap().unwrap();
            assert_eq!(
                result["receipt"]["status"],
                if cleanup == "accepted" {
                    "accepted"
                } else {
                    "uncertain"
                }
            );
            if cleanup != "accepted" {
                assert!(result["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("Steer was accepted"));
            }
            assert_eq!(gateway.write_route("queue/steer", body).unwrap(), result);
            assert!(
                receiver.try_recv().is_err(),
                "Same action receipt must never redeliver steering"
            );
            drop(gateway);
            let (gateway, receiver) = command_fixture(&root);
            gateway
                .state
                .lock()
                .unwrap()
                .subscribed
                .insert("t".to_owned());
            let target = Arc::clone(&gateway);
            let retry = std::thread::spawn(move || {
                target.write_route("queue/steer", br#"{"threadId":"t","clientUserMessageId":"client-31","expectedTurnId":"successor","actionId":"another-device"}"#)
            });
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            answer_queue_page(
                &receiver,
                None,
                json!({"data":queue_entries(31,32),"nextCursor":null}),
            );
            assert_eq!(
                retry.join().unwrap().unwrap()["error"]["code"],
                "action_conflict"
            );
            assert!(
                receiver.try_recv().is_err(),
                "Another action/device or server restart cannot re-steer the remaining copy"
            );
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn queued_steering_rejection_preserves_queue_and_permits_a_fresh_action() {
        for uncertain in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "gateway-queue-steer-reject-{uncertain}-{}",
                std::process::id()
            ));
            let (gateway, receiver) = command_fixture(&root);
            gateway
                .state
                .lock()
                .unwrap()
                .subscribed
                .insert("t".to_owned());
            for attempt in 0..2 {
                let body = json!({"threadId":"t","clientUserMessageId":"client-0","expectedTurnId":"active","actionId":format!("attempt-{attempt}")}).to_string();
                let target = Arc::clone(&gateway);
                let steer =
                    std::thread::spawn(move || target.write_route("queue/steer", body.as_bytes()));
                answer_call(
                    &receiver,
                    "thread/read",
                    json!({"thread":{"id":"t","cwd":root}}),
                );
                answer_queue_page(
                    &receiver,
                    None,
                    json!({"data":queue_entries(0,1),"nextCursor":null}),
                );
                if attempt == 0 {
                    match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
                        Command::Call { method, answer, .. } => {
                            assert_eq!(method, "turn/steer");
                            answer.send(Err(if uncertain { Failure::Uncertain("steer delivery unknown".to_owned()) } else {
                                Failure::rpc_rejection("turn/steer", json!({"code":-32600,"message":"expected turn is no longer active"}))
                            })).unwrap();
                        }
                        _ => panic!("Expected steering rejection"),
                    }
                } else if !uncertain {
                    answer_call(&receiver, "turn/steer", json!({"turnId":"active"}));
                    answer_call(&receiver, "thread/queue/delete", json!({"deleted":true}));
                }
                let result = steer.join().unwrap().unwrap();
                assert_eq!(
                    result["receipt"]["status"],
                    if attempt == 0 && uncertain {
                        "uncertain"
                    } else if attempt == 0 || uncertain {
                        "rejected"
                    } else {
                        "accepted"
                    }
                );
                assert!(
                    receiver.try_recv().is_err(),
                    "Rejected or unconfirmed steering must never delete the queued input"
                );
            }
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn queued_steering_refuses_resolved_ambiguous_and_reconnected_queue() {
        for case in ["missing", "ambiguous", "reconnected"] {
            let root = std::env::temp_dir().join(format!(
                "gateway-queue-steer-invalid-{case}-{}",
                std::process::id()
            ));
            let (gateway, receiver) = command_fixture(&root);
            gateway
                .state
                .lock()
                .unwrap()
                .subscribed
                .insert("t".to_owned());
            let target = Arc::clone(&gateway);
            let steer = std::thread::spawn(move || {
                target.write_route("queue/steer", br#"{"threadId":"t","clientUserMessageId":"client-0","expectedTurnId":"active","actionId":"invalid-steer"}"#)
            });
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            let mut entries = queue_entries(0, if case == "missing" { 0 } else { 2 });
            if case == "ambiguous" {
                entries[1]["clientUserMessageId"] = json!("client-0");
            }
            if case == "reconnected" {
                match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
                    Command::Call {
                        method,
                        epoch,
                        answer,
                        ..
                    } => {
                        assert_eq!(method, "thread/queue/list");
                        assert_eq!(epoch, 1);
                        gateway.state.lock().unwrap().epoch = 2;
                        answer
                            .send(Ok(json!({"data":entries,"nextCursor":null})))
                            .unwrap();
                    }
                    _ => panic!("Expected original connection queue read"),
                }
            } else {
                answer_queue_page(&receiver, None, json!({"data":entries,"nextCursor":null}));
            }
            assert_eq!(
                steer.join().unwrap().unwrap()["error"]["code"],
                if case == "reconnected" {
                    "unavailable"
                } else {
                    "action_conflict"
                }
            );
            assert!(
                receiver.try_recv().is_err(),
                "Invalid native queue cannot steer or delete"
            );
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn malformed_queue_pages_never_publish_partial_data_or_delete() {
        for case in [
            "data",
            "id",
            "client",
            "input",
            "text",
            "cursor",
            "empty-cursor",
            "repeat-cursor",
            "repeat-id",
            "page-limit",
            "ambiguous-client",
        ] {
            let root = std::env::temp_dir()
                .join(format!("gateway-bad-queue-{case}-{}", std::process::id()));
            let (gateway, receiver) = command_fixture(&root);
            let target = Arc::clone(&gateway);
            let cancel = std::thread::spawn(move || {
                target.write_route("queue/cancel",
                br#"{"threadId":"t","clientUserMessageId":"client-31","actionId":"malformed-cancel"}"#)
            });
            answer_call(
                &receiver,
                "thread/read",
                json!({"thread":{"id":"t","cwd":root}}),
            );
            answer_queue_page(
                &receiver,
                None,
                json!({"data":queue_entries(0, 25),"nextCursor":"25"}),
            );
            let mut tail = json!({"data":queue_entries(25, 32),"nextCursor":null});
            match case {
                "data" => tail["data"] = json!({}),
                "id" => tail["data"][0]["id"] = json!(""),
                "client" => tail["data"][0]["clientUserMessageId"] = Value::Null,
                "input" => tail["data"][0]["input"] = json!({}),
                "text" => tail["data"][0]["input"][0]["text"] = json!({"unsafe":"shape"}),
                "cursor" => tail["nextCursor"] = json!(25),
                "empty-cursor" => tail["nextCursor"] = json!(""),
                "repeat-cursor" => tail["nextCursor"] = json!("25"),
                "repeat-id" => tail["data"][0]["id"] = json!("native-0"),
                "page-limit" => tail["data"] = json!(queue_entries(25, 51)),
                "ambiguous-client" => tail["data"][0]["clientUserMessageId"] = json!("client-31"),
                _ => unreachable!(),
            }
            answer_queue_page(&receiver, Some("25"), tail);
            let result = cancel.join().unwrap().unwrap();
            assert_eq!(
                result["receipt"]["status"], "rejected",
                "Reject {case} before mutation"
            );
            assert_eq!(
                result["error"]["code"],
                if case == "ambiguous-client" {
                    "action_conflict"
                } else {
                    "gateway_error"
                }
            );
            if case != "ambiguous-client" {
                assert!(result["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("thread t, page 2"));
            }
            assert!(
                receiver.try_recv().is_err(),
                "Malformed {case} must cause no deletion"
            );
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn queue_change_between_pages_restarts_the_entire_snapshot() {
        let root =
            std::env::temp_dir().join(format!("gateway-queue-page-race-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        let target = Arc::clone(&gateway);
        let open = std::thread::spawn(move || target.open("t"));
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root}}),
        );
        answer_call(
            &receiver,
            "thread/resume",
            json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root}),
        );
        answer_queue_page(
            &receiver,
            None,
            json!({"data":queue_entries(0, 25),"nextCursor":"25"}),
        );
        let (_, answer) = queue_call(&receiver, "thread/queue/list");
        gateway
            .state
            .lock()
            .unwrap()
            .reduce(&json!({"method":"thread/queue/changed","params":{"threadId":"t"}}));
        answer
            .send(Ok(json!({"data":queue_entries(26, 32),"nextCursor":null})))
            .unwrap();
        answer_queue_page(
            &receiver,
            None,
            json!({"data":queue_entries(90, 93),"nextCursor":null}),
        );
        let result = open.join().unwrap().unwrap();
        assert_eq!(
            result["snapshot"]["queue"],
            json!(queue_entries(90, 93)),
            "No entries from the invalidated offset traversal survive"
        );
        assert_eq!(result["snapshot"]["sequence"], 1);
        assert!(receiver.try_recv().is_err());
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn queue_read_rejects_epoch_scope_and_archive_cycles_between_pages() {
        for case in [
            "epoch",
            "reconnect",
            "scope",
            "scope-cycle",
            "archive",
            "archive-cycle",
        ] {
            let root = std::env::temp_dir()
                .join(format!("gateway-queue-fence-{case}-{}", std::process::id()));
            let (gateway, receiver) = command_fixture(&root);
            let target = Arc::clone(&gateway);
            let read = std::thread::spawn(move || target.read_queue("t"));
            answer_queue_page(
                &receiver,
                None,
                json!({"data":queue_entries(0, 25),"nextCursor":"25"}),
            );
            let (_, answer) = queue_call(&receiver, "thread/queue/list");
            {
                let mut state = gateway.state.lock().unwrap();
                match case {
                    "epoch" => state.connection(Connection::Connected),
                    "reconnect" => state.connection(Connection::Reconnecting),
                    "scope" | "scope-cycle" => {
                        state.reduce(&json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"cwd":"/"}}}));
                        if case == "scope-cycle" {
                            state.remember(
                                &json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root}),
                            );
                        }
                    }
                    "archive" | "archive-cycle" => {
                        state
                            .reduce(&json!({"method":"thread/archived","params":{"threadId":"t"}}));
                        if case == "archive-cycle" {
                            state.reduce(
                                &json!({"method":"thread/unarchived","params":{"threadId":"t"}}),
                            );
                            state.remember(
                                &json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root}),
                            );
                        }
                    }
                    _ => unreachable!(),
                }
            }
            answer
                .send(Ok(json!({"data":queue_entries(25, 32),"nextCursor":null})))
                .unwrap();
            let result = read.join().unwrap();
            assert!(
                result.is_err(),
                "Reject the {case} traversal rather than retagging old entries"
            );
            assert!(
                receiver.try_recv().is_err(),
                "Do not continue on the changed {case} scope"
            );
            drop(gateway);
            remove_test_scratch(&root);
        }
    }

    #[test]
    fn perpetually_changing_or_unbounded_queues_fail_with_no_partial_result() {
        for changing in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "gateway-queue-bound-{changing}-{}",
                std::process::id()
            ));
            let (gateway, receiver) = command_fixture(&root);
            let target = Arc::clone(&gateway);
            let read = std::thread::spawn(move || target.read_queue("t"));
            let calls = if changing { 4 } else { QUEUE_MAX_PAGES };
            for index in 0..calls {
                let (_, answer) = queue_call(&receiver, "thread/queue/list");
                if changing {
                    gateway.state.lock().unwrap().reduce(
                        &json!({"method":"thread/queue/changed","params":{"threadId":"t"}}),
                    );
                }
                answer
                    .send(Ok(
                        json!({"data":[],"nextCursor":format!("cursor-{index}")}),
                    ))
                    .unwrap();
            }
            let error = match read.join().unwrap() {
                Err(error) => error,
                Ok(_) => panic!("Unbounded queue must fail"),
            };
            assert_eq!(
                error.details().0,
                if changing {
                    "unavailable"
                } else {
                    "gateway_error"
                }
            );
            assert!(error.details().1.contains(if changing {
                "kept changing"
            } else {
                "100-page"
            }));
            assert!(receiver.try_recv().is_err());
            drop(gateway);
            remove_test_scratch(&root);
        }
    }
    #[test]
    fn disconnect_is_scoped_guarded_and_invalidates_stale_gateway_handles() {
        let root = std::env::temp_dir().join(format!("gateway-disconnect-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        gateway.state.lock().unwrap().threads.get_mut("t").unwrap()["path"] =
            json!(root.join("rollout-t.jsonl"));
        for alias in ["t", "native:t", "rollout-t"] {
            assert!(gateway.owns_thread(alias));
        }
        for unrelated in ["other", "native:other", "prefix-rollout-t"] {
            assert!(!gateway.owns_thread(unrelated));
        }
        {
            let guard = gateway.lifecycle.read().unwrap();
            assert!(matches!(
                gateway.disconnect(),
                Err(DisconnectError::Busy(_))
            ));
            drop(guard);
        }
        for reason in ["active", "approval", "recovering"] {
            let mut state = gateway.state.lock().unwrap();
            if reason == "active" {
                state.threads.get_mut("t").unwrap()["status"] = json!({"type":"active"});
            }
            if reason == "approval" {
                state.pending.insert(
                    "request".into(),
                    json!({"method":"item/commandExecution/requestApproval"}),
                );
            }
            if reason == "recovering" {
                state.connection = Connection::Reconnecting;
            }
            drop(state);
            assert!(
                matches!(gateway.disconnect(), Err(DisconnectError::Busy(_))),
                "{reason}"
            );
            assert!(gateway.owns_thread("t"));
            let mut state = gateway.state.lock().unwrap();
            state.threads.get_mut("t").unwrap()["status"] = json!({"type":"idle"});
            state.pending.clear();
            state.connection = Connection::Connected;
        }
        assert!(
            receiver.try_recv().is_err(),
            "Rejected disconnects do not stop the worker"
        );
        let target = Arc::clone(&gateway);
        let closing = std::thread::spawn(move || target.disconnect());
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root,"status":{"type":"idle"}}}),
        );
        answer_call(
            &receiver,
            "thread/queue/list",
            json!({"data":[],"nextCursor":null}),
        );
        closing.join().unwrap().unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            Command::Stop
        ));
        assert!(!gateway.owns_thread("t"));
        gateway.disconnect().unwrap();
        let reply = gateway
            .route(&Request {
                method: "GET",
                name: "api/codex/models",
                query: "",
                body: &[],
                authenticated: true,
                origin_ok: true,
            })
            .unwrap();
        assert_eq!(reply.code, "409 Conflict");
        assert!(
            receiver.try_recv().is_err(),
            "Stale handles cannot issue RPC after disconnect"
        );
        drop(gateway);
        remove_test_scratch(&root);
    }

    #[test]
    fn disconnect_checks_the_current_native_queue_instead_of_only_cached_turns() {
        let root =
            std::env::temp_dir().join(format!("gateway-disconnect-queue-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        let target = Arc::clone(&gateway);
        let closing = std::thread::spawn(move || target.disconnect());
        answer_call(
            &receiver,
            "thread/read",
            json!({"thread":{"id":"t","cwd":root,"status":{"type":"idle"}}}),
        );
        answer_call(
            &receiver,
            "thread/queue/list",
            json!({"data":[{"id":"q","clientUserMessageId":"m","input":[{"type":"text","text":"queued"}]}],"nextCursor":null}),
        );
        assert!(
            matches!(closing.join().unwrap(), Err(DisconnectError::Busy(message)) if message.contains("queued input"))
        );
        assert!(gateway.owns_thread("t"));
        assert!(receiver.try_recv().is_err());
        drop(gateway);
        remove_test_scratch(&root);
    }
    #[test]
    fn independent_native_http_reads_remain_concurrent_while_disconnect_is_guarded() {
        let root =
            std::env::temp_dir().join(format!("gateway-concurrent-route-{}", std::process::id()));
        let (gateway, receiver) = command_fixture(&root);
        let route = |gateway: Arc<Gateway>, name: &'static str| {
            std::thread::spawn(move || {
                gateway
                    .route(&Request {
                        method: "GET",
                        name,
                        query: "",
                        body: &[],
                        authenticated: true,
                        origin_ok: true,
                    })
                    .unwrap()
            })
        };
        let first = route(gateway.clone(), "api/codex/models");
        let (_, answer) = queue_call(&receiver, "model/list");
        let second = route(gateway.clone(), "api/codex/projects");
        answer_call(
            &receiver,
            "project/list",
            json!({"data":[],"nextCursor":null}),
        );
        assert_eq!(second.join().unwrap().code, "200 OK");
        assert!(matches!(
            gateway.disconnect(),
            Err(DisconnectError::Busy(_))
        ));
        answer
            .send(Ok(json!({"data":[],"nextCursor":null})))
            .unwrap();
        assert_eq!(first.join().unwrap().code, "200 OK");
        drop(gateway);
        remove_test_scratch(&root);
    }
}
