//! A single native WebSocket owner. HTTP handlers never read or write its socket.

use super::auth::ServerToken;
use super::state::{id_key, Connection, State};
use super::transport::{Endpoint, NativeStream};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::{
    client::{client_with_config, IntoClientRequest},
    protocol::WebSocketConfig,
    Error, Message, WebSocket,
};

const RPC_TIMEOUT: Duration = Duration::from_secs(20);
// Native history can arrive in one frame. Match the existing aggregate-message bound,
// rather than Tungstenite's smaller 16 MiB frame default; keep both limits finite.
const NATIVE_RPC_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(super) enum Failure {
    Rejected(String),
    RpcRejected { details: String, error: Value },
    Unavailable(String),
    Uncertain(String),
}

impl Failure {
    pub fn message(&self) -> &str {
        match self {
            Self::Rejected(v) | Self::Unavailable(v) | Self::Uncertain(v) => v,
            Self::RpcRejected { details, .. } => details,
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::Rejected(_) | Self::RpcRejected { .. } => "rejected",
            Self::Unavailable(_) => "unavailable",
            Self::Uncertain(_) => "uncertain",
        }
    }

    pub fn rpc_rejection(method: &str, error: Value) -> Self {
        Self::RpcRejected {
            details: format!("{method}: {error}"),
            error,
        }
    }

    pub fn is_rpc_rejection(&self, code: i64, message: &str) -> bool {
        matches!(self, Self::RpcRejected { error, .. }
            if error["code"].as_i64() == Some(code) && error["message"].as_str() == Some(message))
    }
}

pub(super) enum Command {
    Call {
        method: String,
        params: Value,
        epoch: u64,
        deadline: Instant,
        answer: Sender<Result<Value, Failure>>,
    },
    Respond {
        id: Value,
        result: Value,
        epoch: u64,
        request_token: String,
        answer: Sender<Result<Value, Failure>>,
    },
    Stop,
}

struct Pending {
    method: String,
    thread_id: Option<String>,
    reserved_subscription: bool,
    started_sequence: u64,
    deadline: Instant,
    epoch: u64,
    continuation: Continuation,
    answer: Sender<Result<Value, Failure>>,
}

enum Continuation {
    Direct,
    CanonicalResume { resume: Value },
}

impl Pending {
    fn operation(&self) -> &str {
        match self.continuation {
            Continuation::Direct => &self.method,
            Continuation::CanonicalResume { .. } => "thread/resume canonical thread/read",
        }
    }
}

fn needs_canonical_history(result: &Value) -> bool {
    result["thread"]["historyMode"] == "legacy"
        || result["thread"]["historyMode"] == "paginated"
            && result["thread"]["turns"]
                .as_array()
                .is_some_and(|turns| turns.iter().any(|turn| turn["status"] == "inProgress"))
}

fn resume_identity<'a>(resume: &'a Value, thread_id: &str) -> Result<&'a str, Failure> {
    let cwd = resume["cwd"]
        .as_str()
        .or_else(|| resume["thread"]["cwd"].as_str());
    if resume["thread"]["id"].as_str() != Some(thread_id)
        || !cwd.is_some_and(|cwd| Path::new(cwd).is_absolute())
    {
        return Err(Failure::Rejected(
            "thread/resume canonical history requires the requested thread and an absolute execution directory".to_owned(),
        ));
    }
    Ok(cwd.unwrap())
}

/// Native resume uses a current-turn legacy builder that can renumber item-N
/// aliases. Full reads preserve global legacy aliases or paginated Core IDs.
/// Keep resume settings while replacing its history with that native full view.
fn canonical_resume(mut resume: Value, read: Value, thread_id: &str) -> Result<Value, Failure> {
    let cwd = resume_identity(&resume, thread_id)?;
    let mode = resume["thread"]["historyMode"].as_str();
    let thread = &read["thread"];
    if thread["id"].as_str() != Some(thread_id)
        || thread["cwd"].as_str() != Some(cwd)
        || !matches!(mode, Some("legacy" | "paginated"))
        || thread["historyMode"].as_str() != mode
        || !thread["turns"].is_array()
    {
        return Err(Failure::Rejected(
            "thread/resume canonical thread/read returned a different thread, execution directory, or invalid native history mode".to_owned(),
        ));
    }
    let turns = thread["turns"].as_array().unwrap();
    for active in resume["thread"]["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|turn| turn["status"] == "inProgress")
    {
        if !active["id"].as_str().is_some_and(|id| {
            turns
                .iter()
                .any(|turn| turn["id"] == id && turn["items"].is_array())
        }) {
            return Err(Failure::Rejected(
                "thread/resume canonical thread/read omitted the active turn or its materialized items".to_owned(),
            ));
        }
    }
    resume["thread"] = thread.clone();
    Ok(resume)
}

fn history_failure(failure: Failure) -> Failure {
    let context = |message: String| format!("thread/resume canonical history: {message}");
    match failure {
        Failure::Rejected(message) => Failure::Rejected(context(message)),
        Failure::RpcRejected { details, error } => Failure::RpcRejected {
            details: context(details),
            error,
        },
        Failure::Unavailable(message) => Failure::Unavailable(context(message)),
        Failure::Uncertain(message) => Failure::Uncertain(context(message)),
    }
}

fn resume_fence(state: &State, thread_id: &str, epoch: u64, after: u64) -> Result<(), Failure> {
    if state.epoch != epoch {
        return Err(Failure::Unavailable(
            "thread/resume canonical history belongs to a replaced native connection".to_owned(),
        ));
    }
    if state.snapshot_invalidated_after(thread_id, after) {
        return Err(Failure::Rejected(
            "thread/resume canonical history was invalidated by a newer execution scope or archive transition".to_owned(),
        ));
    }
    Ok(())
}

fn snapshot_scope(state: &Mutex<State>, result: &Value) -> super::state::DirectoryScope {
    let roots = state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .allowed_roots
        .clone();
    super::directory_scope(
        &roots,
        result["cwd"]
            .as_str()
            .or_else(|| result["thread"]["cwd"].as_str()),
    )
}

fn reduce_notification(state: &Mutex<State>, message: &Value) {
    let (roots, cwd) = {
        let current = state.lock().unwrap_or_else(|error| error.into_inner());
        (
            current.allowed_roots.clone(),
            super::notification_directory(&current, message),
        )
    };
    let scope = super::directory_scope(&roots, cwd.as_deref());
    state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .reduce_scoped(message, &scope);
}

fn finalize_resume(state: &Mutex<State>, call: Pending, result: Result<Value, Failure>) {
    let scope = result
        .as_ref()
        .ok()
        .map(|result| snapshot_scope(state, result));
    let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
    let result = if call.deadline <= Instant::now() {
        Err(Failure::Uncertain(format!(
            "{} response timed out after delivery",
            call.operation()
        )))
    } else {
        result
    };
    let result = if call.method == "thread/resume" {
        result.and_then(|result| {
            let thread_id = call.thread_id.as_deref().ok_or_else(|| {
                Failure::Rejected("thread/resume has no requested thread ID".to_owned())
            })?;
            resume_fence(&current, thread_id, call.epoch, call.started_sequence)?;
            Ok(result)
        })
    } else {
        result
    };
    if let Ok(result) = &result {
        if matches!(call.method.as_str(), "thread/start" | "thread/resume") {
            current.remember_scoped(
                result,
                Some(call.started_sequence),
                scope
                    .as_ref()
                    .expect("successful response has a scope decision"),
            );
        }
    } else if call.reserved_subscription {
        if let Some(thread_id) = &call.thread_id {
            current.subscribed.remove(thread_id);
            current.clear_provisional_streams(thread_id);
            current
                .pending
                .retain(|_, request| request["params"]["threadId"].as_str() != Some(thread_id));
        }
    }
    if call.method == "thread/resume" {
        if let Some(id) = &call.thread_id {
            current.finish_resume(id);
        }
    }
    drop(current);
    let _ = call.answer.send(result);
}

pub(super) fn call(
    sender: &Sender<Command>,
    method: &str,
    params: Value,
    epoch: u64,
) -> Result<Value, Failure> {
    let (answer, receiver) = mpsc::channel();
    sender
        .send(Command::Call {
            method: method.to_owned(),
            params,
            epoch,
            deadline: Instant::now() + RPC_TIMEOUT,
            answer,
        })
        .map_err(|_| Failure::Unavailable("Codex Gateway worker stopped".to_owned()))?;
    receiver
        .recv_timeout(RPC_TIMEOUT + Duration::from_secs(2))
        .unwrap_or_else(|_| {
            Err(Failure::Uncertain(format!(
                "{method} reply did not arrive before the deadline"
            )))
        })
}

pub(super) fn respond(
    sender: &Sender<Command>,
    id: Value,
    result: Value,
    epoch: u64,
    request_token: String,
) -> Result<Value, Failure> {
    let (answer, receiver) = mpsc::channel();
    sender
        .send(Command::Respond {
            id,
            result,
            epoch,
            request_token,
            answer,
        })
        .map_err(|_| Failure::Unavailable("Codex Gateway worker stopped".to_owned()))?;
    receiver.recv_timeout(RPC_TIMEOUT).unwrap_or_else(|_| {
        Err(Failure::Uncertain(
            "Runtime response delivery timed out".to_owned(),
        ))
    })
}

enum ConnectError {
    Unauthorized,
    Other(String),
}

impl From<std::io::Error> for ConnectError {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.to_string())
    }
}

/// Who holds the server end of a connection (#s145): `transport::tcp_server_owner`, or a test's
/// stand-in for an account it cannot be.
type ServerOwner = fn(&TcpStream) -> Option<u32>;

