//! Explicit activation of a shared Codex runtime. Construction and status never spawn work.

use crate::{auth::ServerToken, transport::Endpoint, validate_state_path, Config, Gateway};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Host configuration is trusted CLI input; browsers cannot replace binary, home or scope.
pub struct RuntimeConfig {
    pub gateway: Config,
    pub binary: PathBuf,
    pub home: PathBuf,
    pub log_path: PathBuf,
}

/// A lazy client/lifecycle boundary. Only `activate` may start a process or RPC worker.
pub struct NativeRuntime {
    config: RuntimeConfig,
    endpoint: Endpoint,
    token_file: Option<PathBuf>,
    addresses: Vec<SocketAddr>,
    state: Mutex<RuntimeState>,
    transition: Mutex<()>,
}

#[derive(Default)]
struct RuntimeState {
    gateway: Option<Arc<Gateway>>,
    child: Option<Child>,
    last_error: Option<String>,
    shutdown_warnings: Vec<String>,
}

impl NativeRuntime {
    /// Validate inert configuration, including the absolute receipt database path.
    /// This opens no connection, directory, database or process.
    pub fn new(mut config: RuntimeConfig) -> Result<Arc<Self>> {
        validate_state_path(&config.gateway.state_path)?;
        if config.gateway.endpoint == "unix://" {
            config.gateway.endpoint = format!(
                "unix://{}",
                config
                    .home
                    .join("app-server-control/app-server-control.sock")
                    .display()
            );
        }
        let endpoint = Endpoint::parse(&config.gateway.endpoint)?;
        let addresses = match &endpoint {
            Endpoint::PrivateUnix(_) => Vec::new(),
            Endpoint::AuthenticatedWebSocket(value) => {
                let uri: tungstenite::http::Uri = value.parse()?;
                let authority = uri.authority().context("Codex endpoint has no authority")?;
                let addresses: Vec<_> = authority
                    .as_str()
                    .to_socket_addrs()
                    .context("Resolve configured loopback Codex address")?
                    .filter(|address| address.ip().is_loopback())
                    .collect();
                if addresses.is_empty() {
                    bail!("Codex address did not resolve to loopback");
                }
                if uri.port_u16().is_none_or(|port| port == 0) {
                    bail!("Native Codex endpoint needs an explicit nonzero port");
                }
                if uri.path() != "/" {
                    bail!("Native Codex endpoint cannot include a path");
                }
                addresses
            }
        };
        if !config.home.is_absolute() || !config.log_path.is_absolute() {
            bail!("Native Codex home and log path must be absolute");
        }
        if config.gateway.allowed_roots.is_empty() {
            bail!("Native Codex mode needs an allowed project root");
        }
        let token_file = if endpoint.is_unix() {
            None
        } else {
            Some(ServerToken::path(&config.gateway.state_path)?)
        };
        Ok(Arc::new(Self {
            config,
            endpoint,
            token_file,
            addresses,
            state: Mutex::new(RuntimeState::default()),
            transition: Mutex::new(()),
        }))
    }

    /// Configuration identity used to reject handoff to a differently configured Monitor.
    pub fn identity(&self) -> Value {
        json!({"endpoint":self.config.gateway.endpoint,"home":self.config.home,
            "allowedRoots":self.config.gateway.allowed_roots,"binary":self.config.binary})
    }

