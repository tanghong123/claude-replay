//! The browser shell and shared-runtime integration, leaving both existing
//! Monitor readers available. Execution policy belongs to `codex_gateway`.

use crate::codex_gateway::runtime::{NativeRuntime, RuntimeConfig};
use crate::codex_gateway::Config;
use crate::index::Index;
use claude_replay_core::Agent;
use claude_replay_html::{HttpReply, HttpResponse, Request, RouteHandler, StreamingRouteHandler};
use serde_json::{json, Value};
use std::sync::Arc;

/// Only static Home Screen icons and manifests bypass authentication.
/// The ordinary gate's 401 page links to its built-in pairing page.
pub const PUBLIC_ASSETS: &[&str] = &[
    "favicon.svg",
    "favicon.ico",
    "apple-touch-icon.png",
    "icon-192.png",
    "icon-512.png",
    "icon-192-maskable.png",
    "icon-512-maskable.png",
    "manifest.webmanifest",
    "codex-workspace/manifest.webmanifest",
    "codex-workspace/icon.svg",
    "codex-workspace/icon-192.png",
    "codex-workspace/icon-512.png",
    "codex-workspace/apple-touch-icon.png",
    "codex-workspace/icon-180.png",
];

/// Compose inert native support with the unchanged app/classic reader routes.
/// No runtime is activated by construction, assets, status or session reading.
pub fn handler(
    legacy: RouteHandler,
    runtime: Arc<NativeRuntime>,
    index: Arc<Index>,
) -> StreamingRouteHandler {
    struct Ownership(std::sync::Weak<NativeRuntime>);
    impl crate::index::NativeSessions for Ownership {
        fn is_active(&self) -> bool {
            self.0
                .upgrade()
                .is_some_and(|runtime| runtime.gateway().is_some())
        }
        fn owns_session(&self, id: &str) -> bool {
            self.0
                .upgrade()
                .and_then(|runtime| runtime.gateway())
                .is_some_and(|gateway| gateway.owns_thread(id))
        }
    }
    index.require_shared_codex_runtime(Arc::new(Ownership(Arc::downgrade(&runtime))));
    Arc::new(move |request| {
        if let Some(reply) = asset(request.name) {
            return reply.into();
        }
        if matches!(
            request.name,
            "api/codex/status" | "api/codex/start" | "api/codex/stop"
        ) {
            if request.name == "api/codex/status" {
                if request.method != "GET" {
                    return HttpResponse::method_not_allowed("native status requires GET").into();
                }
                if !request.authenticated {
                    return HttpResponse::unauthorized("Native Codex requires pairing").into();
                }
                if !request.origin_ok {
                    return HttpResponse::forbidden("cross-origin native status refused").into();
                }
                return HttpResponse::json(runtime.status().to_string()).into();
            }
            if let Some(deny) = request.deny_write() {
                return deny.into();
            }
            if request.name == "api/codex/stop" {
                return match runtime.deactivate() {
                    Ok(status) => {
                        index.native_sessions_changed();
                        HttpResponse::json(status.to_string()).into()
                    }
                    Err(error) => {
                        let busy = matches!(error, claude_monitor_codex::DisconnectError::Busy(_));
                        let mut response = HttpResponse::json(json!({"ok":false,"error":{"code":if busy {"native_busy"} else {"native_disconnect_failed"},"message":error.to_string()}}).to_string());
                        response.code = if busy {
                            "409 Conflict"
                        } else {
                            "503 Service Unavailable"
                        };
                        response.into()
                    }
                };
            }
            let body: Value = match serde_json::from_slice(request.body) {
                Ok(body) => body,
                Err(error) => {
                    return invalid(&format!("Invalid native activation request: {error}")).into()
                }
            };
            if body["writeMode"].as_bool() != Some(true)
                || body["startIfMissing"].as_bool().is_none()
            {
                return invalid(
                    "Native activation requires Write mode and an explicit startIfMissing choice",
                )
                .into();
            }
            return match runtime.activate(body["startIfMissing"].as_bool().expect("validated")) {
                Ok(status) => {
                    index.native_sessions_changed();
                    HttpResponse::json(status.to_string()).into()
                }
                Err(error) => {
                    let mut reply = HttpResponse::json(json!({"ok":false,"error":{"code":"native_start_failed","message":format!("{error:#}")}}).to_string());
                    reply.code = "503 Service Unavailable";
                    reply.into()
                }
            };
        }
        if request.name == "api/codex/render" {
            return render_route(request).into();
        }
        if let Some(gateway) = runtime.gateway() {
            if request.name == "api/codex/events" {
                if request.method != "GET" {
                    return HttpResponse::method_not_allowed("event stream requires GET").into();
                }
                if !request.origin_ok {
                    return HttpResponse::forbidden("cross-origin event stream refused").into();
                }
                if !request.authenticated {
                    return HttpResponse::unauthorized("event stream requires pairing").into();
                }
                let (initial, receiver) = gateway.subscribe();
                return HttpReply::events(initial, receiver);
            }
            if let Some(reply) = gateway.route(request) {
                return reply.into();
            }
            if let Some(reply) = deny_legacy_codex_write(request, |id| {
                gateway.owns_thread(id) && index.agent_for_session(id) == Some(Agent::CODEX)
            }) {
                return reply.into();
            }
        } else if request.name.starts_with("api/codex/") {
            if !request.authenticated {
                return HttpResponse::unauthorized("Native Codex requires pairing").into();
            }
            let mut reply = HttpResponse::json(json!({"ok":false,"error":{"code":"native_not_enabled","message":"Open Write mode, then enable Codex native mode."}}).to_string());
            reply.code = "409 Conflict";
            return reply.into();
        }
        let mut reply = legacy(request);
        if reply.content_type.starts_with("text/html") && reply.stream.is_none() {
            if let Ok(html) = String::from_utf8(reply.body.clone()) {
                if html.contains("src=\"/monitor-ui/app.js\"") {
                    reply.body = native_reader_page(html).into_bytes();
                }
            }
        }
        reply.into()
    })
}