fn handshake(
    endpoint: &str,
    token: Option<&ServerToken>,
    owner: ServerOwner,
) -> Result<WebSocket<NativeStream>, ConnectError> {
    let mut request = endpoint
        .into_client_request()
        .map_err(|_| ConnectError::Other("Invalid WebSocket endpoint".into()))?;
    let uri = request.uri();
    let host = uri
        .host()
        .ok_or_else(|| ConnectError::Other("WebSocket endpoint has no host".into()))?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = uri.port_u16().unwrap_or(80);
    let address = (host, port)
        .to_socket_addrs()?
        .find(|address| address.ip().is_loopback())
        .ok_or_else(|| ConnectError::Other("WebSocket endpoint must resolve to loopback".into()))?;
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(super::transport::HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(super::transport::HANDSHAKE_TIMEOUT))?;
    if let Some(token) = token {
        // #s145: the credential goes only to this OS user's server, checked on this connection.
        super::transport::credential_may_go_to(owner(&stream)).map_err(ConnectError::Other)?;
        request
            .headers_mut()
            .insert(tungstenite::http::header::AUTHORIZATION, token.bearer());
    }
    let config = WebSocketConfig::default()
        .max_frame_size(Some(NATIVE_RPC_BYTES))
        .max_message_size(Some(NATIVE_RPC_BYTES));
    let (socket, _) = client_with_config(request, NativeStream::Tcp(stream), Some(config))
        .map_err(|error| {
            // A peer can echo a credential in headers/body. Never include handshake data in logs.
            match error {
                tungstenite::HandshakeError::Failure(Error::Http(response))
                    if response.status() == tungstenite::http::StatusCode::UNAUTHORIZED =>
                {
                    ConnectError::Unauthorized
                }
                tungstenite::HandshakeError::Failure(Error::Http(response)) => {
                    ConnectError::Other(format!(
                        "Native WebSocket handshake refused with HTTP {}",
                        response.status().as_u16()
                    ))
                }
                tungstenite::HandshakeError::Failure(Error::Io(error)) => {
                    ConnectError::Other(error.to_string())
                }
                _ => ConnectError::Other("Native WebSocket handshake failed".into()),
            }
        })?;
    socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(40)))?;
    Ok(socket)
}

fn socket(endpoint: &str, token: &ServerToken) -> Result<WebSocket<NativeStream>, String> {
    socket_owned_by(endpoint, token, super::transport::tcp_server_owner)
}

fn socket_owned_by(
    endpoint: &str,
    token: &ServerToken,
    owner: ServerOwner,
) -> Result<WebSocket<NativeStream>, String> {
    // Recheck on EVERY connection, including reconnects after a process replacement.
    match handshake(endpoint, None, owner) {
        Ok(mut socket) => {
            let _ = socket.close(None);
            return Err(
                "Refusing a Codex server that accepts unauthenticated WebSocket clients".into(),
            );
        }
        Err(ConnectError::Unauthorized) => {}
        Err(ConnectError::Other(error)) => return Err(error),
    }
    handshake(endpoint, Some(token), owner).map_err(|error| match error {
        ConnectError::Unauthorized => "Native server rejected the Monitor credential".into(),
        ConnectError::Other(error) => error,
    })
}

/// Credential choice is bound to its endpoint, never inferred from a failed handshake.
pub(super) enum NativeConnection {
    WebSocket {
        endpoint: String,
        token: ServerToken,
    },
    Unix {
        path: std::path::PathBuf,
    },
}
impl NativeConnection {
    pub fn configured(endpoint: &str, state: &Path) -> anyhow::Result<Self> {
        Ok(match Endpoint::parse(endpoint)? {
            Endpoint::AuthenticatedWebSocket(endpoint) => Self::WebSocket {
                endpoint,
                token: ServerToken::load_or_create(state)?,
            },
            Endpoint::PrivateUnix(path) => Self::Unix { path },
        })
    }
    fn connect(&self) -> Result<WebSocket<NativeStream>, String> {
        match self {
            Self::WebSocket { endpoint, token } => socket(endpoint, token),
            Self::Unix { path } => {
                #[cfg(unix)]
                {
                    let stream =
                        super::transport::connect_unix(path).map_err(|e| format!("{e:#}"))?;
                    let config = WebSocketConfig::default()
                        .max_frame_size(Some(NATIVE_RPC_BYTES))
                        .max_message_size(Some(NATIVE_RPC_BYTES));
                    let (socket, _) =
                        client_with_config("ws://localhost/rpc", stream, Some(config))
                            .map_err(|_| "Private Unix WebSocket handshake failed".to_owned())?;
                    socket
                        .get_ref()
                        .set_read_timeout(Some(Duration::from_millis(40)))
                        .map_err(|e| e.to_string())?;
                    Ok(socket)
                }
                #[cfg(not(unix))]
                {
                    let _ = path;
                    Err("Unix sockets are unsupported on this platform".into())
                }
            }
        }
    }
    pub fn verify(&self) -> Result<(), String> {
        self.connect()?
            .close(None)
            .map_err(|_| "Close native connection probe".into())
    }
}