    /// Return the activated client without implicitly creating it.
    pub fn gateway(&self) -> Option<Arc<Gateway>> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .gateway
            .clone()
    }

    /// Observe a listener and current client state. A connection probe is not an RPC handshake.
    pub fn status(&self) -> Value {
        let listening = self.listening();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(child) = state.child.as_mut() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    state.last_error =
                        Some(format!("Monitor-started Codex server exited with {exit}"));
                    state.child = None;
                }
                Ok(None) => {}
                Err(error) => {
                    state.last_error =
                        Some(format!("Inspect Monitor-started Codex server: {error}"))
                }
            }
        }
        let mut result = state.gateway.as_ref().map(|gateway| gateway.status()).unwrap_or_else(||
            json!({"ok":true,"connection":"disconnected","epoch":0,"allowedRoots":self.config.gateway.allowed_roots}));
        result["ok"] = json!(true);
        result["runtime"] = json!(if state.gateway.is_some() {
            "activated"
        } else if listening {
            "available"
        } else {
            "stopped"
        });
        result["serverListening"] = json!(listening);
        result["endpoint"] = json!(self.config.gateway.endpoint);
        result["codexHome"] = json!(self.config.home);
        result["transport"] = json!(if self.endpoint.is_unix() {
            "unix"
        } else {
            "websocket"
        });
        result["socketPath"] = json!(self.endpoint.socket_path());
        result["authentication"] = match &self.endpoint {
            Endpoint::PrivateUnix(_) => json!({"type":"unix-permissions"}),
            Endpoint::AuthenticatedWebSocket(_) => {
                json!({"type":"bearer","tokenFile":self.token_file})
            }
        };
        result["homeProvenance"] = json!(if state.child.is_some() {
            "monitor-started"
        } else {
            "configured"
        });
        result["ownedServerPid"] = json!(state.child.as_ref().map(Child::id));
        result["lastStartupError"] = json!(state.last_error);
        result["shutdownWarnings"] = json!(state.shutdown_warnings);
        result
    }

    fn listening(&self) -> bool {
        #[cfg(unix)]
        if let Endpoint::PrivateUnix(path) = &self.endpoint {
            return crate::transport::connect_unix(path).is_ok();
        }
        self.addresses
            .iter()
            .any(|address| TcpStream::connect_timeout(address, Duration::from_millis(150)).is_ok())
    }

    /// Explicitly connect, starting a server only when requested and the endpoint is absent.
    /// Callers must enforce authenticated POST and same-origin checks before this method.
    /// Serialized activation/retry never creates a second worker or process. A successfully
    /// started server is stopped when this runtime's native mode is switched off.
    pub fn activate(&self, start_if_missing: bool) -> Result<Value> {
        let result = self.activate_inner(start_if_missing);
        if let Err(error) = &result {
            self.state
                .lock()
                .unwrap_or_else(|cause| cause.into_inner())
                .last_error = Some(format!("{error:#}"));
        }
        result?;
        Ok(self.status())
    }

    /// Close the gateway and stop/reap only this runtime's owned server. The HTTP caller must
    /// enforce pairing, POST and Origin before invoking this process-level method.
    pub fn deactivate(&self) -> std::result::Result<Value, crate::DisconnectError> {
        let _transition = self.transition.try_lock().map_err(|_| {
            crate::DisconnectError::Busy("A native lifecycle transition is already running".into())
        })?;
        let closed = self.gateway().map(|gateway| gateway.close()).transpose();
        // A subscription failure must not strand an owned server. Independently
        // started servers have no Child handle here and are never signalled.
        let mut owned = StartupChild(
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .child
                .take(),
        );
        let stopped = owned.stop_gracefully();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.child = owned.0.take();
        if let Ok(warnings) = &closed {
            state.gateway = None;
            state.shutdown_warnings = warnings.clone().unwrap_or_default();
        }
        drop(state);
        match (closed, stopped) {
            (Err(close), Err(stop)) => {
                return Err(crate::DisconnectError::Worker(format!(
                    "{close}; stopping the Monitor-owned Codex server also failed: {stop:#}"
                )))
            }
            (Err(close), Ok(())) => return Err(close),
            (Ok(_), Err(stop)) => {
                return Err(crate::DisconnectError::Worker(format!(
                    "Stop Monitor-owned Codex server: {stop:#}"
                )))
            }
            (Ok(_), Ok(())) => (),
        }
        Ok(self.status())
    }

    /// Explicit recovery when the server/project has disappeared. Never interrupts work
    /// or kills a server; refuses known active turns and pending native requests.
    /// The caller must apply the same gate as `deactivate` and explain uncertain delivery.
    pub fn force_deactivate(&self) -> std::result::Result<Value, crate::DisconnectError> {
        self.deactivate_inner(true)
    }

    fn deactivate_inner(&self, force: bool) -> std::result::Result<Value, crate::DisconnectError> {
        let _transition = self.transition.try_lock().map_err(|_| {
            crate::DisconnectError::Busy("A native lifecycle transition is already running".into())
        })?;
        if let Some(gateway) = self.gateway() {
            if force {
                gateway.force_disconnect()?;
            } else {
                gateway.disconnect()?;
            }
            self.state.lock().unwrap_or_else(|e| e.into_inner()).gateway = None;
        }
        Ok(self.status())
    }

    fn activate_inner(&self, start_if_missing: bool) -> Result<()> {
        let _transition = self
            .transition
            .try_lock()
            .map_err(|_| anyhow::anyhow!("A native lifecycle transition is already running"))?;
        if self.gateway().is_some() {
            return Ok(());
        }
        let listening = self.listening();
        if !listening && !start_if_missing {
            bail!("Codex server is not running. Choose Start and enable native mode.");
        }
        for root in &self.config.gateway.allowed_roots {
            if !root
                .canonicalize()
                .with_context(|| format!("Resolve native project root {}", root.display()))?
                .is_dir()
            {
                bail!("Native project root {} is not a directory", root.display());
            }
        }
        #[cfg(unix)]
        if let Endpoint::PrivateUnix(path) = &self.endpoint {
            crate::transport::check_start_path(path)?;
        }
        let connection = crate::client::NativeConnection::configured(
            &self.config.gateway.endpoint,
            &self.config.gateway.state_path,
        )?;
        let mut startup = StartupChild(None);
        if !listening {
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(child) = state.child.as_mut() {
                    if child.try_wait()?.is_none() {
                        bail!("The requested Codex server is still starting. Retry after it binds the configured endpoint.");
                    }
                    state.child = None;
                }
            }
            startup.0 = Some(self.spawn_server()?);
            let child = startup.0.as_mut().expect("owned startup child");
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(exit) = child.try_wait()? {
                    bail!(
                        "Codex server exited with {exit}; inspect {}",
                        self.config.log_path.display()
                    );
                }
                if self.listening() {
                    break;
                }
                if Instant::now() >= deadline {
                    bail!(
                        "Codex server did not bind {} within 10 seconds; inspect {}",
                        self.config.gateway.endpoint,
                        self.config.log_path.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let activated = (|| -> Result<Arc<Gateway>> {
            connection.verify().map_err(anyhow::Error::msg)?;
            Gateway::start(Config {
                endpoint: self.config.gateway.endpoint.clone(),
                allowed_roots: self.config.gateway.allowed_roots.clone(),
                state_path: self.config.gateway.state_path.clone(),
            })
            .context("Activate native Codex Gateway")
        })();
        let gateway = match activated {
            Ok(gateway) => gateway,
            Err(error) => {
                startup
                    .stop()
                    .context("Clean up newly started Codex process group")?;
                return Err(error);
            }
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.gateway = Some(gateway);
        if startup.0.is_some() {
            state.child = startup.0.take();
        }
        state.last_error = None;
        state.shutdown_warnings.clear();
        Ok(())
    }

    fn spawn_server(&self) -> Result<Child> {
        let parent = self
            .config
            .log_path
            .parent()
            .context("Codex log has no parent directory")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Create Codex log directory {}", parent.display()))?;
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            let metadata = std::fs::metadata(parent)?;
            // This directory contains execution logs, so another user must not own it.
            if metadata.uid() != unsafe { libc::geteuid() } {
                bail!("Codex log directory is not owned by this user");
            }
        }
        let log = options
            .open(&self.config.log_path)
            .with_context(|| format!("Open native Codex log {}", self.config.log_path.display()))?;
        let metadata = log.metadata()?;
        if !metadata.is_file() {
            bail!("Native Codex log must be a regular file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                bail!("Native Codex log must be an owner-only file");
            }
        }
        let mut command = self.server_command()?;
        command
            .env("CODEX_HOME", &self.config.home)
            .env("CODEX_APP_SERVER_FORCE_CLI", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // The authenticated server outlives Monitor; failed startup owns the whole group.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        command.spawn().with_context(|| {
            format!(
                "Start {} app-server at {}",
                self.config.binary.display(),
                self.config.gateway.endpoint
            )
        })
    }

    fn server_command(&self) -> Result<Command> {
        let mut command = Command::new(&self.config.binary);
        command.args(["app-server", "--listen", &self.config.gateway.endpoint]);
        if !self.endpoint.is_unix() {
            command
                .args(["--ws-auth", "capability-token", "--ws-token-file"])
                .arg(ServerToken::path(&self.config.gateway.state_path)?);
        }
        command
            .args(["-c", "service_tier=\"default\""])
            .current_dir(&self.config.gateway.allowed_roots[0])
            .env_clear();
        inherit_server_environment(&mut command, std::env::vars_os());
        Ok(command)
    }
}

