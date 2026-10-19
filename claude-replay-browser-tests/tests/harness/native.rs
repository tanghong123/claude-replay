//! A protocol fixture, never an LLM session. The real gateway owns its WebSocket client.
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::os::unix::{
    fs::{symlink, DirBuilderExt, PermissionsExt},
    net::{UnixListener, UnixStream},
};

trait FixtureStream: Read + Write + Send {
    fn timeout(&self, value: Duration) -> io::Result<()>;
}
impl FixtureStream for std::net::TcpStream {
    fn timeout(&self, value: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(value))
    }
}
impl FixtureStream for UnixStream {
    fn timeout(&self, value: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(value))
    }
}
enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}
impl Listener {
    fn accept(&self) -> io::Result<Box<dyn FixtureStream>> {
        match self {
            Self::Tcp(listener) => listener
                .accept()
                .map(|(s, _)| Box::new(s) as Box<dyn FixtureStream>),
            Self::Unix(listener) => listener
                .accept()
                .map(|(s, _)| Box::new(s) as Box<dyn FixtureStream>),
        }
    }
}

use std::{
    net::TcpListener,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

pub struct NativeServer {
    pub endpoint: String,
    pub calls: Arc<Mutex<Vec<String>>>,
    pub active: Arc<AtomicBool>,
    pub occupied: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    socket_directory: Option<std::path::PathBuf>,
}
impl NativeServer {
    pub fn start(root: &Path, state: &Path, rollout: &Path) -> Self {
        Self::start_transport(root, state, rollout, false)
    }
    pub fn start_transport(root: &Path, state: &Path, rollout: &Path, unix: bool) -> Self {
        let (listener, endpoint, socket_directory) = if unix {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let directory = std::path::PathBuf::from("/tmp").join(format!(
                "am-browser-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            let path = directory.join("rpc.sock");
            let listener = UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let alias = root.join("native-control.sock");
            symlink(&path, &alias).unwrap();
            (
                Listener::Unix(listener),
                format!("unix://{}", alias.display()),
                Some(directory),
            )
        } else {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            (Listener::Tcp(listener), endpoint, None)
        };
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicBool::new(false));
        let quit = stop.clone();
        let log = calls.clone();
        let busy = active.clone();
        let occupied = Arc::new(AtomicBool::new(false));
        let owned = occupied.clone();
        let created = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicBool::new(false));
        let root = root.to_owned();
        let rollout = rollout.to_owned();
        let token_path = state.join("codex-server.token");
        let worker = thread::spawn(move || {
            let mut clients = Vec::new();
            while !quit.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok(stream) => {
                        let quit = quit.clone();
                        let log = log.clone();
                        let busy = busy.clone();
                        let owned = owned.clone();
                        let created = created.clone();
                        let sent = sent.clone();
                        let root = root.clone();
                        let rollout = rollout.clone();
                        let token_path = token_path.clone();
                        clients.push(thread::spawn(move || {
                            // Availability probes may close before the fixture accepts them.
                            if stream.timeout(Duration::from_secs(2)).is_err() { return; }
                            // Tungstenite requires its HTTP rejection response by value.
                            #[allow(clippy::result_large_err)]
                            let Ok(mut socket) = tungstenite::accept_hdr(stream, |request: &tungstenite::handshake::server::Request, response| {
                                let token = std::fs::read_to_string(&token_path).ok();
                                let authorized = if unix { request.headers().get("Authorization").is_none() } else { token.is_some_and(|token| request.headers().get("Authorization").and_then(|v| v.to_str().ok()) == Some(format!("Bearer {}", token.trim()).as_str())) };
                                if authorized { Ok(response) } else { Err(tungstenite::http::Response::builder().status(401).body(Some("Authentication required".into())).unwrap()) }
                            }) else { return; };
                            if socket.get_ref().timeout(Duration::from_millis(150)).is_err() { return; }
                            while !quit.load(Ordering::SeqCst) {
                                let message = match socket.read() {
                                    Ok(message) => message,
                                    Err(tungstenite::Error::Io(error)) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                                    Err(_) => break,
                                };
                                if message.is_close() { break; }
                                if !message.is_text() { continue; }
                                let Ok(text) = message.to_text() else { continue; };
                                let request: Value = serde_json::from_str(text).unwrap();
                                let Some(method) = request["method"].as_str() else { continue; };
                                log.lock().unwrap().push(method.to_owned());
                                if request.get("id").is_none() { continue; }
                                if method == "thread/resume" && owned.load(Ordering::SeqCst) {
                                    let error = json!({"id":request["id"],"error":{"code":-32600,"message":"thread native-owned already has an active writer"}});
                                    if socket.send(tungstenite::Message::Text(error.to_string().into())).is_err() { break; }
                                    continue;
                                }
                                let status = if busy.load(Ordering::SeqCst) { "active" } else { "idle" };
                                let thread = json!({"id":"native-owned","cwd":root,"preview":"Native review fixture","createdAt":1,"updatedAt":2,"status":{"type":status},"turns":[],"path":rollout});
                                let created_path = rollout.with_file_name("rollout-native-created.jsonl");
                                let turns = if sent.load(Ordering::SeqCst) { json!([{"id":"first-turn","status":"completed","items":[]}]) } else { json!([]) };
                                let new_thread = json!({"id":"native-created","cwd":root,"preview":"New synthetic session","createdAt":3,"updatedAt":4,"status":{"type":"idle"},"turns":turns,"path":created_path});
                                let selected = if request["params"]["threadId"] == "native-created" || method == "thread/start" { new_thread.clone() } else { thread.clone() };
                                if method == "thread/queue/add" {
                                    sent.store(true, Ordering::SeqCst);
                                    socket.send(tungstenite::Message::Text(json!({"method":"turn/started","params":{"threadId":"native-created","turn":{"id":"first-turn","status":"inProgress","items":[]}}}).to_string().into())).unwrap();
                                }
                                let result = match method {
                                    "initialize" => json!({"userAgent":"native-browser-fixture"}),
                                    "model/list" => json!({"data":[{"id":"fixture-model","model":"fixture-model","displayName":"Fixture model","isDefault":true,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}],"serviceTiers":[{"id":"default","name":"Standard"},{"id":"fast","name":"Fast"}]}],"nextCursor":null}),
                                    "project/list" => json!({"data":[{"id":"fixture-project","name":"Fixture project","roots":[{"path":root}]}],"nextCursor":null}),
                                    "project/create" => json!({"project":{"id":"added-project","name":request["params"]["name"],"roots":request["params"]["roots"]}}),
                                    "project/read" => json!({"project":{"id":"fixture-project","name":"Fixture project","roots":[{"path":root}]}}),
                                    "thread/list" => json!({"data":if created.load(Ordering::SeqCst) { vec![thread, new_thread] } else { vec![thread] },"nextCursor":null}),
                                    "thread/read" => json!({"thread":selected}),
                                    "thread/start" => { created.store(true, Ordering::SeqCst); json!({"thread":selected,"cwd":root,"model":"fixture-model","reasoningEffort":"low","serviceTier":null}) },
                                    "thread/resume" => json!({"thread":selected,"cwd":root,"model":"fixture-model","reasoningEffort":"low","serviceTier":null}),
                                    "thread/queue/add" => json!({"queuedSubmission":{"id":"submission","clientUserMessageId":request["params"]["clientUserMessageId"],"input":request["params"]["input"]}}),
                                    "thread/queue/start" => json!({}),
                                    "thread/unsubscribe" => json!({}),
                                    "thread/queue/list" => json!({"data":[],"nextCursor":null}),
                                    _ => { let _ = socket.send(tungstenite::Message::Text(json!({"id":request["id"],"error":{"code":-32601,"message":format!("Unsupported fixture method {method}")}}).to_string().into())); continue; }
                                };
                                if socket.send(tungstenite::Message::Text(json!({"id":request["id"],"result":result}).to_string().into())).is_err() { break; }
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("accept native fixture: {error}"),
                }
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        Self {
            endpoint,
            calls,
            active,
            occupied,
            stop,
            worker: Some(worker),
            socket_directory,
        }
    }
}
impl Drop for NativeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
        if let Some(directory) = &self.socket_directory {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