fn idle(error: &Error) -> bool {
    matches!(error, Error::Io(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

fn write<S: Read + Write>(socket: &mut WebSocket<S>, value: Value) -> Result<(), String> {
    socket
        .send(Message::Text(value.to_string().into()))
        .map_err(|e| e.to_string())
}

fn receive<S: Read + Write>(socket: &mut WebSocket<S>) -> Result<Option<Value>, String> {
    match socket.read() {
        Ok(Message::Text(text)) => {
            let packet: Value =
                serde_json::from_str(&text).map_err(|e| format!("Invalid Codex RPC JSON: {e}"))?;
            if !packet.is_object() {
                return Err("Invalid Codex RPC envelope: expected an object".to_owned());
            }
            Ok(Some(packet))
        }
        Ok(Message::Close(_)) => Err("Codex WebSocket closed".to_owned()),
        Ok(_) => Ok(None),
        Err(error) if idle(&error) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn bootstrap_call<S: Read + Write>(
    socket: &mut WebSocket<S>,
    state: &Mutex<State>,
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, Failure> {
    write(socket, json!({"id":id,"method":method,"params":params}))
        .map_err(Failure::Unavailable)?;
    let deadline = Instant::now() + RPC_TIMEOUT;
    while Instant::now() < deadline {
        if state.lock().unwrap_or_else(|e| e.into_inner()).stopping {
            return Err(Failure::Unavailable("Native mode is closing".into()));
        }
        if let Some(message) = receive(socket).map_err(Failure::Unavailable)? {
            if message["id"] == id
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| Failure::rpc_rejection(method, message["error"].clone()));
            }
            reduce_notification(state, &message);
        }
    }
    Err(Failure::Unavailable(format!(
        "{method} timed out during native connection setup"
    )))
}

fn bootstrap(
    connection: &NativeConnection,
    state: &Mutex<State>,
) -> Result<WebSocket<NativeStream>, String> {
    let mut socket = connection.connect()?;
    bootstrap_call(
        &mut socket,
        state,
        "gateway-initialize",
        "initialize",
        json!({"clientInfo":{"name":"agent_monitor_gateway","title":"Agent Monitor Codex Gateway","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true}}),
    ).map_err(|failure| failure.message().to_owned())?;
    write(&mut socket, json!({"method":"initialized"}))?;
    let subscribed: Vec<String> = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.clear();
        state
            .subscribed
            .iter()
            .filter(|id| !state.archived_threads.contains(*id))
            .cloned()
            .collect()
    };
    for (index, thread_id) in subscribed.into_iter().enumerate() {
        let (epoch, started_sequence) = {
            let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
            current.begin_resume(&thread_id);
            (current.epoch, current.sequence)
        };
        let recovered = bootstrap_call(
            &mut socket,
            state,
            &format!("gateway-recover-{index}"),
            "thread/resume",
            json!({"threadId":thread_id}),
        )
        .and_then(|resume| {
            if !needs_canonical_history(&resume) {
                return Ok(resume);
            }
            resume_identity(&resume, &thread_id)?;
            let read = bootstrap_call(
                &mut socket,
                state,
                &format!("gateway-recover-history-{index}"),
                "thread/read",
                json!({"threadId":thread_id,"includeTurns":true}),
            )
            .map_err(history_failure)?;
            canonical_resume(resume, read, &thread_id)
        })
        .and_then(|result| {
            resume_fence(
                &state.lock().unwrap_or_else(|e| e.into_inner()),
                &thread_id,
                epoch,
                started_sequence,
            )?;
            Ok(result)
        });
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finish_resume(&thread_id);
        match recovered {
            Ok(result) => {
                let scope = snapshot_scope(state, &result);
                state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remember_scoped(&result, Some(started_sequence), &scope);
            }
            Err(failure) => {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                if super::archived_rejection(&failure, &thread_id) {
                    // Another native client may archive a selected active thread
                    // while this socket is offline. Preserve the selection, pause
                    // writers, and let the browser verify fresh native inventory and
                    // history through open rather than treating archive as deletion.
                    state.archived_threads.insert(thread_id.clone());
                    state.clear_provisional_streams(&thread_id);
                    state.settings.remove(&thread_id);
                    state.pending.retain(|_, request| {
                        request["params"]["threadId"].as_str() != Some(&thread_id)
                    });
                    state.emit(
                        "gateway/snapshot-required",
                        json!({"threadId":thread_id,"accessMode":"archived"}),
                        None,
                    );
                    continue;
                }
                state.threads.remove(&thread_id);
                state.settings.remove(&thread_id);
                state.subscribed.remove(&thread_id);
                state.clear_provisional_streams(&thread_id);
                state.emit(
                    "gateway/recoveryError",
                    json!({"threadId":thread_id,"message":failure.message()}),
                    None,
                );
            }
        }
    }
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .connection(Connection::Connected);
    Ok(socket)
}

/// Release only subscriptions acquired by this connection. Use one shared deadline
/// so an unresponsive server or a large history cannot trap the mode switch.
fn release_subscriptions<S: Read + Write>(socket: &mut WebSocket<S>, state: &Mutex<State>) {
    let threads: Vec<String> = state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .subscribed
        .iter()
        .cloned()
        .collect();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut waiting = HashMap::new();
    let result = (|| -> Result<(), String> {
        for (index, thread) in threads.iter().enumerate() {
            if Instant::now() >= deadline {
                return Err("Timed out releasing native subscriptions".into());
            }
            let id = format!("gateway-unsubscribe-{index}");
            write(
                socket,
                json!({"id":id,"method":"thread/unsubscribe","params":{"threadId":thread}}),
            )?;
            waiting.insert(id, thread);
        }
        while !waiting.is_empty() && Instant::now() < deadline {
            if let Some(message) = receive(socket)? {
                if let Some(id) = message["id"].as_str() {
                    if waiting.contains_key(id)
                        && (message.get("result").is_some() || message.get("error").is_some())
                    {
                        waiting.remove(id);
                        if message.get("error").is_some() {
                            return Err("Server refused a native subscription release".into());
                        }
                    }
                }
            }
        }
        if !waiting.is_empty() {
            return Err("Timed out releasing native subscriptions".into());
        }
        Ok(())
    })();
    if let Err(error) = result {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown_warnings
            .push(format!(
                "{error}; Monitor closed its connection. Work on an external server may continue."
            ));
    }
}

/// Reconnect and recover subscriptions before accepting writes. Any call sent on a lost
/// connection is uncertain, even when the server may already have committed its action.
pub(super) fn run(
    endpoint: String,
    connection: NativeConnection,
    state: Arc<Mutex<State>>,
    receiver: Receiver<Command>,
) {
    let mut next_id = 0_u64;
    loop {
        if state.lock().unwrap_or_else(|e| e.into_inner()).stopping {
            return;
        }
        let mut socket = match bootstrap(&connection, &state) {
            Ok(socket) => socket,
            Err(message) => {
                state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .reconnect(format!(
                        "Initialize/recover Codex RPC at {endpoint}: {message}"
                    ));
                if !wait_to_retry(&receiver, &message) {
                    return;
                }
                continue;
            }
        };
        let mut pending: HashMap<String, Pending> = HashMap::new();
        let disconnected = 'connected: loop {
            if state.lock().unwrap_or_else(|e| e.into_inner()).stopping {
                release_subscriptions(&mut socket, &state);
                for (_, call) in pending.drain() {
                    finalize_resume(&state, call, Err(Failure::Uncertain(
                        "Native mode closed after delivery; check the saved action receipt before retrying".into(),
                    )));
                }
                let _ = socket.close(None);
                return;
            }
            while let Ok(command) = receiver.try_recv() {
                if state.lock().unwrap_or_else(|e| e.into_inner()).stopping {
                    unavailable(command, "Native mode is closing");
                    break;
                }
                match command {
                    Command::Stop => {
                        let _ = socket.close(None);
                        return;
                    }
                    Command::Call {
                        method,
                        params,
                        epoch,
                        deadline,
                        answer,
                    } => {
                        if params["threadId"].as_str().is_some_and(|id| {
                            state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .archived_threads
                                .contains(id)
                        }) && matches!(
                            method.as_str(),
                            "thread/settings/update"
                                | "thread/queue/add"
                                | "thread/queue/start"
                                | "thread/queue/delete"
                                | "turn/steer"
                                | "turn/interrupt"
                                | "thread/archive"
                        ) {
                            let _ = answer.send(Err(Failure::Rejected("Archived history is read-only; restore and reopen this thread before delivery".to_owned())));
                            continue;
                        }
                        if params["threadId"].as_str().is_some_and(|id| {
                            state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .scope_closed(id)
                        }) && matches!(
                            method.as_str(),
                            "thread/settings/update"
                                | "thread/queue/add"
                                | "thread/queue/start"
                                | "thread/queue/delete"
                                | "turn/steer"
                                | "turn/interrupt"
                                | "thread/archive"
                                | "thread/unarchive"
                        ) {
                            let _ = answer.send(Err(Failure::Rejected("Shared execution directory moved outside the Gateway's configured project roots before delivery".to_owned())));
                            continue;
                        }
                        if deadline <= Instant::now()
                            || state.lock().unwrap_or_else(|e| e.into_inner()).epoch != epoch
                        {
                            let _ = answer.send(Err(Failure::Unavailable(
                                "RPC expired or its native connection changed before delivery"
                                    .to_owned(),
                            )));
                            continue;
                        }
                        next_id += 1;
                        let id = format!("gateway-{next_id}");
                        let (thread_id, reserved_subscription, started_sequence) = {
                            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                            let thread_id = params["threadId"].as_str().map(str::to_owned);
                            let reserved = method == "thread/resume"
                                && thread_id
                                    .as_ref()
                                    .is_some_and(|id| state.subscribed.insert(id.clone()));
                            if method == "thread/resume" {
                                if let Some(id) = &thread_id {
                                    state.begin_resume(id);
                                }
                            }
                            (thread_id, reserved, state.sequence)
                        };
                        pending.insert(
                            id.clone(),
                            Pending {
                                method: method.clone(),
                                thread_id,
                                reserved_subscription,
                                started_sequence,
                                deadline,
                                epoch,
                                continuation: Continuation::Direct,
                                answer,
                            },
                        );
                        if let Err(message) = write(
                            &mut socket,
                            json!({"id":id,"method":method,"params":params}),
                        ) {
                            break 'connected message;
                        }
                    }
                    Command::Respond {
                        id,
                        result,
                        epoch,
                        request_token,
                        answer,
                    } => {
                        let current = state.lock().unwrap_or_else(|e| e.into_inner());
                        let request = current.pending.get(&id_key(&id));
                        let valid = current.epoch == epoch
                            && request.is_some_and(|request| {
                                request["canRespond"] == true
                                    && request["requestToken"].as_str()
                                        == Some(request_token.as_str())
                                    && !request["params"]["threadId"]
                                        .as_str()
                                        .is_some_and(|id| current.archived_threads.contains(id))
                            });
                        drop(current);
                        if !valid {
                            let _ = answer.send(Err(Failure::Rejected(
                                "Pending runtime request is stale or owned by its tool executor"
                                    .to_owned(),
                            )));
                            continue;
                        }
                        if let Err(message) = write(&mut socket, json!({"id":id,"result":result})) {
                            let _ = answer.send(Err(Failure::Uncertain(message.clone())));
                            break 'connected message;
                        }
                        // This acknowledges delivery, not first-answer ownership. The runtime
                        // resolution event is the authority for clearing pending controls.
                        let _ = answer.send(Ok(json!({"delivered":true})));
                    }
                }
            }
            match receive(&mut socket) {
                Ok(Some(message)) => {
                    if let Some(id) = message.get("id").and_then(Value::as_str) {
                        if message.get("result").is_some() || message.get("error").is_some() {
                            if let Some(mut call) = pending.remove(id) {
                                let result = message.get("result").cloned().ok_or_else(|| {
                                    Failure::rpc_rejection(
                                        call.operation(),
                                        message["error"].clone(),
                                    )
                                });
                                let canonical = matches!(
                                    call.continuation,
                                    Continuation::CanonicalResume { .. }
                                );
                                let result = match &call.continuation {
                                    Continuation::CanonicalResume { resume } => {
                                        result.and_then(|read| {
                                            canonical_resume(
                                                resume.clone(),
                                                read,
                                                call.thread_id.as_deref().unwrap_or_default(),
                                            )
                                        })
                                    }
                                    Continuation::Direct => result,
                                };
                                if call.method == "thread/resume" {
                                    if let Ok(resume) = &result {
                                        if needs_canonical_history(resume) && !canonical {
                                            let current =
                                                state.lock().unwrap_or_else(|e| e.into_inner());
                                            let validated = call
                                                .thread_id
                                                .as_deref()
                                                .ok_or_else(|| {
                                                    Failure::Rejected(
                                                        "thread/resume has no requested thread ID"
                                                            .to_owned(),
                                                    )
                                                })
                                                .and_then(|thread_id| {
                                                    resume_identity(resume, thread_id)?;
                                                    resume_fence(
                                                        &current,
                                                        thread_id,
                                                        call.epoch,
                                                        call.started_sequence,
                                                    )
                                                });
                                            drop(current);
                                            if validated.is_ok() {
                                                next_id += 1;
                                                let followup_id = format!("gateway-{next_id}");
                                                let thread_id = call.thread_id.clone().unwrap();
                                                call.continuation = Continuation::CanonicalResume {
                                                    resume: resume.clone(),
                                                };
                                                pending.insert(followup_id.clone(), call);
                                                if let Err(message) = write(
                                                    &mut socket,
                                                    json!({"id":followup_id,"method":"thread/read","params":{"threadId":thread_id,"includeTurns":true}}),
                                                ) {
                                                    break 'connected message;
                                                }
                                                continue;
                                            }
                                            // Finalize below so reserved subscriptions and archived
                                            // pending replay receive the same failure cleanup.
                                            finalize_resume(&state, call, validated.and(result));
                                            continue;
                                        }
                                    }
                                }
                                // Preserve socket order: an HTTP handler may wake
                                // after a later settings event has already arrived.
                                finalize_resume(&state, call, result);
                            }
                            continue;
                        }
                    }
                    reduce_notification(&state, &message);
                }
                Ok(None) => {}
                Err(message) => break message,
            }
            let expired: Vec<String> = pending
                .iter()
                .filter(|(_, call)| call.deadline <= Instant::now())
                .map(|(id, _)| id.clone())
                .collect();
            for id in expired {
                if let Some(call) = pending.remove(&id) {
                    let operation = call.operation().to_owned();
                    finalize_resume(
                        &state,
                        call,
                        Err(Failure::Uncertain(format!(
                            "{operation} response timed out after delivery"
                        ))),
                    );
                }
            }
        };
        for (_, call) in pending {
            let _ = call.answer.send(Err(Failure::Uncertain(format!(
                "{} disconnected after delivery: {disconnected}",
                call.operation()
            ))));
        }
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reconnect(format!("Codex WebSocket at {endpoint}: {disconnected}"));
    }
}

/// Wait before the next connection attempt after a failure that said `message`, answering any
/// command meanwhile as unavailable. False when the worker must stop.
///
/// The usual wait is 250 ms, and a command ends it at once. After an owner refusal (#s240) the
/// wait is `OWNER_REFUSAL_PAUSE` and a command does NOT end it: another account holding the
/// port is no reason to look again every time a page asks, and each look runs the owner
/// lookup for up to its whole bound.
fn wait_to_retry(receiver: &Receiver<Command>, message: &str) -> bool {
    let pause = super::transport::retry_pause(message);
    let patient = pause > Duration::from_millis(250);
    let until = Instant::now() + pause;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(left) {
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            Ok(command) => {
                unavailable(command, message);
                if !patient {
                    return true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return true,
        }
    }
}

fn unavailable(command: Command, message: &str) {
    match command {
        Command::Call { answer, .. } | Command::Respond { answer, .. } => {
            let _ = answer.send(Err(Failure::Unavailable(format!(
                "Shared Codex server is disconnected: {message}"
            ))));
        }
        Command::Stop => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::scope_fixture::ScopeFixture;
    use std::net::TcpListener;

    fn request(socket: &mut WebSocket<TcpStream>) -> Value {
        loop {
            if let Message::Text(message) = socket.read().unwrap() {
                return serde_json::from_str(&message).unwrap();
            }
        }
    }

    // In-process fixture only: no Codex runtime or model is launched.
    #[allow(clippy::result_large_err)]
    fn authenticated(listener: &TcpListener) -> WebSocket<TcpStream> {
        let (probe, _) = listener.accept().unwrap();
        probe
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        assert!(tungstenite::accept_hdr(
            probe,
            |request: &tungstenite::handshake::server::Request, _| {
                assert!(request.headers().get("authorization").is_none());
                Err(tungstenite::http::Response::builder()
                    .status(401)
                    .body(None)
                    .unwrap())
            }
        )
        .is_err());
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        tungstenite::accept_hdr(
            stream,
            |request: &tungstenite::handshake::server::Request, response| {
                assert!(request.headers().get("origin").is_none());
                assert_eq!(
                    request.headers()["authorization"],
                    ServerToken::fixture().bearer()
                );
                Ok(response)
            },
        )
        .unwrap()
    }

    fn initialize(listener: &TcpListener) -> WebSocket<TcpStream> {
        let mut socket = authenticated(listener);
        let init = request(&mut socket);
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["capabilities"]["experimentalApi"], true);
        write(
            &mut socket,
            json!({"id":init["id"],"result":{"userAgent":"fixture"}}),
        )
        .unwrap();
        assert_eq!(request(&mut socket)["method"], "initialized");
        socket
    }

    #[test]
    fn mode_shutdown_releases_only_its_subscription_and_preserves_uncertain_delivery() {
        for release in ["success", "rejected", "timeout"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let (delivered, received) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let mut socket = initialize(&listener);
                let pending_request = request(&mut socket);
                assert_eq!(pending_request["method"], "model/list");
                delivered.send(()).unwrap(); // Deliberately leave the reply pending.
                let subscription = request(&mut socket);
                assert_eq!(subscription["method"], "thread/unsubscribe");
                assert_eq!(subscription["params"]["threadId"], "owned");
                match release {
                "success" => write(&mut socket, json!({"id":subscription["id"],"result":{}})).unwrap(),
                "rejected" => write(&mut socket, json!({"id":subscription["id"],"error":{"code":-32601,"message":"Unsupported unsubscribe"}})).unwrap(),
                _ => (),
            }
                assert!(matches!(socket.read(), Ok(Message::Close(_)) | Err(_)));
            });
            let state = Arc::new(Mutex::new(State::default()));
            let (sender, receiver) = mpsc::channel();
            let target = state.clone();
            let worker = std::thread::spawn(move || {
                run(
                    endpoint.clone(),
                    NativeConnection::WebSocket {
                        endpoint,
                        token: ServerToken::fixture(),
                    },
                    target,
                    receiver,
                )
            });
            await_epoch(&state, 0);
            let epoch = {
                let mut state = state.lock().unwrap();
                state.subscribed.insert("owned".into());
                state.epoch
            };
            let calls = sender.clone();
            let read = std::thread::spawn(move || call(&calls, "model/list", json!({}), epoch));
            received.recv_timeout(Duration::from_secs(5)).unwrap();
            let start = Instant::now();
            state.lock().unwrap().stopping = true;
            sender.send(Command::Stop).unwrap();
            worker.join().unwrap();
            assert!(start.elapsed() < Duration::from_secs(5));
            assert!(matches!(read.join().unwrap(), Err(Failure::Uncertain(_))));
            assert_eq!(
                state.lock().unwrap().shutdown_warnings.is_empty(),
                release == "success"
            );
            server.join().unwrap();
        }
    }

    fn await_epoch(state: &Mutex<State>, epoch: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let state = state.lock().unwrap();
            if state.epoch >= epoch && state.connection == Connection::Connected {
                return;
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("native connection did not recover");
    }

    fn active_resume(cwd: &str) -> Value {
        json!({"thread":{"id":"t","cwd":cwd,"historyMode":"paginated","turns":[{"id":"turn","status":"inProgress","items":[
            {"id":"item-1","type":"userMessage","content":[{"type":"text","text":"prompt"}]},
            {"id":"item-2","type":"agentMessage","text":"same"}
        ]}]},"cwd":cwd,"model":"resume-model","reasoningEffort":"high","sandbox":{"type":"readOnly"}})
    }

    fn canonical_read(cwd: &str) -> Value {
        json!({"thread":{"id":"t","cwd":cwd,"historyMode":"paginated","turns":[{"id":"turn","status":"inProgress","items":[
            {"id":"user","type":"userMessage","content":[{"type":"text","text":"prompt"}]},
            {"id":"agent-a","type":"agentMessage","text":"same"},
            {"id":"agent-b","type":"agentMessage","text":"same"}
        ]}]}})
    }

    #[test]
    fn large_single_frame_resume_preserves_history_and_keeps_the_native_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let history = format!("{}END_OF_HISTORY", "x".repeat(24 * 1024 * 1024));
        let expected = history.clone();
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let resume = request(&mut socket);
            assert_eq!(resume["method"], "thread/resume");
            write(
                &mut socket,
                json!({"id":resume["id"],"result":{
                    "thread":{"id":"large","cwd":"/tmp","historyMode":"paginated","turns":[
                        {"id":"past","status":"completed","items":[
                            {"id":"item","type":"agentMessage","text":history}
                        ]}
                    ]},"cwd":"/tmp","model":"fixture-model","reasoningEffort":"low"
                }}),
            )
            .unwrap();
            let next = request(&mut socket);
            assert_eq!(next["method"], "model/list");
            write(
                &mut socket,
                json!({"id":next["id"],"result":{"data":[],"sameConnection":true}}),
            )
            .unwrap();
            let _ = socket.read();
        });
        let state = Arc::new(Mutex::new(State::default()));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        let resumed = call(&sender, "thread/resume", json!({"threadId":"large"}), 1).unwrap();
        assert_eq!(resumed["thread"]["turns"][0]["items"][0]["text"], expected);
        let current = state.lock().unwrap();
        assert!(current.connection == Connection::Connected);
        assert_eq!(current.epoch, 1);
        assert_eq!(
            current.threads["large"]["turns"][0]["items"][0]["text"],
            expected
        );
        drop(current);
        assert_eq!(
            call(&sender, "model/list", json!({}), 1).unwrap()["sameConnection"],
            true
        );
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn setup_and_idle_disconnect_causes_survive_until_a_successful_reconnect() {
        for malformed_setup in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let (release, gate) = mpsc::channel();
            let fixture = std::thread::spawn(move || {
                let mut first = if malformed_setup {
                    let mut first = authenticated(&listener);
                    assert_eq!(request(&mut first)["method"], "initialize");
                    first.send(Message::Text("{invalid-json".into())).unwrap();
                    first
                } else {
                    let mut first = initialize(&listener);
                    first.close(None).unwrap();
                    first
                };
                gate.recv_timeout(Duration::from_secs(5)).unwrap();
                let _ = first.close(None);
                let mut recovered = initialize(&listener);
                let _ = recovered.read();
            });
            let state = Arc::new(Mutex::new(State::connecting(
                vec![],
                "cause-test".to_owned(),
            )));
            let (_, events) = state.lock().unwrap().subscribe();
            let (commands, receiver) = mpsc::channel();
            let worker_state = Arc::clone(&state);
            let worker_endpoint = endpoint.clone();
            let worker = std::thread::spawn(move || {
                run(
                    worker_endpoint.clone(),
                    NativeConnection::WebSocket {
                        endpoint: worker_endpoint,
                        token: ServerToken::fixture(),
                    },
                    worker_state,
                    receiver,
                )
            });
            let event = events.recv_timeout(Duration::from_secs(3)).unwrap();
            let event = if event["params"]["connection"] == "connected" {
                events.recv_timeout(Duration::from_secs(3)).unwrap()
            } else {
                event
            };
            let cause = event["params"]["connectionError"].as_str().unwrap();
            assert!(cause.contains(&endpoint), "{cause}");
            assert!(
                cause.contains(if malformed_setup {
                    "Invalid Codex RPC JSON"
                } else {
                    "Codex WebSocket closed"
                }),
                "{cause}"
            );
            assert_eq!(state.lock().unwrap().status()["connectionError"], cause);
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while state.lock().unwrap().connection != Connection::Connected {
                assert!(
                    Instant::now() < deadline,
                    "Native fixture failed to reconnect"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(state.lock().unwrap().status()["connectionError"].is_null());
            commands.send(Command::Stop).unwrap();
            worker.join().unwrap();
            fixture.join().unwrap();
        }
    }

    #[test]
    fn non_object_native_frames_are_refused_without_discarding_valid_objects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut socket = authenticated(&listener);
            for packet in [
                "null",
                "[]",
                "17",
                "\"text\"",
                r#"{"method":"future/event","params":{}}"#,
            ] {
                socket.send(Message::Text(packet.into())).unwrap();
            }
        });
        let mut native = socket(&endpoint, &ServerToken::fixture()).unwrap();
        for _ in 0..4 {
            assert_eq!(
                receive(&mut native).unwrap_err(),
                "Invalid Codex RPC envelope: expected an object"
            );
        }
        assert_eq!(
            receive(&mut native).unwrap().unwrap()["method"],
            "future/event"
        );
        fixture.join().unwrap();
    }

    #[test]
    fn oversized_native_frame_is_rejected_before_its_payload_is_read() {
        use std::io::Write;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut socket = authenticated(&listener);
            // Advertise an oversized single text frame without allocating/sending its body.
            let mut header = vec![0x81, 127];
            header.extend_from_slice(&((NATIVE_RPC_BYTES as u64) + 1).to_be_bytes());
            socket.get_mut().write_all(&header).unwrap();
            let _ = socket.read();
        });
        let mut native = socket(&endpoint, &ServerToken::fixture()).unwrap();
        assert_eq!(native.get_config().max_message_size, Some(NATIVE_RPC_BYTES));
        let error = receive(&mut native).unwrap_err();
        assert!(error.contains("67108865 > 67108864"), "{error}");
        drop(native);
        fixture.join().unwrap();
    }

    #[test]
    fn active_paginated_resume_reads_canonical_ids_without_blocking_actor_or_losing_events() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let resume = request(&mut socket);
            assert_eq!(resume["method"], "thread/resume");
            write(
                &mut socket,
                json!({"id":resume["id"],"result":active_resume("/tmp")}),
            )
            .unwrap();
            let read = request(&mut socket);
            assert_eq!(read["method"], "thread/read");
            assert_eq!(read["params"], json!({"threadId":"t","includeTurns":true}));
            write(&mut socket, json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"turn","status":"inProgress","items":[]}}})).unwrap();
            write(&mut socket, json!({"method":"item/started","params":{"threadId":"t","turnId":"turn","item":{"id":"agent-a","type":"agentMessage","text":"same"}}})).unwrap();
            write(&mut socket, json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"agent-a","delta":" suffix"}})).unwrap();
            write(&mut socket, json!({"id":9,"method":"item/tool/requestUserInput","params":{"threadId":"t","questions":[{"id":"q"}]}})).unwrap();
            write(&mut socket, json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"cwd":"/tmp","model":"event-model","effort":"low"}}})).unwrap();
            // A different HTTP request must be delivered while the continuation waits.
            let independent = request(&mut socket);
            assert_eq!(independent["method"], "model/list");
            write(
                &mut socket,
                json!({"id":independent["id"],"result":{"data":[]}}),
            )
            .unwrap();
            write(
                &mut socket,
                json!({"id":read["id"],"result":canonical_read("/tmp")}),
            )
            .unwrap();
            let _ = socket.read();
        });
        let state = Arc::new(Mutex::new(State::default()));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        state.lock().unwrap().remember(
            &json!({"thread":{"id":"t","cwd":"/tmp","historyMode":"paginated","turns":[]}}),
        );
        let resume_sender = sender.clone();
        let resume = std::thread::spawn(move || {
            call(&resume_sender, "thread/resume", json!({"threadId":"t"}), 1)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.lock().unwrap().pending.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(state.lock().unwrap().threads["t"]["turns"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["id"] != "item-2"));
        assert_eq!(
            call(&sender, "model/list", json!({}), 1).unwrap(),
            json!({"data":[]})
        );
        let response = resume.join().unwrap().unwrap();
        assert_eq!(response["model"], "resume-model");
        assert_eq!(response["reasoningEffort"], "high");
        assert_eq!(response["sandbox"]["type"], "readOnly");
        let snapshot = state.lock().unwrap().snapshot("t", json!([]));
        let items = snapshot["thread"]["turns"][0]["items"].as_array().unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["user", "agent-a", "agent-b"]
        );
        assert_eq!(items[1]["text"], "same suffix");
        assert_eq!(
            items[2]["text"], "same",
            "Identical independent messages must survive without text deduplication."
        );
        assert_eq!(snapshot["settings"]["model"], "event-model");
        assert_eq!(snapshot["pending"][0]["id"], 9);
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn active_and_terminal_legacy_resumes_replace_current_turn_aliases_with_full_global_history() {
        for status in ["inProgress", "completed"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let (ready, wait_ready) = mpsc::channel();
            let canonical = json!({"thread":{"id":"t","cwd":"/tmp","historyMode":"legacy","turns":[
                {"id":"past","status":"completed","itemsView":"full","items":[
                    {"id":"item-1","type":"userMessage","content":[{"type":"text","text":"same prompt"}]},
                    {"id":"item-2","type":"agentMessage","text":"same"}
                ]},
                {"id":"turn","status":status,"itemsView":"full","items":[
                    {"id":"item-3","type":"userMessage","content":[{"type":"text","text":"same prompt"}]},
                    {"id":"item-4","type":"agentMessage","text":"same"},
                    {"id":"item-5","type":"agentMessage","text":"same"}
                ]}
            ]}});
            let fixture_read = canonical.clone();
            let fixture = std::thread::spawn(move || {
                let mut socket = initialize(&listener);
                let resume = request(&mut socket);
                assert_eq!(resume["method"], "thread/resume");
                let mut alias_resume = active_resume("/tmp");
                alias_resume["thread"]["historyMode"] = json!("legacy");
                alias_resume["thread"]["turns"][0]["status"] = json!(status);
                write(
                    &mut socket,
                    json!({"id":resume["id"],"result":alias_resume}),
                )
                .unwrap();
                let read = request(&mut socket);
                assert_eq!(
                    read["method"], "thread/read",
                    "Terminal legacy resumes also need the full global view."
                );
                assert_eq!(read["params"], json!({"threadId":"t","includeTurns":true}));
                write(&mut socket, json!({"method":"item/started","params":{"threadId":"t","turnId":"turn","item":{"id":"core-finished","type":"agentMessage","text":"same"}}})).unwrap();
                write(&mut socket, json!({"method":"item/completed","params":{"threadId":"t","turnId":"turn","item":{"id":"core-finished","type":"agentMessage","text":"same"}}})).unwrap();
                if status == "completed" {
                    write(&mut socket, json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"turn","status":"completed","itemsView":"summary","items":[{"id":"core-summary","type":"agentMessage","text":"same"}]}}})).unwrap();
                } else {
                    write(&mut socket, json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"core-next","delta":"later suffix"}})).unwrap();
                }
                ready.send(()).unwrap();
                let independent = request(&mut socket);
                assert_eq!(independent["method"], "model/list");
                write(
                    &mut socket,
                    json!({"id":independent["id"],"result":{"data":[]}}),
                )
                .unwrap();
                write(&mut socket, json!({"id":read["id"],"result":fixture_read})).unwrap();
                let _ = socket.read();
            });
            let state = Arc::new(Mutex::new(State::default()));
            let (sender, receiver) = mpsc::channel();
            let worker_state = Arc::clone(&state);
            let worker = std::thread::spawn(move || {
                run(
                    endpoint.clone(),
                    NativeConnection::WebSocket {
                        endpoint,
                        token: ServerToken::fixture(),
                    },
                    worker_state,
                    receiver,
                )
            });
            await_epoch(&state, 1);
            let mut cached = canonical.clone();
            cached["thread"]["turns"][1]["status"] = json!("inProgress");
            cached["thread"]["turns"][1]["items"]
                .as_array_mut()
                .unwrap()
                .truncate(1);
            state.lock().unwrap().remember(&cached);
            let resume_sender = sender.clone();
            let resume = std::thread::spawn(move || {
                call(&resume_sender, "thread/resume", json!({"threadId":"t"}), 1)
            });
            wait_ready.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                call(&sender, "model/list", json!({}), 1).unwrap(),
                json!({"data":[]})
            );
            let result = resume.join().unwrap().unwrap();
            assert_eq!(result["thread"], canonical["thread"]);
            assert_eq!(result["model"], "resume-model");
            assert_eq!(result["reasoningEffort"], "high");
            let snapshot = state.lock().unwrap().snapshot("t", json!([]));
            assert_eq!(snapshot["thread"], canonical["thread"]);
            if status == "inProgress" {
                assert_eq!(
                    snapshot["provisionalItems"],
                    json!([
                        {"turnId":"turn","item":{"id":"core-next","type":"agentMessage","text":"later suffix","phase":null,"partialFromJoin":true}}
                    ])
                );
            } else {
                assert_eq!(snapshot["provisionalItems"], json!([]));
            }
            sender.send(Command::Stop).unwrap();
            worker.join().unwrap();
            fixture.join().unwrap();
        }
    }

    #[test]
    fn canonical_resume_read_failures_and_scope_archive_epoch_races_never_publish_legacy_ids() {
        for (mode, case) in ["paginated", "legacy"].into_iter().flat_map(|mode| {
            [
                "read-error",
                "wrong-id",
                "wrong-cwd",
                "relative-cwd",
                "wrong-mode",
                "unknown-mode",
                "missing-mode",
                "missing-active",
                "scope",
                "archive",
                "archive-unarchive",
                "epoch",
            ]
            .into_iter()
            .map(move |case| (mode, case))
        }) {
            let root = std::env::temp_dir().canonicalize().unwrap();
            let cwd = root.to_str().unwrap().to_owned();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let (ready, wait_ready) = mpsc::channel();
            let (release, wait_release) = mpsc::channel();
            let fixture_cwd = cwd.clone();
            let fixture = std::thread::spawn(move || {
                let mut socket = initialize(&listener);
                let resume = request(&mut socket);
                let mut resume_result = active_resume(&fixture_cwd);
                resume_result["thread"]["historyMode"] = json!(mode);
                write(
                    &mut socket,
                    json!({"id":resume["id"],"result":resume_result}),
                )
                .unwrap();
                let read = request(&mut socket);
                assert_eq!(read["method"], "thread/read");
                if case == "scope" {
                    write(&mut socket, json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"cwd":"/"}}})).unwrap();
                }
                if matches!(case, "archive" | "archive-unarchive") {
                    write(
                        &mut socket,
                        json!({"method":"thread/archived","params":{"threadId":"t"}}),
                    )
                    .unwrap();
                    if case == "archive-unarchive" {
                        write(
                            &mut socket,
                            json!({"method":"thread/unarchived","params":{"threadId":"t"}}),
                        )
                        .unwrap();
                    }
                }
                ready.send(()).unwrap();
                wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
                let mut result = canonical_read(&fixture_cwd);
                result["thread"]["historyMode"] = json!(mode);
                match case {
                    "wrong-id" => result["thread"]["id"] = json!("foreign"),
                    "wrong-cwd" => result["thread"]["cwd"] = json!("/"),
                    "relative-cwd" => result["thread"]["cwd"] = json!("relative"),
                    "wrong-mode" => {
                        result["thread"]["historyMode"] = json!(if mode == "legacy" {
                            "paginated"
                        } else {
                            "legacy"
                        })
                    }
                    "unknown-mode" => result["thread"]["historyMode"] = json!("future-mode"),
                    "missing-mode" => result["thread"]
                        .as_object_mut()
                        .unwrap()
                        .remove("historyMode")
                        .map(|_| ())
                        .unwrap(),
                    "missing-active" => result["thread"]["turns"] = json!([]),
                    _ => {}
                }
                let reply = if case == "read-error" {
                    json!({"id":read["id"],"error":{"code":-32001,"message":"canonical read unavailable"}})
                } else {
                    json!({"id":read["id"],"result":result})
                };
                write(&mut socket, reply).unwrap();
                let _ = socket.read();
            });
            let state = Arc::new(Mutex::new(State::connecting(
                vec![root],
                "fixture".to_owned(),
            )));
            let (sender, receiver) = mpsc::channel();
            let worker_state = Arc::clone(&state);
            let worker = std::thread::spawn(move || {
                run(
                    endpoint.clone(),
                    NativeConnection::WebSocket {
                        endpoint,
                        token: ServerToken::fixture(),
                    },
                    worker_state,
                    receiver,
                )
            });
            await_epoch(&state, 1);
            let resume_sender = sender.clone();
            let resume = std::thread::spawn(move || {
                call(&resume_sender, "thread/resume", json!({"threadId":"t"}), 1)
            });
            wait_ready.recv_timeout(Duration::from_secs(5)).unwrap();
            if case == "epoch" {
                state.lock().unwrap().epoch += 1;
            }
            release.send(()).unwrap();
            let error = resume.join().unwrap().unwrap_err();
            if case == "read-error" {
                assert_eq!(error.code(), "rejected");
                assert!(error
                    .message()
                    .contains("thread/resume canonical thread/read"));
                assert!(error.message().contains("canonical read unavailable"));
            } else if case == "epoch" {
                assert_eq!(error.code(), "unavailable");
            } else {
                assert_eq!(error.code(), "rejected", "{case}: {}", error.message());
            }
            let current = state.lock().unwrap();
            assert!(
                !current.threads.contains_key("t"),
                "{case} must not publish the unvalidated resume."
            );
            assert!(!current.settings.contains_key("t"));
            drop(current);
            sender.send(Command::Stop).unwrap();
            worker.join().unwrap();
            fixture.join().unwrap();
        }
    }

    #[test]
    fn recovery_waits_for_canonical_history_before_connected_and_keeps_pending_replay() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (ready, wait_ready) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let resume = request(&mut socket);
            assert_eq!(resume["method"], "thread/resume");
            write(
                &mut socket,
                json!({"id":resume["id"],"result":active_resume("/tmp")}),
            )
            .unwrap();
            let read = request(&mut socket);
            assert_eq!(read["method"], "thread/read");
            assert_eq!(read["params"]["includeTurns"], true);
            write(&mut socket, json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"turn","status":"inProgress","items":[]}}})).unwrap();
            write(&mut socket, json!({"method":"item/started","params":{"threadId":"t","turnId":"turn","item":{"id":"agent-a","type":"agentMessage","text":"same"}}})).unwrap();
            write(&mut socket, json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"agent-a","delta":" suffix"}})).unwrap();
            write(&mut socket, json!({"method":"item/completed","params":{"threadId":"t","turnId":"turn","item":{"id":"agent-b","type":"agentMessage","text":"same"}}})).unwrap();
            write(&mut socket, json!({"id":9,"method":"item/tool/requestUserInput","params":{"threadId":"t","questions":[{"id":"q"}]}})).unwrap();
            ready.send(()).unwrap();
            wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
            write(
                &mut socket,
                json!({"id":read["id"],"result":canonical_read("/tmp")}),
            )
            .unwrap();
            let next = request(&mut socket);
            assert_eq!(next["method"], "model/list");
            write(&mut socket, json!({"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"turn","itemId":"agent-a","delta":" tail"}})).unwrap();
            write(&mut socket, json!({"id":next["id"],"result":{"data":[]}})).unwrap();
            let _ = socket.read();
        });
        let mut initial = State::default();
        initial.remember(
            &json!({"thread":{"id":"t","cwd":"/tmp","historyMode":"paginated","turns":[]}}),
        );
        let state = Arc::new(Mutex::new(initial));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        wait_ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(state.lock().unwrap().connection != Connection::Connected);
        assert!(!state.lock().unwrap().threads["t"]
            .to_string()
            .contains("item-2"));
        release.send(()).unwrap();
        await_epoch(&state, 1);
        let snapshot = state.lock().unwrap().snapshot("t", json!([]));
        assert_eq!(snapshot["thread"]["turns"][0]["items"][1]["id"], "agent-a");
        assert_eq!(
            snapshot["thread"]["turns"][0]["items"][1]["text"],
            "same suffix"
        );
        assert_eq!(snapshot["thread"]["turns"][0]["items"][2]["id"], "agent-b");
        assert_eq!(snapshot["pending"][0]["id"], 9);
        assert_eq!(snapshot["settings"]["effort"], "high");
        call(&sender, "model/list", json!({}), 1).unwrap();
        assert_eq!(
            state.lock().unwrap().snapshot("t", json!([]))["thread"]["turns"][0]["items"][1]
                ["text"],
            "same suffix tail"
        );
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn restored_active_resume_hides_pending_controls_until_canonical_history_is_accepted() {
        for succeeds in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let (ready, wait_ready) = mpsc::channel();
            let (release, wait_release) = mpsc::channel();
            let fixture = std::thread::spawn(move || {
                let mut socket = initialize(&listener);
                let resume = request(&mut socket);
                write(&mut socket, json!({"id":9,"method":"item/tool/requestUserInput","params":{"threadId":"t","questions":[{"id":"q"}]}})).unwrap();
                write(
                    &mut socket,
                    json!({"id":resume["id"],"result":active_resume("/tmp")}),
                )
                .unwrap();
                let read = request(&mut socket);
                assert_eq!(read["method"], "thread/read");
                ready.send(()).unwrap();
                wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
                let result = if succeeds {
                    json!({"id":read["id"],"result":canonical_read("/tmp")})
                } else {
                    json!({"id":read["id"],"error":{"code":-32001,"message":"canonical history unavailable"}})
                };
                write(&mut socket, result).unwrap();
                let _ = socket.read();
            });
            let mut initial = State::default();
            initial.remember(&json!({"thread":{"id":"t","cwd":"/tmp","turns":[]}}));
            initial.reduce(&json!({"method":"thread/archived","params":{"threadId":"t"}}));
            let state = Arc::new(Mutex::new(initial));
            let (sender, receiver) = mpsc::channel();
            let worker_state = Arc::clone(&state);
            let worker = std::thread::spawn(move || {
                run(
                    endpoint.clone(),
                    NativeConnection::WebSocket {
                        endpoint,
                        token: ServerToken::fixture(),
                    },
                    worker_state,
                    receiver,
                )
            });
            await_epoch(&state, 1);
            let resume_sender = sender.clone();
            let resume = std::thread::spawn(move || {
                call(&resume_sender, "thread/resume", json!({"threadId":"t"}), 1)
            });
            wait_ready.recv_timeout(Duration::from_secs(5)).unwrap();
            let snapshot = state.lock().unwrap().snapshot("t", json!([]));
            assert_eq!(snapshot["accessMode"], "archived");
            assert_eq!(snapshot["pending"], json!([]));
            release.send(()).unwrap();
            assert_eq!(resume.join().unwrap().is_ok(), succeeds);
            let snapshot = state.lock().unwrap().snapshot("t", json!([]));
            if succeeds {
                assert_eq!(snapshot["accessMode"], "active");
                assert_eq!(snapshot["pending"][0]["id"], 9);
                assert_eq!(snapshot["thread"]["turns"][0]["items"][1]["id"], "agent-a");
            } else {
                assert_eq!(snapshot["accessMode"], "archived");
                assert_eq!(snapshot["pending"], json!([]));
                assert_eq!(snapshot["settings"], json!({}));
            }
            sender.send(Command::Stop).unwrap();
            worker.join().unwrap();
            fixture.join().unwrap();
        }
    }

    #[test]
    fn canonical_read_timeout_preserves_original_deadline_without_ingesting_legacy_history() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let resume = request(&mut socket);
            write(
                &mut socket,
                json!({"id":resume["id"],"result":active_resume("/tmp")}),
            )
            .unwrap();
            let read = request(&mut socket);
            assert_eq!(read["method"], "thread/read");
            let independent = request(&mut socket);
            assert_eq!(independent["method"], "model/list");
            // A late canonical result is no longer correlated after the original deadline.
            write(
                &mut socket,
                json!({"id":read["id"],"result":canonical_read("/tmp")}),
            )
            .unwrap();
            write(
                &mut socket,
                json!({"id":independent["id"],"result":{"data":[]}}),
            )
            .unwrap();
            let _ = socket.read();
        });
        let state = Arc::new(Mutex::new(State::default()));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        let (answer, result) = mpsc::channel();
        sender
            .send(Command::Call {
                method: "thread/resume".to_owned(),
                params: json!({"threadId":"t"}),
                epoch: 1,
                deadline: Instant::now() + Duration::from_millis(200),
                answer,
            })
            .unwrap();
        let error = result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), "uncertain");
        assert!(error.message().contains("canonical thread/read"));
        call(&sender, "model/list", json!({}), 1).unwrap();
        assert!(state.lock().unwrap().threads.is_empty());
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn completed_paginated_resume_keeps_native_history_without_an_extra_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let mut completed = active_resume("/tmp");
        completed["thread"]["turns"][0]["status"] = json!("completed");
        let expected = completed.clone();
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let resume = request(&mut socket);
            write(&mut socket, json!({"id":resume["id"],"result":completed})).unwrap();
            let next = request(&mut socket);
            assert_eq!(next["method"], "model/list");
            write(&mut socket, json!({"id":next["id"],"result":{"data":[]}})).unwrap();
            let _ = socket.read();
        });
        let state = Arc::new(Mutex::new(State::default()));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        assert_eq!(
            call(&sender, "thread/resume", json!({"threadId":"t"}), 1).unwrap(),
            expected
        );
        call(&sender, "model/list", json!({}), 1).unwrap();
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn disconnected_canonical_read_is_uncertain_and_recovery_does_not_replay_the_continuation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut first = initialize(&listener);
            let resume = request(&mut first);
            write(
                &mut first,
                json!({"id":resume["id"],"result":active_resume("/tmp")}),
            )
            .unwrap();
            assert_eq!(request(&mut first)["method"], "thread/read");
            drop(first);
            let mut second = initialize(&listener);
            let recovery = request(&mut second);
            assert_eq!(recovery["method"], "thread/resume");
            let mut recovered = canonical_read("/tmp");
            recovered["thread"]["turns"][0]["status"] = json!("completed");
            recovered["model"] = json!("recovered-model");
            recovered["reasoningEffort"] = json!("low");
            write(&mut second, json!({"id":recovery["id"],"result":recovered})).unwrap();
            let next = request(&mut second);
            assert_eq!(
                next["method"], "model/list",
                "The old canonical read must not be replayed on the replacement socket."
            );
            write(&mut second, json!({"id":next["id"],"result":{"data":[]}})).unwrap();
            let _ = second.read();
        });
        let state = Arc::new(Mutex::new(State::default()));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        let error = call(&sender, "thread/resume", json!({"threadId":"t"}), 1).unwrap_err();
        assert_eq!(error.code(), "uncertain");
        assert!(error.message().contains("canonical thread/read"));
        await_epoch(&state, 2);
        let snapshot = state.lock().unwrap().snapshot("t", json!([]));
        assert_eq!(snapshot["thread"]["turns"][0]["items"][1]["id"], "agent-a");
        assert_eq!(snapshot["settings"]["model"], "recovered-model");
        call(&sender, "model/list", json!({}), 2).unwrap();
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn dropped_reply_is_uncertain_and_reconnect_restores_subscriptions_without_replay() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut first = initialize(&listener);
            let action = request(&mut first);
            assert_eq!(action["method"], "turn/steer");
            // The server received the action; disconnect before returning its reply.
            drop(first);
            let mut second = initialize(&listener);
            let resume = request(&mut second);
            assert_eq!(resume["method"], "thread/resume");
            assert_eq!(resume["params"], json!({"threadId":"thread-fixture"}));
            write(&mut second, json!({"id":resume["id"],"result":{"thread":{"id":"thread-fixture","cwd":"/tmp","turns":[]},"model":"fixture","reasoningEffort":"high"}})).unwrap();
            let next = request(&mut second);
            assert_eq!(
                next["method"], "model/list",
                "a disconnected action must never be replayed"
            );
            write(&mut second, json!({"id":next["id"],"result":{"data":[]}})).unwrap();
            let _ = second.read();
        });
        let state = Arc::new(Mutex::new(State::default()));
        state
            .lock()
            .unwrap()
            .subscribed
            .insert("thread-fixture".to_owned());
        // First bootstrap would recover the subscription. Add it only after initialization.
        state.lock().unwrap().subscribed.clear();
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        state
            .lock()
            .unwrap()
            .subscribed
            .insert("thread-fixture".to_owned());
        assert!(matches!(
            call(
                &sender,
                "turn/steer",
                json!({"threadId":"thread-fixture"}),
                1
            ),
            Err(Failure::Uncertain(_))
        ));
        await_epoch(&state, 2);
        assert_eq!(
            state.lock().unwrap().settings["thread-fixture"]["effort"],
            "high"
        );
        assert!(matches!(
            call(&sender, "model/list", json!({}), 1),
            Err(Failure::Unavailable(_))
        ));
        assert_eq!(
            call(&sender, "model/list", json!({}), 2).unwrap(),
            json!({"data":[]})
        );
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn reconnect_preserves_selected_history_only_for_its_exact_native_archived_rejection() {
        let archived = "session t is archived. Run `codex unarchive t` to unarchive it first.";
        for (case, error) in [
            ("archived", json!({"code":-32600,"message":archived})),
            ("wrong-code", json!({"code":-32000,"message":archived})),
            (
                "foreign-id",
                json!({"code":-32600,"message":"session other is archived. Run `codex unarchive other` to unarchive it first."}),
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let fixture = std::thread::spawn(move || {
                let mut first = initialize(&listener);
                let disconnect = request(&mut first);
                assert_eq!(disconnect["method"], "model/list");
                drop(first);
                let mut second = initialize(&listener);
                let resume = request(&mut second);
                assert_eq!(resume["method"], "thread/resume");
                assert_eq!(resume["params"], json!({"threadId":"t"}));
                write(&mut second, json!({"id":resume["id"],"error":error})).unwrap();
                let next = request(&mut second);
                assert_eq!(
                    next["method"], "model/list",
                    "recovery must not send queue writes or automatically unarchive"
                );
                write(&mut second, json!({"id":next["id"],"result":{"data":[]}})).unwrap();
                let _ = second.read();
            });
            let state = Arc::new(Mutex::new(State::default()));
            let (_, events) = state.lock().unwrap().subscribe();
            let (sender, receiver) = mpsc::channel();
            let worker_state = Arc::clone(&state);
            let worker = std::thread::spawn(move || {
                run(
                    endpoint.clone(),
                    NativeConnection::WebSocket {
                        endpoint,
                        token: ServerToken::fixture(),
                    },
                    worker_state,
                    receiver,
                )
            });
            await_epoch(&state, 1);
            {
                let mut current = state.lock().unwrap();
                current.remember(&json!({"thread":{"id":"t","cwd":"/tmp","turns":[{"id":"old-turn","status":"completed","items":[]}]},"model":"previous","reasoningEffort":"high"}));
                current
                    .pending
                    .insert("old".to_owned(), json!({"params":{"threadId":"t"}}));
            }
            assert!(matches!(
                call(&sender, "model/list", json!({}), 1),
                Err(Failure::Uncertain(_))
            ));
            await_epoch(&state, 2);
            let snapshot = state
                .lock()
                .unwrap()
                .snapshot("t", json!([{"id":"old-queue"}]));
            if case == "archived" {
                assert_eq!(snapshot["accessMode"], "archived");
                assert_eq!(snapshot["thread"]["id"], "t");
                assert_eq!(snapshot["thread"]["turns"][0]["id"], "old-turn");
                assert_eq!(snapshot["settings"], json!({}));
                assert_eq!(snapshot["pending"], json!([]));
                assert_eq!(snapshot["queue"], json!([]));
                assert!(state.lock().unwrap().subscribed.contains("t"));
                for method in [
                    "thread/settings/update",
                    "thread/queue/add",
                    "thread/queue/start",
                    "thread/queue/delete",
                    "turn/steer",
                    "turn/interrupt",
                ] {
                    assert!(
                        matches!(
                            call(&sender, method, json!({"threadId":"t"}), 2),
                            Err(Failure::Rejected(_))
                        ),
                        "{method} must remain read-only after offline archive"
                    );
                }
            } else {
                assert!(
                    snapshot["thread"].is_null(),
                    "unrecognized native failures must still close selection"
                );
                assert!(!state.lock().unwrap().subscribed.contains("t"));
                assert!(!state.lock().unwrap().archived_threads.contains("t"));
            }
            let recovery_events: Vec<Value> = events.try_iter().collect();
            assert_eq!(
                recovery_events
                    .iter()
                    .any(|event| event["method"] == "gateway/snapshot-required"
                        && event["params"] == json!({"threadId":"t","accessMode":"archived"})),
                case == "archived"
            );
            assert_eq!(
                recovery_events
                    .iter()
                    .any(|event| event["method"] == "gateway/recoveryError"),
                case != "archived"
            );
            assert_eq!(
                call(&sender, "model/list", json!({}), 2).unwrap(),
                json!({"data":[]})
            );
            sender.send(Command::Stop).unwrap();
            worker.join().unwrap();
            fixture.join().unwrap();
        }
    }

    #[test]
    fn queued_start_is_fenced_when_execution_scope_changes_before_delivery() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let next = request(&mut socket);
            assert_eq!(
                next["method"], "model/list",
                "out-of-scope queue/start must never reach the native socket"
            );
            write(&mut socket, json!({"id":next["id"],"result":{"data":[]}})).unwrap();
            let _ = socket.read();
        });
        let root = std::env::temp_dir().canonicalize().unwrap();
        let state = Arc::new(Mutex::new(State::connecting(
            vec![root.clone()],
            "fixture".to_owned(),
        )));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        {
            let mut current = state.lock().unwrap();
            current.remember(&json!({"thread":{"id":"t","cwd":root,"turns":[]},"cwd":root}));
            current.reduce(&json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"cwd":"/"}}}));
            assert!(current.scope_closed("t"));
        }
        assert!(matches!(
            call(
                &sender,
                "thread/queue/start",
                json!({"threadId":"t","queuedSubmissionId":"queued"}),
                1
            ),
            Err(Failure::Rejected(_))
        ));
        assert_eq!(
            call(&sender, "model/list", json!({}), 1).unwrap(),
            json!({"data":[]})
        );
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn first_resume_keeps_pending_replay_and_later_effective_settings_in_socket_order() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let resume = request(&mut socket);
            assert_eq!(resume["method"], "thread/resume");
            // Pending replay may precede the resume response on the native socket.
            write(&mut socket, json!({"id":9,"method":"item/tool/requestUserInput","params":{"threadId":"t","questions":[{"id":"q"}]}})).unwrap();
            write(&mut socket, json!({"id":resume["id"],"result":{"thread":{"id":"t","cwd":"/tmp","turns":[]},"cwd":"/tmp","model":"one","reasoningEffort":"low"}})).unwrap();
            write(&mut socket, json!({"method":"thread/settings/updated","params":{"threadId":"t","threadSettings":{"model":"two","effort":"high","cwd":"/tmp"}}})).unwrap();
            let next = request(&mut socket);
            assert_eq!(next["method"], "model/list");
            write(&mut socket, json!({"id":next["id"],"result":{"data":[]}})).unwrap();
            let _ = socket.read();
        });
        let state = Arc::new(Mutex::new(State::default()));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        let response = call(&sender, "thread/resume", json!({"threadId":"t"}), 1).unwrap();
        assert_eq!(response["reasoningEffort"], "low");
        call(&sender, "model/list", json!({}), 1).unwrap();
        let snapshot = state.lock().unwrap().snapshot("t", json!([]));
        assert_eq!(snapshot["settings"]["model"], "two");
        assert_eq!(snapshot["settings"]["effort"], "high");
        assert_eq!(snapshot["pending"][0]["id"], 9);
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn archived_writers_are_fenced_and_reconnect_never_loads_the_archived_execution_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let restore = request(&mut socket);
            assert_eq!(
                restore["method"], "thread/unarchive",
                "read-only reconnect must not resume, respond or deliver a writer"
            );
            write(&mut socket, json!({"id":restore["id"],"result":{}})).unwrap();
            let _ = socket.read();
        });
        let mut initial = State::default();
        initial.remember(&json!({"thread":{"id":"t","cwd":"/tmp","turns":[]}}));
        initial.reduce(&json!({"method":"thread/archived","params":{"threadId":"t"}}));
        // A stale or concurrent caller must still be fenced at the socket boundary.
        initial.pending.insert(
            "9".to_owned(),
            json!({"canRespond":true,"params":{"threadId":"t"}}),
        );
        let state = Arc::new(Mutex::new(initial));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        for method in [
            "thread/settings/update",
            "thread/queue/add",
            "thread/queue/start",
            "thread/queue/delete",
            "turn/steer",
            "turn/interrupt",
            "thread/archive",
        ] {
            assert!(matches!(
                call(&sender, method, json!({"threadId":"t"}), 1),
                Err(Failure::Rejected(_))
            ));
        }
        state.lock().unwrap().pending.insert(
            "9".to_owned(),
            json!({"canRespond":true,"params":{"threadId":"t"}}),
        );
        assert!(matches!(
            respond(&sender, json!(9), json!({}), 1, "stale".to_owned()),
            Err(Failure::Rejected(_))
        ));
        assert_eq!(
            call(&sender, "thread/unarchive", json!({"threadId":"t"}), 1).unwrap(),
            json!({})
        );
        assert_eq!(
            state.lock().unwrap().snapshot("t", json!([]))["accessMode"],
            "archived"
        );
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn queued_approval_response_rechecks_the_exact_pending_request_before_socket_delivery() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let response = request(&mut socket);
            assert_eq!(
                response,
                json!({"id":7,"result":{"decision":"decline"}}),
                "Only the current request's answer may reach the native socket"
            );
            let next = request(&mut socket);
            assert_eq!(next["method"], "model/list");
            write(&mut socket, json!({"id":next["id"],"result":{"data":[]}})).unwrap();
            let _ = socket.read();
        });
        let state = Arc::new(Mutex::new(State::connecting(vec![], "instance".to_owned())));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        let (old_token, token) = {
            let mut current = state.lock().unwrap();
            current.remember(&json!({"thread":{"id":"t","cwd":"/tmp","turns":[]}}));
            current.reduce(&json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"t","command":"old command"}}));
            let old = current.pending["7"]["requestToken"]
                .as_str()
                .unwrap()
                .to_owned();
            current.reduce(&json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"t","command":"new command"}}));
            (
                old,
                current.pending["7"]["requestToken"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        };
        assert!(matches!(
            respond(
                &sender,
                json!(7),
                json!({"decision":"accept"}),
                1,
                old_token
            ),
            Err(Failure::Rejected(_))
        ));
        assert_eq!(
            respond(&sender, json!(7), json!({"decision":"decline"}), 1, token).unwrap(),
            json!({"delivered":true})
        );
        assert_eq!(
            call(&sender, "model/list", json!({}), 1).unwrap(),
            json!({"data":[]})
        );
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }

    #[test]
    fn restored_native_resume_replays_pending_approval_before_enabling_browser_writers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (replayed, wait_replay) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let fixture = std::thread::spawn(move || {
            let mut socket = initialize(&listener);
            let resume = request(&mut socket);
            assert_eq!(resume["method"], "thread/resume");
            write(&mut socket, json!({"id":9,"method":"item/tool/requestUserInput","params":{"threadId":"t","questions":[{"id":"q"}]}})).unwrap();
            replayed.send(()).unwrap();
            wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
            write(&mut socket, json!({"id":resume["id"],"result":{"thread":{"id":"t","cwd":"/tmp","turns":[]},"model":"native-restored","reasoningEffort":"high"}})).unwrap();
            let _ = socket.read();
        });
        let mut initial = State::default();
        initial.remember(&json!({"thread":{"id":"t","cwd":"/tmp","turns":[]}}));
        initial.reduce(&json!({"method":"thread/archived","params":{"threadId":"t"}}));
        let state = Arc::new(Mutex::new(initial));
        let (sender, receiver) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            run(
                endpoint.clone(),
                NativeConnection::WebSocket {
                    endpoint,
                    token: ServerToken::fixture(),
                },
                worker_state,
                receiver,
            )
        });
        await_epoch(&state, 1);
        let resume_sender = sender.clone();
        let resume = std::thread::spawn(move || {
            call(&resume_sender, "thread/resume", json!({"threadId":"t"}), 1)
        });
        wait_replay.recv_timeout(Duration::from_secs(5)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.lock().unwrap().pending.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let snapshot = state.lock().unwrap().snapshot("t", json!([]));
        assert_eq!(snapshot["accessMode"], "archived");
        assert_eq!(snapshot["pending"], json!([]));
        assert!(matches!(
            respond(&sender, json!(9), json!({}), 1, "stale".to_owned()),
            Err(Failure::Rejected(_))
        ));
        release.send(()).unwrap();
        resume.join().unwrap().unwrap();
        let active = state.lock().unwrap().snapshot("t", json!([]));
        assert_eq!(active["accessMode"], "active");
        assert_eq!(active["settings"]["model"], "native-restored");
        assert_eq!(active["pending"][0]["id"], 9);
        sender.send(Command::Stop).unwrap();
        worker.join().unwrap();
        fixture.join().unwrap();
    }
    #[test]
    fn open_servers_are_refused_before_initialize_and_handshake_errors_do_not_echo_secrets() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            assert!(matches!(socket.read(), Ok(Message::Close(_))));
        });
        let error = socket(&endpoint, &ServerToken::fixture()).err().unwrap();
        assert!(error.contains("unauthenticated"));
        fixture.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let fixture = std::thread::spawn(move || {
            use std::io::{Read, Write};
            for status in [401, 403] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                assert_eq!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer"),
                    status == 403
                );
                let secret = "a".repeat(64);
                let response = format!("HTTP/1.1 {status} Refused\r\nX-Echo: {secret}\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{secret}");
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let error = socket(&endpoint, &ServerToken::fixture()).err().unwrap();
        assert!(error.contains("403"));
        assert!(!error.contains(&"a".repeat(64)));
        fixture.join().unwrap();
    }

    /// #s240: after an owner refusal a command is answered as unavailable and the wait goes on
    /// (the Stop sent after it is still read); after any other failure the first command ends the
    /// short wait at once, as it always has.
    #[test]
    fn an_owner_refusal_keeps_waiting_through_commands_and_other_failures_do_not() {
        let call = || {
            let (answer, answered) = mpsc::channel();
            let command = Command::Call {
                method: "thread/list".into(),
                params: json!({}),
                epoch: 0,
                deadline: Instant::now() + Duration::from_secs(5),
                answer,
            };
            (command, answered)
        };
        let refused = super::super::transport::credential_may_go_to(None).unwrap_err();
        let (sender, receiver) = mpsc::channel();
        let (command, answered) = call();
        sender.send(command).unwrap();
        sender.send(Command::Stop).unwrap();
        assert!(
            !wait_to_retry(&receiver, &refused),
            "the Stop behind the command was read"
        );
        assert!(matches!(
            answered.try_recv(),
            Ok(Err(Failure::Unavailable(_)))
        ));

        // A Stop that arrives during the long pause ends it at once, not after 15 s.
        let (sender, receiver) = mpsc::channel();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            sender.send(Command::Stop).unwrap();
        });
        let started = Instant::now();
        assert!(!wait_to_retry(&receiver, &refused));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a Stop mid-pause is honoured at once: {:?}",
            started.elapsed()
        );
        stopper.join().unwrap();

        let (sender, receiver) = mpsc::channel();
        let (command, answered) = call();
        sender.send(command).unwrap();
        sender.send(Command::Stop).unwrap();
        assert!(wait_to_retry(&receiver, "Connection refused (os error 61)"));
        assert!(matches!(
            answered.try_recv(),
            Ok(Err(Failure::Unavailable(_)))
        ));
        assert!(
            matches!(receiver.try_recv(), Ok(Command::Stop)),
            "the short wait ended at the command"
        );
    }

    /// #s145: a squatter of another account answers the probe with 401 as the real server does.
    /// The credential's connection is then refused before a byte is written, whether its server
    /// end is another user's or its owner cannot be told.
    #[cfg(unix)]
    #[test]
    fn a_server_of_another_user_or_of_no_known_user_never_receives_the_credential() {
        let another: ServerOwner = |_| Some(unsafe { libc::geteuid() } + 1);
        let unknown: ServerOwner = |_| None;
        for (owner, refusal) in [(another, "another OS user"), (unknown, "Could not verify")] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let fixture = std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut requests = Vec::new();
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut request = [0_u8; 4096];
                    let count = stream.read(&mut request).unwrap_or(0);
                    requests.push(String::from_utf8_lossy(&request[..count]).to_ascii_lowercase());
                    let _ = stream.write_all(
                        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                requests
            });
            let error = socket_owned_by(&endpoint, &ServerToken::fixture(), owner)
                .err()
                .unwrap();
            assert!(error.contains(refusal), "{error}");
            assert!(error.contains("was not sent"), "{error}");
            let requests = fixture.join().unwrap();
            assert!(
                requests[0].starts_with("get "),
                "the probe went out: {requests:?}"
            );
            assert!(
                requests.iter().all(|r| !r.contains("authorization")),
                "the credential reached a server that is not this user's: {requests:?}"
            );
            assert_eq!(
                requests[1], "",
                "nothing was written on the refused connection"
            );
        }
    }
}