/// Maximum image-enabled native send body; the generic listener receives this policy from its caller.
pub use claude_monitor_codex::SEND_BODY_LIMIT;

/// Resolve trusted host configuration without starting a server or creating its state.
pub fn configured_runtime(
    endpoint: Option<String>,
    roots: Vec<std::path::PathBuf>,
    state: Option<std::path::PathBuf>,
) -> anyhow::Result<Arc<NativeRuntime>> {
    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => match std::env::var("CODEX_NATIVE_SERVER_URL") {
            Ok(endpoint) => endpoint,
            Err(std::env::VarError::NotPresent) => if cfg!(unix) {
                "unix://"
            } else {
                "ws://127.0.0.1:4500"
            }
            .to_owned(),
            Err(error) => {
                return Err(anyhow::Error::new(error).context("Read CODEX_NATIVE_SERVER_URL"))
            }
        },
    };
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("Native mode needs an absolute HOME"))?;
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let binary = std::env::var_os("CODEX_NATIVE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("codex"));
    NativeRuntime::new(RuntimeConfig {
        gateway: Config {
            endpoint,
            allowed_roots: if roots.is_empty() { vec![home] } else { roots },
            state_path: state
                .unwrap_or_else(|| crate::index::state_dir().join("codex-gateway.sqlite3")),
        },
        binary,
        home: codex_home,
        log_path: crate::index::state_dir().join("codex-native.log"),
    })
}

/// This guard executes at the HTTP table, including classic and old app shells.
/// A missing tmux pane or process attribution never authorizes a second writer.
fn deny_legacy_codex_write(
    request: &Request<'_>,
    is_codex: impl Fn(&str) -> bool,
) -> Option<HttpResponse> {
    if !matches!(request.name, "api/send" | "api/consent") {
        return None;
    }
    let target = claude_replay_html::query_get(request.query, "target")?;
    if !is_codex(&crate::index::percent_decode(target)) {
        return None;
    }
    if let Some(deny) = request.deny_write() {
        return Some(deny);
    }
    Some(crate::control::shared_runtime_required())
}

