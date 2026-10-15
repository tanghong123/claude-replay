//! Explicit activation of a shared Codex runtime. Construction and status never spawn work.

use crate::{validate_endpoint, Config, Gateway};
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
    addresses: Vec<SocketAddr>,
    state: Mutex<RuntimeState>,
}

#[derive(Default)]
struct RuntimeState {
    gateway: Option<Arc<Gateway>>,
    child: Option<Child>,
    last_error: Option<String>,
}

impl NativeRuntime {
    /// Validate inert configuration. This opens no connection, directory, database or process.
    pub fn new(config: RuntimeConfig) -> Result<Arc<Self>> {
        validate_endpoint(&config.gateway.endpoint)?;
        let uri: tungstenite::http::Uri = config.gateway.endpoint.parse()?;
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
        if !config.home.is_absolute() || !config.log_path.is_absolute() {
            bail!("Native Codex home and log path must be absolute");
        }
        if config.gateway.allowed_roots.is_empty() {
            bail!("Native Codex mode needs an allowed project root");
        }
        Ok(Arc::new(Self {
            config,
            addresses,
            state: Mutex::new(RuntimeState::default()),
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

    /// Observe a listener and current client state. A TCP probe is not a native handshake.
    pub fn status(&self) -> Value {
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
        let listening = self.listening();
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
        result["homeProvenance"] = json!(if state.child.is_some() {
            "monitor-started"
        } else {
            "configured"
        });
        result["ownedServerPid"] = json!(state.child.as_ref().map(Child::id));
        result["lastStartupError"] = json!(state.last_error);
        result["desktopHelp"] = self.desktop_help();
        result
    }

    fn listening(&self) -> bool {
        self.addresses
            .iter()
            .any(|address| TcpStream::connect_timeout(address, Duration::from_millis(150)).is_ok())
    }

    /// Explicitly connect, starting a server only when requested and the endpoint is absent.
    /// Serialized activation/retry never creates a second worker or process. A successfully
    /// started shared server outlives this client; closing Write/native mode does not kill it.
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

    /// Disconnect an idle gateway while retaining an independently running app-server.
    pub fn deactivate(&self) -> std::result::Result<Value, crate::DisconnectError> {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(gateway) = &state.gateway {
                gateway.disconnect()?;
            }
            state.gateway = None;
        }
        Ok(self.status())
    }

    fn activate_inner(&self, start_if_missing: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let mut started_here = false;
        if !self.listening() {
            if !start_if_missing {
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
            if let Some(child) = state.child.as_mut() {
                if child.try_wait()?.is_none() {
                    bail!("The requested Codex server is still starting. Retry after it binds the configured endpoint.");
                }
                state.child = None;
            }
            let mut child = self.spawn_server()?;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if self.listening() {
                    break;
                }
                if let Some(exit) = child.try_wait()? {
                    bail!(
                        "Codex server exited with {exit}; inspect {}",
                        self.config.log_path.display()
                    );
                }
                if Instant::now() >= deadline {
                    child
                        .kill()
                        .context("Stop the owned Codex server after startup timeout")?;
                    child
                        .wait()
                        .context("Reap the owned Codex startup process")?;
                    bail!(
                        "Codex server did not bind {} within 10 seconds; inspect {}",
                        self.config.gateway.endpoint,
                        self.config.log_path.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            state.child = Some(child);
            started_here = true;
        }
        if state.gateway.is_none() {
            let gateway = Gateway::start(Config {
                endpoint: self.config.gateway.endpoint.clone(),
                allowed_roots: self.config.gateway.allowed_roots.clone(),
                state_path: self.config.gateway.state_path.clone(),
            });
            match gateway {
                Ok(gateway) => state.gateway = Some(gateway),
                Err(error) => {
                    if let Some(mut child) = if started_here {
                        state.child.take()
                    } else {
                        None
                    } {
                        child.kill().context(
                            "Stop owned Codex server after Gateway initialization failure",
                        )?;
                        child
                            .wait()
                            .context("Reap owned Codex initialization process")?;
                    }
                    return Err(error.context("Activate native Codex Gateway"));
                }
            }
        }
        state.last_error = None;
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
        let mut command = Command::new(&self.config.binary);
        command
            .args(["app-server", "--listen", &self.config.gateway.endpoint])
            .env("CODEX_HOME", &self.config.home)
            .env("CODEX_APP_SERVER_FORCE_CLI", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Desktop also uses the server: it must not inherit the Monitor terminal's lifetime.
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

    fn desktop_help(&self) -> Value {
        json!({"title":"Share this server with Codex App",
            "description":"Use the same live Codex sessions in Codex App and Agent Monitor, so you can switch between desktop and mobile without starting another task.",
            "steps":["Close the conflicting conversation in the Codex App that owns it.",
                "Quit that App instance after saving its work, then reopen it with this server endpoint and the same Codex home.",
                "Return here and reconnect the conversation. Agent Monitor will not stop your App automatically."],
            "endpoint":self.config.gateway.endpoint,
            "prompt":format!("Create a Codex App launcher that uses Agent Monitor's existing codex app-server at {}. Detect the App path and the server's CODEX_HOME, set CODEX_APP_SERVER_WS_URL, and unset CODEX_APP_SERVER_FORCE_CLI.", self.config.gateway.endpoint),
            "note":"The Desktop endpoint override is runtime-specific. Validate it when upgrading Codex App."})
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

    #[test]
    fn desktop_setup_prompt_is_generic_even_without_a_discovered_app() {
        let (root, runtime) = fixture();
        let status = runtime.status();
        let help = &status["desktopHelp"];
        let prompt = help["prompt"].as_str().unwrap();
        assert!(prompt.contains(status["endpoint"].as_str().unwrap()));
        assert!(prompt.contains("CODEX_HOME"));
        assert!(prompt.contains("CODEX_APP_SERVER_WS_URL"));
        assert!(prompt.contains("CODEX_APP_SERVER_FORCE_CLI"));
        assert!(!prompt.contains(root.to_str().unwrap()));
        assert!(!prompt.to_ascii_lowercase().contains("fast"));
        assert!(prompt.split_whitespace().count() <= 45);
        assert!(help["description"]
            .as_str()
            .unwrap()
            .contains("desktop and mobile"));
        assert!(help.get("command").is_none());
        assert!(help.get("codexHome").is_none());
        assert!(status["ownedServerPid"].is_null());
        assert!(runtime.gateway().is_none());
        assert!(!root.join("receipts.sqlite3").exists());
        crate::remove_test_scratch(&root);
    }
}