/// Preserve host execution context without inheriting Monitor/session/agent overrides.
fn inherit_server_environment(
    command: &mut Command,
    environment: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) {
    const ALLOWED: &[&str] = &[
        "HOME",
        "USER",
        "LOGNAME",
        "PATH",
        "SSH_AUTH_SOCK",
        "SHELL",
        "TERM",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "OPENAI_API_KEY",
    ];
    for (name, value) in environment {
        if name.to_str().is_some_and(|name| ALLOWED.contains(&name)) {
            command.env(name, value);
        }
    }
}

/// Only a child spawned by this activation may be reaped on failure.
struct StartupChild(Option<Child>);

impl StartupChild {
    /// TERM first, then a bounded KILL escalation for the entire owned group.
    fn stop_gracefully(&mut self) -> Result<()> {
        let Some(child) = self.0.as_mut() else {
            return Ok(());
        };
        #[cfg(unix)]
        {
            let group = -(child.id() as i32);
            for signal in [libc::SIGTERM, libc::SIGKILL] {
                if unsafe { libc::kill(group, signal) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error).context("Signal owned Codex process group");
                    }
                }
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline {
                    let exited = child
                        .try_wait()
                        .context("Reap owned Codex server")?
                        .is_some();
                    let absent = unsafe { libc::kill(group, 0) } != 0
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                    if exited && absent {
                        self.0 = None;
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            bail!("Owned Codex process group did not exit after TERM and KILL");
        }
        #[cfg(not(unix))]
        {
            child.kill().context("Stop owned Codex server")?;
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if child.try_wait()?.is_some() {
                    self.0 = None;
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            bail!("Owned Codex server did not exit");
        }
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.0.take() {
            #[cfg(unix)]
            {
                // spawn_server's setsid makes pid the group ID, including a launcher's children.
                let result = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                if result != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        self.0 = Some(child);
                        return Err(error).context("Kill owned startup process group");
                    }
                }
            }
            #[cfg(not(unix))]
            child.kill().context("Kill owned startup process")?;
            child.wait().context("Reap owned startup process")?;
        }
        Ok(())
    }
}

