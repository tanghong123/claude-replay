//! `agent-monitor --pair-phone` (#11): pair a phone with this PAIRED monitor through
//! `tailscale serve`, by a one-time code — never by showing the long-lived token.
//!
//! The owner serves the monitor to their tailnet with `tailscale serve` on the monitor's OWN port —
//! tailnet 2727 to localhost 2727 (#311, the owner: "directly port mapping") — and a phone on the
//! tailnet opens that. Never the name's root on 443: that one handler is shared by every tool on the
//! machine, and serving the monitor there once silently unserved another tool's page. This finds
//! the serve entry that proxies to THIS monitor's port, shows it and asks before going on (the
//! owner: "if not set up, print instructions, and if set up, confirm info with user"), then mints a
//! single-use code (claude_replay_html::pairing) and prints a terminal QR code for
//! `<address>/pair#code=<CODE>`, with the short code to type as the alternative. Nothing here
//! changes the tailscale configuration: serving the monitor is the owner's own, visible step.

use anyhow::{bail, Context, Result};
use claude_replay_html::pairing;
use serde_json::Value;
use std::io::{BufRead, IsTerminal, Write};

/// The addresses `tailscale serve` relays to 127.0.0.1:`port`, from `tailscale serve status
/// --json` (`serve`) and the machine's tailnet name (`dns_name`, `Self.DNSName` of `tailscale
/// status --json`). A web handler is `http://` or `https://` as its port's `TCP` entry says (`HTTP`
/// / `HTTPS`), with 80 / 443 left out; a raw TCP forward is `http://<name>:<port>/`, or `https://`
/// on the certificate's name when tailscale terminates TLS for it. A foreground serve (no `--bg`)
/// keeps the same shape under `Foreground.<session>`. The monitor's own port comes first — the
/// serve this tool recommends — then the rest in order.
pub fn serve_bases(serve: &Value, dns_name: &str, port: u16) -> Vec<String> {
    let mut out = Vec::new();
    serve_bases_of(serve, dns_name, port, &mut out);
    if let Some(sessions) = serve.get("Foreground").and_then(Value::as_object) {
        for session in sessions.values() {
            serve_bases_of(session, dns_name, port, &mut out);
        }
    }
    out.sort();
    out.dedup();
    let own = format!(":{port}/");
    out.sort_by_key(|b| !b.ends_with(&own));
    out
}

fn serve_bases_of(cfg: &Value, dns_name: &str, port: u16, out: &mut Vec<String>) {
    let local = |target: &str| {
        let t = target
            .trim_start_matches("http://")
            .trim_start_matches("https+insecure://")
            .trim_start_matches("tcp://")
            .trim_end_matches('/');
        t == format!("127.0.0.1:{port}")
            || t == format!("localhost:{port}")
            || t == port.to_string()
    };
    let address = |https: bool, host: &str, p: &str| {
        let (scheme, default) = if https {
            ("https", "443")
        } else {
            ("http", "80")
        };
        if p == default {
            format!("{scheme}://{host}/")
        } else {
            format!("{scheme}://{host}:{p}/")
        }
    };
    let tcp = cfg.get("TCP").and_then(Value::as_object);
    let flag = |p: &str, key: &str| {
        tcp.and_then(|t| t.get(p))
            .and_then(|e| e.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    if let Some(web) = cfg.get("Web").and_then(Value::as_object) {
        for (hostport, handler) in web {
            let proxies = handler
                .get("Handlers")
                .and_then(Value::as_object)
                .and_then(|h| h.get("/"))
                .and_then(|h| h.get("Proxy"))
                .and_then(Value::as_str)
                .is_some_and(local);
            if !proxies {
                continue;
            }
            let (host, p) = hostport
                .rsplit_once(':')
                .unwrap_or((hostport.as_str(), "443"));
            // HTTPS is tailscale's default mode, so only an explicit HTTP entry makes it http.
            let https = !(flag(p, "HTTP") && !flag(p, "HTTPS"));
            out.push(address(https, host, p));
        }
    }
    let name = dns_name.trim_end_matches('.');
    for (tport, entry) in tcp.into_iter().flatten() {
        let fwd = entry
            .get("TCPForward")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !local(fwd) {
            continue;
        }
        match entry
            .get("TerminateTLS")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            Some(cert_name) => out.push(address(true, cert_name, tport)),
            None if !name.is_empty() => out.push(address(false, name, tport)),
            None => {}
        }
    }
}

/// The serve this tool recommends: tailnet `port` → localhost `port`, over HTTPS when the tailnet
/// can issue this machine a certificate (`https`), plain HTTP otherwise — never the root on 443.
pub fn serve_command(port: u16, https: bool) -> String {
    let mode = if https { "--https" } else { "--http" };
    format!("tailscale serve --bg {mode} {port} {port}")
}

/// The `tailscale` CLI: on PATH, else the macOS app's bundled one.
fn tailscale() -> Option<std::path::PathBuf> {
    for dir in std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let bin = dir.join("tailscale");
        if bin.is_file() {
            return Some(bin);
        }
    }
    let app = std::path::PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
    app.is_file().then_some(app)
}

