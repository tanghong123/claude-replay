//! Synthetic transport/process fixtures only; never invoke an installed Codex.
use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
#[allow(clippy::result_large_err)] // tungstenite fixes the handshake callback error type.
fn fixture_server() {
    let Ok(endpoint) = std::env::var("NATIVE_RESTART_FIXTURE_ENDPOINT") else {
        return;
    };
    let token =
        std::fs::read_to_string(std::env::var("NATIVE_RESTART_FIXTURE_TOKEN").unwrap()).unwrap();
    let listener = std::net::TcpListener::bind(endpoint.trim_start_matches("ws://")).unwrap();
    for stream in listener.incoming() {
        let stream = stream.unwrap();
        let token = token.clone();
        std::thread::spawn(move || {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let socket = tungstenite::accept_hdr(
                stream,
                |request: &tungstenite::handshake::server::Request, response| {
                    if request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        != Some(format!("Bearer {token}").as_str())
                    {
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
}

struct Reap(u32);
impl Drop for Reap {
    fn drop(&mut self) {
        // This test's child, never a discovered/shared process.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
            libc::waitpid(self.0 as i32, std::ptr::null_mut(), 0);
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
    let script = format!("#!/bin/sh\nexport NATIVE_RESTART_FIXTURE_ENDPOINT='{}'\nexport NATIVE_RESTART_FIXTURE_TOKEN={}\nexec {} --exact runtime::restart_tests::fixture_server --nocapture\n", runtime.config.gateway.endpoint, quote(runtime.token_file.as_ref().unwrap()), quote(&std::env::current_exe().unwrap()));
    std::fs::write(&runtime.config.binary, script).unwrap();
    std::fs::set_permissions(
        &runtime.config.binary,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
}

#[test]
fn host_exit_preserves_server_and_restart_recovers_it_for_explicit_close() {
    let (root, runtime) = super::tests::fixture();
    install_fixture(&runtime);
    let first = runtime.activate(true).unwrap();
    let pid = first["ownedServerPid"].as_u64().unwrap() as u32;
    let guard = Reap(pid);
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
    let status = next.activate(false).unwrap();
    assert_eq!(status["ownedServerPid"], pid);
    assert_eq!(status["homeProvenance"], "monitor-started");
    next.force_deactivate().unwrap();
    // Model a transiently unreachable listener without killing the owned process.
    let addresses = std::mem::take(&mut Arc::get_mut(&mut next).unwrap().addresses);
    assert!(next.activate(true).is_err());
    assert!(
        ownership::Ownership::claim(&next.config.gateway.state_path).is_err(),
        "a failed reconnect must retain the existing ownership lease"
    );
    Arc::get_mut(&mut next).unwrap().addresses = addresses;
    assert_eq!(
        next.activate(false).unwrap()["ownedServerPid"],
        pid,
        "reconnect keeps its lease"
    );
    next.deactivate().unwrap();
    assert!(!next.listening());
    assert!(!root.join("receipts.owner.json").exists());
    drop(next);
    drop(guard);
    crate::remove_test_scratch(&root);
}

#[test]
fn invalid_receipt_or_token_cannot_authorize_stopping_external_process() {
    for invalid_token in [false, true] {
        let (root, runtime) = super::tests::fixture();
        install_fixture(&runtime);
        let pid = runtime.activate(true).unwrap()["ownedServerPid"]
            .as_u64()
            .unwrap() as u32;
        let guard = Reap(pid);
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
        crate::remove_test_scratch(&root);
    }
}