fn render_route(request: &Request<'_>) -> HttpResponse {
    if let Some(deny) = request.deny_write() {
        return deny;
    }
    let input: Value = match serde_json::from_slice(request.body) {
        Ok(input) => input,
        Err(error) => return invalid(&format!("Invalid Markdown request: {error}")),
    };
    let Some(texts) = input["texts"].as_array() else {
        return invalid("texts must be an array");
    };
    if texts.len() > 128 {
        return invalid("at most 128 Markdown texts per batch");
    }
    let mut html = Vec::with_capacity(texts.len());
    for text in texts {
        let Some(text) = text.as_str() else {
            return invalid("every Markdown text must be a string");
        };
        if text.len() > 256 * 1024 {
            return invalid("Markdown text exceeds 256 KiB");
        }
        html.push(claude_replay_html::render_markdown(text));
    }
    HttpResponse::json(json!({"ok":true,"html":html}).to_string())
}

/// The extension advertises capability; the browser still starts in ordinary reader mode.
fn native_reader_page(page: String) -> String {
    page.replace("</head>", "<link rel=\"stylesheet\" href=\"/monitor-ui/native-entry.css\"><script type=\"module\" src=\"/monitor-ui/native-entry.js\"></script></head>")
}

fn monitor_page() -> String {
    native_reader_page(crate::ui::page(env!("CARGO_PKG_VERSION"), true, true))
        .replace(
            "href=\"/manifest.webmanifest\"",
            "href=\"/codex-workspace/manifest.webmanifest\"",
        )
        .replace(
            "</head>",
            "<meta name=\"apple-mobile-web-app-capable\" content=\"yes\"></head>",
        )
}

fn invalid(message: &str) -> HttpResponse {
    let mut reply = HttpResponse::json(
        json!({"ok":false,"error":{"code":"invalid_input","message":message}}).to_string(),
    );
    reply.code = "400 Bad Request";
    reply
}