impl Drop for StartupChild {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("Clean up owned Codex startup process: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, Arc<NativeRuntime>) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "native-lifecycle-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let runtime = NativeRuntime::new(RuntimeConfig {
            gateway: Config {
                endpoint: format!("ws://127.0.0.1:{port}"),
                allowed_roots: vec![root.clone()],
                state_path: root.join("receipts.sqlite3"),
            },
            binary: root.join("missing-codex"),
            home: root.join("codex-home"),
            log_path: root.join("native.log"),
        })
        .unwrap();
        (root, runtime)
    }

    #[test]
    fn construction_and_status_never_create_native_state_or_a_process() {
        let (root, runtime) = fixture();
        for _ in 0..3 {
            let status = runtime.status();
            assert_eq!(status["ok"], true);
            assert_eq!(status["runtime"], "stopped");
            assert_eq!(status["serverListening"], false);
            assert!(status["ownedServerPid"].is_null());
            assert!(runtime.gateway().is_none());
        }
        assert!(!root.join("receipts.sqlite3").exists());
        assert!(!root.join("codex-home").exists());
        assert!(!root.join("native.log").exists());
        crate::remove_test_scratch(&root);
    }

    #[cfg(unix)]
    #[test]
    fn unix_configuration_resolves_home_without_creating_a_token_or_server() {
        let (root, original) = fixture();
        let runtime = NativeRuntime::new(RuntimeConfig {
            gateway: Config {
                endpoint: "unix://".into(),
                allowed_roots: vec![root.clone()],
                state_path: root.join("unix-receipts.sqlite3"),
            },
            binary: original.config.binary.clone(),
            home: root.join("codex-home"),
            log_path: root.join("unix.log"),
        })
        .unwrap();
        let status = runtime.status();
        assert_eq!(status["transport"], "unix");
        assert_eq!(status["authentication"], json!({"type":"unix-permissions"}));
        assert_eq!(
            status["socketPath"],
            json!(root.join("codex-home/app-server-control/app-server-control.sock"))
        );
        assert_eq!(status["runtime"], "stopped");
        assert!(runtime
            .activate(false)
            .unwrap_err()
            .to_string()
            .contains("not running"));
        let command = runtime.server_command().unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|value| value.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "app-server");
        assert!(args[2].starts_with("unix:///"));
        assert!(!args.iter().any(|a| a.starts_with("--ws-")));
        assert!(args.iter().any(|a| a == "service_tier=\"default\""));
        assert!(!root.join("codex-server.token").exists());
        assert!(!root.join("codex-home").exists());
        assert!(!root.join("unix-receipts.sqlite3").exists());
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn connect_only_refuses_absent_server_without_spawning_or_creating_state() {
        let (root, runtime) = fixture();
        let error = runtime.activate(false).unwrap_err();
        assert!(error.to_string().contains("not running"));
        assert!(runtime.gateway().is_none());
        assert!(!root.join("receipts.sqlite3").exists());
        assert!(!root.join("native.log").exists());
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn failed_explicit_start_retains_its_binary_cause_without_activating_gateway() {
        let (root, runtime) = fixture();
        let error = runtime.activate(true).unwrap_err();
        assert!(format!("{error:#}").contains("missing-codex"));
        assert!(runtime.gateway().is_none());
        assert!(runtime.status()["ownedServerPid"].is_null());
        assert!(!root.join("receipts.sqlite3").exists());
        assert!(runtime.status()["lastStartupError"]
            .as_str()
            .unwrap()
            .contains("missing-codex"));
        let token =
            std::fs::read_to_string(ServerToken::path(&runtime.config.gateway.state_path).unwrap())
                .unwrap();
        assert!(!format!("{error:#}").contains(&token));
        assert!(!runtime.status().to_string().contains(&token));
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn reflected_handshake_credentials_never_enter_status_or_startup_error() {
        use std::io::{Read, Write};
        let (root, runtime) = fixture();
        ServerToken::load_or_create(&runtime.config.gateway.state_path).unwrap();
        let token =
            std::fs::read_to_string(ServerToken::path(&runtime.config.gateway.state_path).unwrap())
                .unwrap();
        assert!(!runtime.status().to_string().contains(&token));
        let listener = std::net::TcpListener::bind(runtime.addresses[0]).unwrap();
        let reflected = token.clone();
        let peer = std::thread::spawn(move || {
            // Activation first makes an ordinary TCP availability probe.
            drop(listener.accept().unwrap().0);
            for status in [401, 403] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                assert_eq!(
                    String::from_utf8(request).unwrap().contains(&reflected),
                    status == 403
                );
                write!(stream, "HTTP/1.1 {status} Refused\r\nX-Echo: {reflected}\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{reflected}").unwrap();
            }
        });
        let error = runtime.activate(false).unwrap_err();
        peer.join().unwrap();
        let status = runtime.status();
        assert!(status["lastStartupError"].as_str().unwrap().contains("403"));
        assert!(!format!("{error:#}").contains(&token));
        assert!(!status.to_string().contains(&token));
        assert!(!status.to_string().contains(&token[..16]));
        assert!(runtime.gateway().is_none());
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn absolute_receipt_path_is_preserved_when_the_child_changes_directory() {
        let (root, original) = fixture();
        let absolute = root.join("private-state/receipts.sqlite3");
        let runtime = NativeRuntime::new(RuntimeConfig {
            gateway: Config {
                endpoint: original.config.gateway.endpoint.clone(),
                allowed_roots: vec![root.clone()],
                state_path: absolute.clone(),
            },
            binary: original.config.binary.clone(),
            home: original.config.home.clone(),
            log_path: original.config.log_path.clone(),
        })
        .unwrap();
        assert_eq!(runtime.config.gateway.state_path, absolute);
        let command = runtime.server_command().unwrap();
        let args: Vec<_> = command.get_args().collect();
        let index = args
            .iter()
            .position(|arg| *arg == "--ws-token-file")
            .unwrap();
        assert_eq!(
            args[index + 1],
            ServerToken::path(&absolute).unwrap().as_os_str()
        );
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
        crate::remove_test_scratch(&root);
    }

    #[cfg(unix)]
    #[test]
    fn server_command_preserves_ssh_and_shell_without_session_overrides() {
        use std::os::unix::ffi::OsStringExt;
        if std::env::var("NATIVE_COMMAND_ENV_TEST").as_deref() != Ok("child") {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime::tests::server_command_preserves_ssh_and_shell_without_session_overrides", "--nocapture"])
                .env_clear()
                .env("TMPDIR", std::env::temp_dir())
                .env("NATIVE_COMMAND_ENV_TEST", "child")
                .env("SSH_AUTH_SOCK", "/synthetic/ssh-agent.sock")
                .env("SHELL", "/bin/zsh")
                .env("TERM", "xterm-256color")
                .env("OPENAI_API_KEY", "synthetic-key")
                .env("HTTPS_PROXY", std::ffi::OsString::from_vec(b"synthetic-\xff".to_vec()))
                .env("CODEX_THREAD_ID", "must-not-inherit")
                .env("RUST_LOG", "trace")
                .output().unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (root, runtime) = fixture();
        // Execute a harmless fixture at the configured binary path. It ignores
        // the app-server arguments and reports the actual inherited environment.
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&runtime.config.binary, "#!/bin/sh\nexec /usr/bin/env\n").unwrap();
        std::fs::set_permissions(
            &runtime.config.binary,
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mut command = runtime.server_command().unwrap();
        let values: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for (name, expected) in [
            ("SSH_AUTH_SOCK", "/synthetic/ssh-agent.sock"),
            ("SHELL", "/bin/zsh"),
            ("TERM", "xterm-256color"),
        ] {
            assert_eq!(values[std::ffi::OsStr::new(name)].unwrap(), expected);
        }
        for name in ["CODEX_THREAD_ID", "RUST_LOG", "NATIVE_COMMAND_ENV_TEST"] {
            assert!(!values.contains_key(std::ffi::OsStr::new(name)));
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        // An inherited Unix value need not be UTF-8. Never panic with the full
        // environment bytes, which could include credentials, as the error value.
        let environment = String::from_utf8_lossy(&output.stdout);
        for expected in [
            "SSH_AUTH_SOCK=/synthetic/ssh-agent.sock",
            "SHELL=/bin/zsh",
            "TERM=xterm-256color",
            "HTTPS_PROXY=synthetic-\u{fffd}",
            "OPENAI_API_KEY=synthetic-key",
        ] {
            assert!(
                environment.lines().any(|line| line == expected),
                "Missing expected variable: {expected}"
            );
        }
        for name in ["CODEX_THREAD_ID", "RUST_LOG", "NATIVE_COMMAND_ENV_TEST"] {
            assert!(
                !environment
                    .lines()
                    .any(|line| line.starts_with(&format!("{name}="))),
                "Session override leaked: {name}"
            );
        }
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn reconnect_failure_preserves_a_server_started_by_an_earlier_activation() {
        let (root, runtime) = fixture();
        let _listener = std::net::TcpListener::bind(runtime.addresses[0]).unwrap();
        std::fs::create_dir(root.join("receipts.sqlite3")).unwrap();
        runtime.state.lock().unwrap().child =
            Some(Command::new("sleep").arg("60").spawn().unwrap());
        struct Reap(Arc<NativeRuntime>);
        impl Drop for Reap {
            fn drop(&mut self) {
                if let Some(mut child) = self.0.state.lock().unwrap().child.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let reaper = Reap(runtime.clone());
        assert!(runtime.activate(false).is_err());
        assert!(runtime.gateway().is_none());
        assert!(
            runtime
                .state
                .lock()
                .unwrap()
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none(),
            "A failed reconnect must not terminate the retained shared process"
        );
        drop(reaper);
        crate::remove_test_scratch(&root);
    }

    #[cfg(unix)]
    #[test]
    fn native_toggle_stops_and_reaps_its_server_but_preserves_external_listeners() {
        use std::os::unix::process::CommandExt;
        let (root, runtime) = fixture();
        let listener = std::net::TcpListener::bind(runtime.addresses[0]).unwrap();
        for _ in 0..2 {
            let mut command = Command::new("/bin/sleep");
            command.arg("60");
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().unwrap();
            let pid = child.id();
            runtime.state.lock().unwrap().child = Some(child);
            assert_eq!(runtime.status()["ownedServerPid"], pid);
            let status = runtime.deactivate().unwrap();
            assert!(status["ownedServerPid"].is_null());
            assert_eq!(
                unsafe { libc::kill(pid as i32, 0) },
                -1,
                "Owned server must be reaped"
            );
            assert_eq!(
                status["serverListening"], true,
                "Independent listener is preserved"
            );
            assert_eq!(status["runtime"], "available");
        }
        assert_eq!(runtime.deactivate().unwrap()["serverListening"], true);
        drop(listener);
        assert_eq!(runtime.status()["runtime"], "stopped");
        crate::remove_test_scratch(&root);
    }

    #[test]
    fn startup_command_is_authenticated_standard_speed_and_has_explicit_context() {
        let (root, runtime) = fixture();
        let command = runtime.server_command().unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(args
            .windows(2)
            .any(|args| args == ["--ws-auth", "capability-token"]));
        assert!(args.contains(&"--ws-token-file"));
        assert!(args.contains(&"service_tier=\"default\""));
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
        assert!(runtime.status().get("desktopHelp").is_none());
        assert!(!root.join("codex-server.token").exists());
        crate::remove_test_scratch(&root);
    }
    #[test]
    fn slow_activation_does_not_hold_runtime_state_mutex() {
        let (root, runtime) = fixture();
        let listener = std::net::TcpListener::bind(runtime.addresses[0]).unwrap();
        listener.set_nonblocking(true).unwrap();
        let accept = || {
            // Credential publication between the probe and handshake includes two fsyncs.
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => return stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "Activation did not reach the mock listener within 15 seconds"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("Accept activation connection: {error}"),
                }
            }
        };
        let target = runtime.clone();
        let activation = std::thread::spawn(move || target.activate(false));
        // The first connection is only the listener probe. Hold the authentication
        // exchange open while checking the independent state and status readers.
        let probe = accept();
        drop(probe);
        let handshake = accept();
        let start = Instant::now();
        assert!(runtime.gateway().is_none());
        assert!(runtime.status()["serverListening"].as_bool().unwrap());
        assert!(start.elapsed() < Duration::from_millis(250));
        assert!(runtime
            .activate(false)
            .unwrap_err()
            .to_string()
            .contains("transition"));
        drop(handshake);
        assert!(activation.join().unwrap().is_err());
        drop(listener);
        crate::remove_test_scratch(&root);
    }

    #[cfg(unix)]
    #[test]
    fn failed_startup_reaps_the_owned_group_including_a_launchers_child() {
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        // A disposable shell/sleep process group; never invokes a Codex executable.
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 60 & echo $!; exit 7"])
            .stdout(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let leader = child.id();
        let mut stdout = child.stdout.take().unwrap();
        let mut pid = Vec::new();
        loop {
            let mut byte = [0];
            stdout.read_exact(&mut byte).unwrap();
            if byte[0] == b'\n' {
                break;
            }
            pid.push(byte[0]);
        }
        let descendant: i32 = String::from_utf8(pid).unwrap().parse().unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(7));
        let mut startup = StartupChild(Some(child));
        startup.stop().unwrap();
        assert!(startup.0.is_none());
        assert_eq!(unsafe { libc::kill(leader as i32, 0) }, -1);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let output = Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &descendant.to_string()])
                .output()
                .unwrap();
            let status = String::from_utf8(output.stdout).unwrap();
            if status.trim().is_empty() || status.trim_start().starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Launcher descendant is still running: {status}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
