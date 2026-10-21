//! Synthetic transport/process fixtures only; never invoke an installed Codex.
use super::*;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::Path;

struct RestartScratch(PathBuf);
impl std::ops::Deref for RestartScratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}
impl Drop for RestartScratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("Remove restart fixture {}: {error}", self.0.display());
            }
        }
    }
}

// Use a short, private path: workspace scratch paths can exceed sockaddr_un.sun_path.
fn restart_fixture() -> (RestartScratch, Arc<NativeRuntime>) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = PathBuf::from("/tmp").join(format!(
        "cr-restart-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runtime = NativeRuntime::new(RuntimeConfig {
        gateway: Config {
            endpoint: format!("unix://{}", root.join("rpc.sock").display()),
            allowed_roots: vec![root.clone()],
            state_path: root.join("receipts.sqlite3"),
        },
        binary: root.join("native-fixture"),
        home: root.join("codex-home"),
        log_path: root.join("native.log"),
    })
    .unwrap();
    (RestartScratch(root), runtime)
}

#[test]
fn restart_scratch_is_removed_when_a_test_unwinds() {
    let (root, runtime) = restart_fixture();
    let path = root.0.clone();
    std::fs::write(root.join("copied-test-binary"), b"synthetic fixture").unwrap();
    let failed = std::panic::catch_unwind(move || {
        let _scratch = root;
        let _runtime = runtime;
        panic!("synthetic restart assertion failure");
    });
    assert!(failed.is_err());
    assert!(
        !path.exists(),
        "failed tests must remove copied executables"
    );
}