/// Embed the workspace at build time, preserving Monitor's standalone binary.
pub fn asset(name: &str) -> Option<HttpResponse> {
    if matches!(
        name,
        "codex-workspace" | "codex-workspace/" | "codex-workspace/index.html"
    ) {
        return Some(HttpResponse::html(monitor_page()));
    }
    if name == "codex-workspace/pair" {
        // Keep navigation inside the standalone app's scope so Safari cannot
        // redeem the code into a different cookie context. The return path is
        // fixed, rather than accepting an arbitrary redirect from the query.
        return Some(HttpResponse::html(
            claude_replay_html::pairing::pair_page(
                claude_replay_html::pairing::DEFAULT_PAIR_COMMAND,
            )
            .replace(
                "location.replace(\"/\")",
                "location.replace(\"/codex-workspace\")",
            ),
        ));
    }
    let (kind, bytes): (&'static str, &[u8]) = match name {
        "monitor-ui/native-entry.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-ui/native-entry.js"),
        ),
        "monitor-ui/native-entry.css" => (
            "text/css; charset=utf-8",
            include_bytes!("codex-ui/native-entry.css"),
        ),

        "codex-workspace/monitor-controls.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/monitor-controls.js"),
        ),
        "codex-workspace/outgoing.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/outgoing.js"),
        ),
        "codex-workspace/project-picker.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/project-picker.js"),
        ),
        "codex-workspace/session-conflict.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/session-conflict.js"),
        ),
        "codex-workspace/attachments.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/attachments.js"),
        ),
        "codex-workspace/monitor-model.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/monitor-model.js"),
        ),
        "codex-workspace/monitor.css" => (
            "text/css; charset=utf-8",
            include_bytes!("codex-workspace/monitor.css"),
        ),
        "codex-workspace/api.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/api.js"),
        ),
        "codex-workspace/model.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/model.js"),
        ),
        "codex-workspace/requests.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/requests.js"),
        ),
        "codex-workspace/manifest.webmanifest" => (
            "application/manifest+json",
            include_bytes!("codex-workspace/manifest.webmanifest"),
        ),
        "codex-workspace/icon.svg" => ("image/svg+xml", include_bytes!("codex-workspace/icon.svg")),
        "codex-workspace/icon-192.png" => {
            ("image/png", include_bytes!("codex-workspace/icon-192.png"))
        }
        "codex-workspace/icon-512.png" => {
            ("image/png", include_bytes!("codex-workspace/icon-512.png"))
        }
        "codex-workspace/apple-touch-icon.png" | "codex-workspace/icon-180.png" => {
            ("image/png", include_bytes!("codex-workspace/icon-180.png"))
        }
        "codex-workspace/sw.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-workspace/sw.js"),
        ),
        _ => return None,
    };
    let mut reply = HttpResponse::ok(kind, bytes.to_vec());
    reply
        .headers
        .push("X-Content-Type-Options: nosniff".to_owned());
    if name == "codex-workspace/sw.js" {
        reply
            .headers
            .push("Service-Worker-Allowed: /codex-workspace".to_owned());
    }
    Some(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn environment_endpoint_probe() {
        let Some(case) = std::env::var_os("NATIVE_ENDPOINT_PROBE_CASE") else {
            return;
        };
        let case = case.to_str().unwrap();
        let configured = (case == "override").then(|| "ws://127.0.0.1:2".to_owned());
        let result = configured_runtime(configured, vec![], None);
        if case == "invalid" {
            let error = result
                .err()
                .expect("Invalid UTF-8 must never select the default endpoint");
            assert!(format!("{error:#}").contains("Read CODEX_NATIVE_SERVER_URL"));
            assert!(matches!(
                error.downcast_ref::<std::env::VarError>(),
                Some(std::env::VarError::NotUnicode(_))
            ));
        } else {
            let runtime = result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
            let status = runtime.status();
            let home = std::path::PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
            let (endpoint, transport) = match case {
                "absent" => (
                    format!(
                        "unix://{}",
                        home.join("app-server-control/app-server-control.sock")
                            .display()
                    ),
                    "unix",
                ),
                "unix" => (
                    format!("unix://{}", home.join("custom.sock").display()),
                    "unix",
                ),
                "valid" => ("ws://127.0.0.1:1".to_owned(), "websocket"),
                "override" => ("ws://127.0.0.1:2".to_owned(), "websocket"),
                _ => panic!("Unknown endpoint probe {case}"),
            };
            assert_eq!(status["endpoint"], endpoint);
            assert_eq!(status["transport"], transport);
            assert_eq!(status["ownedServerPid"], Value::Null);
        }
        assert!(!std::path::Path::new(&std::env::var_os("HOME").unwrap()).exists());
        println!("NATIVE_ENDPOINT_PROBE_OK {case}");
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_environment_distinguishes_absence_invalid_utf8_and_explicit_override() {
        use std::os::unix::ffi::OsStringExt;
        let root = std::env::temp_dir().join(format!("native-env-probe-{}", std::process::id()));
        assert!(!root.exists());
        for case in ["invalid", "absent", "valid", "unix", "override"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "codex_workspace::tests::environment_endpoint_probe",
                    "--nocapture",
                ])
                .env("NATIVE_ENDPOINT_PROBE_CASE", case)
                .env("HOME", &root)
                .env("CODEX_HOME", root.join("codex"))
                .env("AGENT_MONITOR_STATE", root.join("state"));
            if case == "absent" {
                command.env_remove("CODEX_NATIVE_SERVER_URL");
            } else if case == "valid" {
                command.env("CODEX_NATIVE_SERVER_URL", "ws://127.0.0.1:1");
            } else if case == "unix" {
                command.env(
                    "CODEX_NATIVE_SERVER_URL",
                    format!("unix://{}", root.join("codex/custom.sock").display()),
                );
            } else {
                command.env(
                    "CODEX_NATIVE_SERVER_URL",
                    std::ffi::OsString::from_vec(vec![0xff]),
                );
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{case}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .contains(&format!("NATIVE_ENDPOINT_PROBE_OK {case}")),
                "Child probe must execute the intended test"
            );
        }
        assert!(!root.exists());
    }

    #[test]
    fn workspace_entry_keeps_original_reader_and_loads_only_the_enabling_control() {
        let page = String::from_utf8(asset("codex-workspace").unwrap().body).unwrap();
        assert!(
            !page.contains("maximum-scale=1"),
            "A reader starts with ordinary viewport behavior"
        );
        assert!(!crate::ui::page(env!("CARGO_PKG_VERSION"), true, true).contains("maximum-scale=1"));
        for marker in [
            "id=\"transcript\"",
            "id=\"composeInput\"",
            "id=\"navigatorTurns\"",
            "src=\"/monitor-ui/app.js\"",
            "src=\"/monitor-ui/native-entry.js\"",
            "href=\"/monitor-ui/native-entry.css\"",
        ] {
            assert!(
                page.contains(marker),
                "Missing original reader marker {marker}"
            );
        }
        assert!(!page.contains("data-codex-native"));
        assert!(!page.contains("/codex-workspace/monitor.css"));
        assert!(!page.contains("src=\"/codex-workspace/app.js\""));
        assert!(!PUBLIC_ASSETS.contains(&"codex-workspace/monitor-controls.js"));
        assert!(!PUBLIC_ASSETS.contains(&"monitor-ui/app.js"));
    }

    #[test]
    fn only_install_icons_and_manifest_are_public() {
        let manifest: Value =
            serde_json::from_slice(&asset("codex-workspace/manifest.webmanifest").unwrap().body)
                .unwrap();
        assert_eq!(manifest["scope"], "/");
        for icon in manifest["icons"].as_array().unwrap() {
            let name = icon["src"].as_str().unwrap().trim_start_matches('/');
            assert!(PUBLIC_ASSETS.contains(&name));
            let response = asset(name).unwrap();
            if icon["type"] == "image/png" {
                assert_eq!(&response.body[..8], b"\x89PNG\r\n\x1a\n");
            }
        }
        let pairing = String::from_utf8(asset("codex-workspace/pair").unwrap().body).unwrap();
        assert!(pairing.contains("location.replace(\"/codex-workspace\")"));
        assert!(!pairing.contains("location.replace(\"/\")"));
        for name in [
            "codex-workspace/monitor.css",
            "codex-workspace",
            "codex-workspace/",
            "codex-workspace/index.html",
            "codex-workspace/pair",
            "codex-workspace/api.js",
            "codex-workspace/sw.js",
        ] {
            assert!(asset(name).is_some());
            assert!(!PUBLIC_ASSETS.contains(&name));
        }
        for name in ["monitor-ui/reference.css", "monitor-ui/production.css"] {
            assert!(!PUBLIC_ASSETS.contains(&name));
        }
        assert!(asset("codex-workspace/sw.js")
            .unwrap()
            .headers
            .contains(&"Service-Worker-Allowed: /codex-workspace".to_owned()));
    }

    #[test]
    fn legacy_codex_writes_are_refused_before_the_legacy_transport() {
        for name in ["api/send", "api/consent"] {
            let request = Request {
                method: "POST",
                name,
                query: "target=shared%2Dthread",
                body: b"prompt",
                authenticated: true,
                origin_ok: true,
            };
            assert_eq!(
                deny_legacy_codex_write(&request, |id| id == "shared-thread")
                    .unwrap()
                    .code,
                "409 Conflict"
            );
            assert!(deny_legacy_codex_write(&request, |_| false).is_none());
        }
    }

    #[test]
    fn markdown_batch_uses_monitor_rendering_and_gates_active_content() {
        let body = br#"{"texts":["**strong**","<script>alert(1)</script>","[unsafe](javascript:alert%281%29)","[safe](https://example.com)","![unsafe](javascript:alert%281%29)","[unsafe](data:text/html,test)"]}"#;
        let mut request = Request {
            method: "POST",
            name: "api/codex/render",
            query: "",
            body,
            authenticated: false,
            origin_ok: true,
        };
        assert_eq!(render_route(&request).code, "401 Unauthorized");
        request.authenticated = true;
        let reply: Value = serde_json::from_slice(&render_route(&request).body).unwrap();
        assert!(reply["html"][0]
            .as_str()
            .unwrap()
            .contains("<strong>strong</strong>"));
        assert!(!reply["html"][1].as_str().unwrap().contains("<script>"));
        assert!(!reply["html"][2].as_str().unwrap().contains("href="));
        assert!(reply["html"][3]
            .as_str()
            .unwrap()
            .contains("href=\"https://example.com\""));
        assert!(!reply["html"][4].as_str().unwrap().contains("href="));
        assert!(!reply["html"][5].as_str().unwrap().contains("href="));
        request.body = br#"{"texts":[42]}"#;
        assert_eq!(render_route(&request).code, "400 Bad Request");
        request.origin_ok = false;
        assert_eq!(render_route(&request).code, "403 Forbidden");
    }
}