fn tailscale_json(bin: &std::path::Path, args: &[&str]) -> Option<Value> {
    let out = std::process::Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// A QR code for `text`, drawn with half-block characters so it fits a terminal. With `paint`,
/// every line is explicitly black on white (ANSI 256-colour 16 on 231): in the terminal's own
/// colours a dark theme draws the code light on dark, a negative a phone's camera may not read.
pub fn qr_text(text: &str, paint: bool) -> Result<String> {
    let code =
        qrcode::QrCode::new(text.as_bytes()).context("encode the pairing address as a QR code")?;
    let plain = code
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Dark)
        .light_color(qrcode::render::unicode::Dense1x2::Light)
        .quiet_zone(true)
        .build();
    if !paint {
        return Ok(plain);
    }
    Ok(plain
        .lines()
        .map(|l| format!("\x1b[38;5;16;48;5;231m{l}\x1b[0m"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Whether to paint the QR code: a terminal, unless `NO_COLOR` says not to; `FORCE_COLOR` paints
/// whatever stdout is.
fn paint_qr() -> bool {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if set("FORCE_COLOR") {
        return true;
    }
    !set("NO_COLOR") && std::io::stdout().is_terminal()
}

/// Ask the monitor listening on 127.0.0.1:`port` for `/pair` as the phone will — through the
/// tailnet name in `Host`, which is what `tailscale serve` hands it — and say what is wrong if it
/// cannot answer: nothing listening, or a build that refuses the name.
fn check_listener(port: u16, base: &str) -> Option<String> {
    use std::io::Read;
    let host = base
        .split_once("://")
        .map_or(base, |(_, rest)| rest)
        .trim_end_matches('/');
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut conn) =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2))
    else {
        return Some(format!(
            "Nothing is listening on 127.0.0.1:{port} yet: start the monitor (agent-monitor --port {port}) \
             before the phone opens the link."
        ));
    };
    conn.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .ok();
    let request = format!("GET /pair HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if conn.write_all(request.as_bytes()).is_err() {
        return Some(format!(
            "The monitor on 127.0.0.1:{port} did not take a request."
        ));
    }
    let mut head = [0u8; 64];
    let n = conn.read(&mut head).unwrap_or(0);
    let status = String::from_utf8_lossy(&head[..n]);
    let code = status.split_whitespace().nth(1).unwrap_or("no answer");
    (code != "200").then(|| {
        format!(
            "The monitor on 127.0.0.1:{port} answered /pair for {host} with {code}: it is not paired, or \
             is a build from before phone pairing — restart it (agent-monitor --port {port})."
        )
    })
}

/// `agent-monitor --pair-phone [--url <base>] [--yes]`.
pub fn run(token: Option<&str>, port: u16, url: Option<String>, yes: bool) -> Result<()> {
    if token.is_none() {
        bail!(
            "this monitor is not paired, so a phone could not be let in by a code either — \
             start it once with `agent-monitor --pair`, then run `agent-monitor --pair-phone`"
        );
    }
    let base = match url {
        Some(u) => {
            let u = u.trim().to_string();
            if u.ends_with('/') {
                u
            } else {
                format!("{u}/")
            }
        }
        None => {
            let Some(bin) = tailscale() else {
                println!(
                    "Tailscale is not installed here. Install it, put the phone on the same \
                     tailnet, and serve the monitor with:\n\n    {}\n\n\
                     Or pass the address the phone should open: agent-monitor --pair-phone --url <address>",
                    serve_command(port, true)
                );
                return Ok(());
            };
            let status = tailscale_json(&bin, &["status", "--json"]).unwrap_or(Value::Null);
            let dns = status
                .pointer("/Self/DNSName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // HTTPS needs a certificate for the name, which the tailnet grants per machine.
            let https = status
                .get("CertDomains")
                .and_then(Value::as_array)
                .is_some_and(|d| !d.is_empty());
            let serve = tailscale_json(&bin, &["serve", "status", "--json"]).unwrap_or(Value::Null);
            let bases = serve_bases(&serve, &dns, port);
            let Some(first) = bases.first().cloned() else {
                let name = dns.trim_end_matches('.');
                println!(
                    "The monitor is not served to your tailnet yet. Serve it once with:\n\n    \
                     {}\n\n\
                     which makes {}://{}:{port}/ reach http://127.0.0.1:{port} for devices on your \
                     tailnet only. Then run `agent-monitor --pair-phone` again.",
                    serve_command(port, https),
                    if https { "https" } else { "http" },
                    if name.is_empty() {
                        "<this machine>.<tailnet>.ts.net"
                    } else {
                        name
                    }
                );
                return Ok(());
            };
            println!("tailscale serve relays this monitor (127.0.0.1:{port}) at:");
            for b in &bases {
                println!("    {b}");
            }
            let own = format!(":{port}/");
            if !first.ends_with(&own) {
                println!(
                    "\nThat serve holds the tailnet name's root, which every tool on this machine shares — \
                     serving another there silently replaces it. The monitor's own port is better:\n\n    {}\n",
                    serve_command(port, https)
                );
            }
            let interactive = std::io::stdin().is_terminal();
            if interactive && !yes {
                print!("Pair a phone through {first} ? [Y/n] ");
                std::io::stdout().flush().ok();
                let mut answer = String::new();
                std::io::stdin().lock().read_line(&mut answer).ok();
                if matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no") {
                    println!("Not paired.");
                    return Ok(());
                }
            }
            first
        }
    };
    let codes = crate::index::state_dir().join(PAIR_CODES_FILE);
    if let Some(dir) = codes.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let code = pairing::mint_pair_code(&codes, pairing::now_secs())
        .context("mint a pairing code (read /dev/urandom, write the state directory)")?;
    let link = format!("{base}pair#code={code}");
    println!("\n{}", qr_text(&link, paint_qr())?);
    println!(
        "Scan with the phone's camera, or open {base}pair and type:\n\n    {}\n\n\
         The code works once, for {} minutes. The phone then stays paired (a cookie); run this \
         again for another device.",
        pairing::display_code(&code),
        pairing::PAIR_CODE_TTL_SECS / 60
    );
    if let Some(problem) = check_listener(port, &base) {
        println!("\nNote: {problem}");
    }
    Ok(())
}

/// The codes file in the monitor's state directory, shared by this CLI and the running monitor.
pub const PAIR_CODES_FILE: &str = "pair-codes";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_https_handler_for_this_port_is_the_address() {
        let serve = serde_json::json!({
            "TCP": {"443": {"HTTPS": true}, "17890": {"TCPForward": "127.0.0.1:7890"}},
            "Web": {"box.tail0.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}}}
        });
        assert_eq!(
            serve_bases(&serve, "box.tail0.ts.net.", 2727),
            vec!["https://box.tail0.ts.net/".to_string()],
            "443 is left out of the address; another port's TCP forward is not this monitor"
        );
    }

    #[test]
    fn an_http_handler_is_http() {
        // `tailscale serve --bg --http 2727 2727`, and one on 80 (left out, as 443 is for https).
        let serve = serde_json::json!({
            "TCP": {"2727": {"HTTP": true}, "80": {"HTTP": true}},
            "Web": {
                "box.tail0.ts.net:2727": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}},
                "box.tail0.ts.net:80": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}}
            }
        });
        assert_eq!(
            serve_bases(&serve, "box.tail0.ts.net.", 2727),
            vec![
                "http://box.tail0.ts.net:2727/".to_string(),
                "http://box.tail0.ts.net/".to_string()
            ]
        );
    }

    #[test]
    fn the_monitors_own_port_comes_first() {
        // This machine's shape on 2026-09-28: the root on 443 AND the recommended same-port serve.
        let serve = serde_json::json!({
            "TCP": {"443": {"HTTPS": true}, "2727": {"HTTPS": true}},
            "Web": {
                "box.tail0.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}},
                "box.tail0.ts.net:2727": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}}
            }
        });
        assert_eq!(
            serve_bases(&serve, "box.tail0.ts.net.", 2727),
            vec![
                "https://box.tail0.ts.net:2727/".to_string(),
                "https://box.tail0.ts.net/".to_string()
            ]
        );
    }

    #[test]
    fn a_foreground_serve_is_found() {
        let serve = serde_json::json!({"Foreground": {"a1b2c3": {
            "TCP": {"2727": {"HTTPS": true}},
            "Web": {"box.tail0.ts.net:2727": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}}}
        }}});
        assert_eq!(
            serve_bases(&serve, "box.tail0.ts.net.", 2727),
            vec!["https://box.tail0.ts.net:2727/".to_string()]
        );
    }

    #[test]
    fn a_raw_forward_is_http_unless_tailscale_terminates_tls() {
        let serve = serde_json::json!({
            "TCP": {
                "9000": {"TCPForward": "localhost:2728"},
                "9443": {"TCPForward": "127.0.0.1:2728", "TerminateTLS": "box.tail0.ts.net"}
            }
        });
        assert_eq!(
            serve_bases(&serve, "box.tail0.ts.net.", 2728),
            vec![
                "http://box.tail0.ts.net:9000/".to_string(),
                "https://box.tail0.ts.net:9443/".to_string()
            ]
        );
    }

    #[test]
    fn nothing_served_for_this_port_is_no_address() {
        let serve = serde_json::json!({"TCP": {"17890": {"TCPForward": "127.0.0.1:7890"}}});
        assert!(serve_bases(&serve, "box.tail0.ts.net.", 2727).is_empty());
        assert!(serve_bases(&Value::Null, "", 2727).is_empty());
    }

    #[test]
    fn the_recommended_serve_is_the_monitors_own_port_never_the_root() {
        assert_eq!(
            serve_command(2727, true),
            "tailscale serve --bg --https 2727 2727"
        );
        assert_eq!(
            serve_command(2727, false),
            "tailscale serve --bg --http 2727 2727"
        );
    }

    #[test]
    fn the_qr_code_draws() {
        let q = qr_text("https://box.tail0.ts.net/pair#code=ABCDEFGH", false).unwrap();
        assert!(q.lines().count() > 10, "{q}");
        assert!(!q.contains('\x1b'), "plain when not painting");
        let painted = qr_text("https://box.tail0.ts.net/pair#code=ABCDEFGH", true).unwrap();
        assert!(
            painted
                .lines()
                .all(|l| l.starts_with("\x1b[38;5;16;48;5;231m") && l.ends_with("\x1b[0m")),
            "every line black on white, whatever the terminal's theme"
        );
        // Dark modules are the drawn (foreground, black) half-blocks; the quiet zone is blank.
        assert!(
            q.lines().next().unwrap().chars().all(|c| c == ' '),
            "quiet zone is light"
        );
    }
}