#[test]
fn fixture_server() {
    let Ok(endpoint) = std::env::var("NATIVE_RESTART_FIXTURE_ENDPOINT") else {
        return;
    };
    match Endpoint::parse(&endpoint).unwrap() {
        Endpoint::PrivateUnix(path) => {
            let listener = UnixListener::bind(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            for stream in listener.incoming() {
                let stream = stream.unwrap();
                stream.set_nonblocking(false).unwrap();
                serve_fixture(crate::transport::NativeStream::Unix(stream), None);
            }
        }
        Endpoint::AuthenticatedWebSocket(endpoint) => {
            let token =
                std::fs::read_to_string(std::env::var("NATIVE_RESTART_FIXTURE_TOKEN").unwrap())
                    .unwrap();
            let listener =
                std::net::TcpListener::bind(endpoint.trim_start_matches("ws://")).unwrap();
            for stream in listener.incoming() {
                let stream = stream.unwrap();
                stream.set_nonblocking(false).unwrap();
                serve_fixture(
                    crate::transport::NativeStream::Tcp(stream),
                    Some(token.clone()),
                );
            }
        }
    }
}

fn serve_fixture(stream: crate::transport::NativeStream, token: Option<String>) {
    std::thread::spawn(move || {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        #[allow(clippy::result_large_err)] // tungstenite fixes the callback error type.
        let socket = tungstenite::accept_hdr(
            stream,
            |request: &tungstenite::handshake::server::Request, response| {
                if token.as_ref().is_some_and(|token| {
                    request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        != Some(format!("Bearer {token}").as_str())
                }) {
                    return Err(tungstenite::http::Response::builder()
                        .status(401)
                        .body(None)
                        .unwrap());
                }
                Ok(response)
            },
        );
        let Ok(mut socket) = socket else {
            return;
        };
        while let Ok(message) = socket.read() {
            let tungstenite::Message::Text(text) = message else {
                continue;
            };
            let request: Value = serde_json::from_str(&text).unwrap();
            if let Some(id) = request.get("id") {
                let result = if request["method"] == "initialize" {
                    json!({"userAgent":"synthetic-fixture"})
                } else {
                    json!({"data":[],"nextCursor":null})
                };
                if socket
                    .send(tungstenite::Message::Text(
                        json!({"id":id,"result":result}).to_string().into(),
                    ))
                    .is_err()
                {
                    break;
                }
            }
        }
    });
}

struct Reap(Option<u32>);
impl Reap {
    fn assert_exited(&mut self) {
        let pid = self.0.unwrap() as i32;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let mut status = 0;
            let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if reaped == pid {
                self.0 = None;
                return;
            }
            assert_eq!(
                reaped,
                0,
                "wait for fixture {pid}: {}",
                std::io::Error::last_os_error()
            );
            assert!(
                Instant::now() < deadline,
                "fixture {pid} survived explicit stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Reap {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // This test's unreaped child, never a discovered/shared process.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
                libc::waitpid(pid as i32, std::ptr::null_mut(), 0);
            }
        }
    }
}
fn copy_runtime(runtime: &NativeRuntime) -> Arc<NativeRuntime> {
    NativeRuntime::new(RuntimeConfig {
        gateway: Config {
            endpoint: runtime.config.gateway.endpoint.clone(),
            allowed_roots: runtime.config.gateway.allowed_roots.clone(),
            state_path: runtime.config.gateway.state_path.clone(),
        },
        binary: runtime.config.binary.clone(),
        home: runtime.config.home.clone(),
        log_path: runtime.config.log_path.clone(),
    })
    .unwrap()
}
fn install_fixture(runtime: &NativeRuntime) {
    let quote =
        |path: &std::path::Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let token = runtime
        .token_file
        .as_ref()
        .map(|path| format!("export NATIVE_RESTART_FIXTURE_TOKEN={}\n", quote(path)))
        .unwrap_or_default();
    let script = format!("#!/bin/sh\nexport NATIVE_RESTART_FIXTURE_ENDPOINT='{}'\n{token}exec {} --exact runtime::restart_tests::fixture_server --nocapture\n", runtime.config.gateway.endpoint, quote(&std::env::current_exe().unwrap()));
    std::fs::write(&runtime.config.binary, script).unwrap();
    std::fs::set_permissions(
        &runtime.config.binary,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
}

#[test]
fn host_exit_preserves_server_and_restart_recovers_it_for_explicit_close() {
    let (root, runtime) = restart_fixture();
    install_fixture(&runtime);
    let first = runtime.activate(true).unwrap();
    let pid = first["ownedServerPid"].as_u64().unwrap() as u32;
    let mut guard = Reap(Some(pid));
    let mut next = copy_runtime(&runtime);
    let observer = copy_runtime(&runtime);
    assert!(
        observer.activate(false).unwrap()["ownedServerPid"].is_null(),
        "another Monitor cannot steal the live lease"
    );
    observer.deactivate().unwrap();
    drop(observer);
    assert_eq!(runtime.shutdown().unwrap()["serverListening"], true);
    drop(runtime);
    // On the next host, a live receipt with an unavailable endpoint also names
    // the PID without claiming ownership or spawning a duplicate server.
    let endpoint = std::mem::replace(
        &mut Arc::get_mut(&mut next).unwrap().endpoint,
        Endpoint::PrivateUnix(root.join("unavailable.sock")),
    );
    assert!(next
        .activate(true)
        .unwrap_err()
        .to_string()
        .contains(&pid.to_string()));
    Arc::get_mut(&mut next).unwrap().endpoint = endpoint;
    let status = next.activate(false).unwrap();
    assert_eq!(status["ownedServerPid"], pid);
    assert_eq!(status["homeProvenance"], "monitor-started");
    next.force_deactivate().unwrap();
    // Model a transiently unreachable listener without killing the owned process.
    let endpoint = std::mem::replace(
        &mut Arc::get_mut(&mut next).unwrap().endpoint,
        Endpoint::PrivateUnix(root.join("unavailable.sock")),
    );
    assert!(next
        .activate(true)
        .unwrap_err()
        .to_string()
        .contains(&pid.to_string()));
    assert!(
        ownership::Ownership::claim(&next.config.gateway.state_path).is_err(),
        "a failed reconnect must retain the existing ownership lease"
    );
    Arc::get_mut(&mut next).unwrap().endpoint = endpoint;
    assert_eq!(
        next.activate(false).unwrap()["ownedServerPid"],
        pid,
        "reconnect keeps its lease"
    );
    next.deactivate().unwrap();
    assert!(!next.listening());
    guard.assert_exited();
    assert!(!root.join("receipts.owner.json").exists());
    drop(next);
    drop(guard);
}

#[test]
fn invalid_receipt_or_token_cannot_authorize_stopping_external_process() {
    for invalid_token in [false, true] {
        let (root, runtime) = super::tests::fixture();
        let root = RestartScratch(root);
        install_fixture(&runtime);
        let pid = runtime.activate(true).unwrap()["ownedServerPid"]
            .as_u64()
            .unwrap() as u32;
        let guard = Reap(Some(pid));
        let next = copy_runtime(&runtime);
        runtime.shutdown().unwrap();
        drop(runtime);
        if invalid_token {
            std::fs::write(next.token_file.as_ref().unwrap(), "0".repeat(64)).unwrap();
            assert!(next.activate(false).is_err());
        } else {
            std::fs::set_permissions(
                root.join("receipts.owner.json"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert!(next.activate(false).unwrap()["ownedServerPid"].is_null());
            next.deactivate().unwrap();
        }
        assert!(next.listening(), "unverified server must stay alive");
        let address = next.addresses[0];
        drop(next);
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_ok(),
            "dropping an observer must preserve the external subprocess"
        );
        drop(guard);
    }
}

#[test]
fn executable_removal_preserves_status_restart_and_explicit_stop() {
    let (root, runtime) = restart_fixture();
    install_fixture(&runtime);
    // A package upgrade can unlink the executable while its server keeps running.
    let executable = root.join("old-native-fixture");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let launcher = std::fs::read_to_string(&runtime.config.binary)
        .unwrap()
        .replace(
            std::env::current_exe().unwrap().to_str().unwrap(),
            executable.to_str().unwrap(),
        );
    std::fs::write(&runtime.config.binary, launcher).unwrap();
    let pid = runtime.activate(true).unwrap()["ownedServerPid"]
        .as_u64()
        .unwrap() as u32;
    let mut guard = Reap(Some(pid));
    runtime.shutdown().unwrap();
    let next = copy_runtime(&runtime);
    drop(runtime);
    assert_eq!(next.activate(false).unwrap()["ownedServerPid"], pid);
    std::fs::remove_file(executable).unwrap();
    assert_eq!(
        next.status()["ownedServerPid"],
        pid,
        "status must retain the lease after unlink"
    );
    assert!(ownership::Ownership::claim(&next.config.gateway.state_path).is_err());
    next.shutdown().unwrap();
    let recovered = copy_runtime(&next);
    drop(next);
    assert_eq!(
        recovered.activate(false).unwrap()["ownedServerPid"],
        pid,
        "a fresh host must recover an authenticated server with a removed executable"
    );
    recovered.deactivate().unwrap();
    assert!(!recovered.listening());
    guard.assert_exited();
    drop(recovered);
    drop(guard);
}
