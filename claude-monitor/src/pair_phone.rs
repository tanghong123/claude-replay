//! `agent-monitor --pair-phone` (#11): pair a phone with this PAIRED monitor through
//! `tailscale serve`, by a one-time code — never by showing the long-lived token.
//!
//! The owner serves the monitor to their tailnet with `tailscale serve` (HTTPS on the machine's
//! tailnet name, proxied to the loopback port), and a phone on the tailnet opens that. This finds
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
/// status --json`). An HTTPS web handler is `https://<host>[:<port>]/` (443 omitted); a raw TCP
/// forward is `http://<name>:<port>/`.
pub fn serve_bases(serve: &Value, dns_name: &str, port: u16) -> Vec<String> {
    let local = |target: &str| {
        let t = target
            .trim_start_matches("http://")
            .trim_start_matches("https+insecure://")
            .trim_end_matches('/');
        t == format!("127.0.0.1:{port}")
            || t == format!("localhost:{port}")
            || t == port.to_string()
    };
    let mut out = Vec::new();
    if let Some(web) = serve.get("Web").and_then(Value::as_object) {
        for (hostport, cfg) in web {
            let proxies = cfg
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
            out.push(if p == "443" {
                format!("https://{host}/")
            } else {
                format!("https://{host}:{p}/")
            });
        }
    }
    let name = dns_name.trim_end_matches('.');
    if let Some(tcp) = serve.get("TCP").and_then(Value::as_object) {
        for (tport, cfg) in tcp {
            let fwd = cfg.get("TCPForward").and_then(Value::as_str).unwrap_or("");
            if !name.is_empty() && local(fwd) {
                out.push(format!("http://{name}:{tport}/"));
            }
        }
    }
    out.sort();
    out.dedup();
    out
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

/// A QR code for `text`, drawn with half-block characters so it fits a terminal.
pub fn qr_text(text: &str) -> Result<String> {
    let code =
        qrcode::QrCode::new(text.as_bytes()).context("encode the pairing address as a QR code")?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Dark)
        .light_color(qrcode::render::unicode::Dense1x2::Light)
        .quiet_zone(true)
        .build())
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
                     tailnet, and serve the monitor with:\n\n    tailscale serve --bg {port}\n\n\
                     Or pass the address the phone should open: agent-monitor --pair-phone --url <address>"
                );
                return Ok(());
            };
            let dns = tailscale_json(&bin, &["status", "--json"])
                .and_then(|s| {
                    s.pointer("/Self/DNSName")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            let serve = tailscale_json(&bin, &["serve", "status", "--json"]).unwrap_or(Value::Null);
            let bases = serve_bases(&serve, &dns, port);
            let Some(first) = bases.first().cloned() else {
                let name = dns.trim_end_matches('.');
                println!(
                    "The monitor is not served to your tailnet yet. Serve it once with:\n\n    \
                     tailscale serve --bg {port}\n\n\
                     which makes https://{}/ reach http://127.0.0.1:{port} for devices on your \
                     tailnet only. Then run `agent-monitor --pair-phone` again.",
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
    println!("\n{}", qr_text(&link)?);
    println!(
        "Scan with the phone's camera, or open {base}pair and type:\n\n    {}\n\n\
         The code works once, for {} minutes. The phone then stays paired (a cookie); run this \
         again for another device.",
        pairing::display_code(&code),
        pairing::PAIR_CODE_TTL_SECS / 60
    );
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
    fn a_non_default_port_and_a_raw_forward_keep_their_port() {
        let serve = serde_json::json!({
            "TCP": {"9000": {"TCPForward": "localhost:2728"}},
            "Web": {"box.tail0.ts.net:8443": {"Handlers": {"/": {"Proxy": "http://localhost:2728/"}}}}
        });
        assert_eq!(
            serve_bases(&serve, "box.tail0.ts.net.", 2728),
            vec![
                "http://box.tail0.ts.net:9000/".to_string(),
                "https://box.tail0.ts.net:8443/".to_string()
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
    fn the_qr_code_draws() {
        let q = qr_text("https://box.tail0.ts.net/pair#code=ABCDEFGH").unwrap();
        assert!(q.lines().count() > 10, "{q}");
    }
}
