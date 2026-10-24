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

/// Whether a serve target (a web handler's `Proxy`, a TCP entry's `TCPForward`) is this monitor
/// at 127.0.0.1:`port`.
fn targets_monitor(target: &str, port: u16) -> bool {
    let t = target
        .trim_start_matches("http://")
        .trim_start_matches("https+insecure://")
        .trim_start_matches("tcp://")
        .trim_end_matches('/');
    t == format!("127.0.0.1:{port}") || t == format!("localhost:{port}") || t == port.to_string()
}

/// The addresses of [`serve_bases`] the gate may take as the monitor's own (#s170, the owner's
/// yes on s171, 2026-10-19): those the tailnet alone reaches and that serve the monitor alone.
/// tailscaled serves the background setup and every foreground one together, so both tests run
/// across all of them: an address is private only if no setup opens it to Funnel (from the
/// internet), and the monitor's alone only if every handler any setup has at that name and port
/// is the monitor — another app's pages there would share the monitor's origin, and with it the
/// pairing cookie.
pub fn trusted_bases(serve: &Value, dns_name: &str, port: u16) -> Vec<String> {
    let mut setups = vec![serve];
    if let Some(sessions) = serve.get("Foreground").and_then(Value::as_object) {
        setups.extend(sessions.values());
    }
    serve_bases(serve, dns_name, port)
        .into_iter()
        .filter(|base| {
            let Some((host, tport)) = base_hostport(base) else {
                return false;
            };
            let hostport = format!("{host}:{tport}");
            let funnel = setups.iter().any(|setup| {
                setup
                    .get("AllowFunnel")
                    .and_then(|f| f.get(&hostport))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            });
            !funnel
                && setups
                    .iter()
                    .all(|setup| serves_only_monitor(setup, &hostport, tport, port))
        })
        .collect()
}

/// The `name:port` a [`serve_bases`] address is configured under, the scheme's default port
/// written out as tailscale keys it.
fn base_hostport(base: &str) -> Option<(&str, u16)> {
    let (rest, default) = if let Some(r) = base.strip_prefix("https://") {
        (r, 443)
    } else {
        (base.strip_prefix("http://")?, 80)
    };
    let rest = rest.trim_end_matches('/');
    match rest.rsplit_once(':') {
        Some((host, p)) => Some((host, p.parse().ok()?)),
        None => Some((rest, default)),
    }
}

/// Whether one serve setup has nothing at `hostport` (on tailnet port `tport`) but this monitor at
/// 127.0.0.1:`port`, or nothing there at all.
fn serves_only_monitor(setup: &Value, hostport: &str, tport: u16, port: u16) -> bool {
    let Some(entry) = setup.get("TCP").and_then(|t| t.get(tport.to_string())) else {
        return true;
    };
    if let Some(forward) = entry
        .get("TCPForward")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
    {
        return targets_monitor(forward, port);
    }
    setup
        .get("Web")
        .and_then(|w| w.get(hostport))
        .and_then(|site| site.get("Handlers"))
        .and_then(Value::as_object)
        .is_none_or(|handlers| {
            handlers.values().all(|h| {
                targets_monitor(h.get("Proxy").and_then(Value::as_str).unwrap_or(""), port)
            })
        })
}

fn serve_bases_of(cfg: &Value, dns_name: &str, port: u16, out: &mut Vec<String>) {
    let local = |target: &str| targets_monitor(target, port);
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

/// The hosts a request relayed by `tailscale serve` arrives under (#331), from the addresses
/// [`serve_bases`] found: `name:port`, or `name` alone on the scheme's default port — the `Host` a
/// browser sends for each, lower-cased.
pub fn hosts_of(bases: &[String]) -> Vec<String> {
    let mut out: Vec<String> = bases
        .iter()
        .map(|b| {
            b.trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_ascii_lowercase()
        })
        .filter(|h| !h.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The hosts `AGENT_MONITOR_TRUSTED_HOSTS` names (#331): a comma-separated list of `host[:port]`
/// the monitor answers to as its own beside the loopback and its tailnet name — for a proxy other
/// than `tailscale serve` that hands the monitor its own name, and for the cases that stand one up.
pub fn listed_hosts(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

/// The monitor's own hosts beyond the loopback (#331), for [`claude_replay_html::AuthGate`]'s
/// `origin_ok`, which refused every request a phone made through `tailscale serve` (Host: the
/// machine's tailnet name) to read a file or to write: the names `tailscale serve` relays to THIS
/// port privately and to the monitor alone ([`trusted_bases`], #s170), and any
/// `AGENT_MONITOR_TRUSTED_HOSTS` lists — nothing broader (no `*.ts.net`), since the
/// check is the DNS-rebinding and cross-site guard. The serve can be set up after the monitor
/// starts, so the tailnet's names are read again on a miss, at most every 30 s: a stranger's Host
/// costs one `tailscale` read per half-minute, whatever the rate.
///
/// No request waits on `tailscale` (#s151): the names are read in the background, starting now,
/// each read bounded at 3 s (`TAILSCALE_TIMEOUT`), and a check answers from the last names read.
/// Only a check in the first 2 s after this call (`FIRST_WAIT`) waits for the first read, and no
/// longer; a read that failed or timed out keeps the names read before it (#s170).
pub fn trusted_hosts(port: u16) -> claude_replay_html::HostCheck {
    let listed = std::env::var("AGENT_MONITOR_TRUSTED_HOSTS")
        .map(|v| listed_hosts(&v))
        .unwrap_or_default();
    trusted_hosts_from(
        listed,
        std::sync::Arc::new(move || tailnet_hosts(port)),
        REFRESH_AFTER,
        FIRST_WAIT,
    )
}

/// How old the tailnet's names may be before a miss reads them again.
const REFRESH_AFTER: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a check made just after startup waits for the first read of the tailnet's names.
const FIRST_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// How long one `tailscale` status read may take before it is abandoned (#s151). Measured on
/// hong-devserver: 74 ms for both reads together.
const TAILSCALE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A lookup of the tailnet's names: `None` when a read failed or timed out, which keeps the names
/// read before (#s170: a `tailscale` restarting must not cost the phone its writes for 30 s).
type Lookup = std::sync::Arc<dyn Fn() -> Option<Vec<String>> + Send + Sync>;

struct Tailnet {
    state: std::sync::Mutex<TailnetState>,
    read: std::sync::Condvar,
}

#[derive(Default)]
struct TailnetState {
    hosts: Vec<String>,
    /// When the last read finished; `None` before the first.
    read_at: Option<std::time::Instant>,
    reading: bool,
}

impl Tailnet {
    /// Start a read in the background unless one is under way.
    fn refresh(self: &std::sync::Arc<Self>, state: &mut TailnetState, lookup: &Lookup) {
        if state.reading {
            return;
        }
        state.reading = true;
        let (tailnet, lookup) = (self.clone(), lookup.clone());
        std::thread::spawn(move || {
            // A read that panicked is one that gave no names; `reading` must still clear.
            let hosts = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lookup()))
                .ok()
                .flatten();
            let mut state = tailnet.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hosts) = hosts {
                state.hosts = hosts;
            }
            state.read_at = Some(std::time::Instant::now());
            state.reading = false;
            tailnet.read.notify_all();
        });
    }
}

/// [`trusted_hosts`] over any lookup and timings, which is what the cases drive.
fn trusted_hosts_from(
    listed: Vec<String>,
    lookup: Lookup,
    refresh_after: std::time::Duration,
    first_wait: std::time::Duration,
) -> claude_replay_html::HostCheck {
    let tailnet = std::sync::Arc::new(Tailnet {
        state: std::sync::Mutex::new(TailnetState::default()),
        read: std::sync::Condvar::new(),
    });
    let first_by = std::time::Instant::now() + first_wait;
    {
        let mut state = tailnet.state.lock().unwrap_or_else(|e| e.into_inner());
        tailnet.refresh(&mut state, &lookup);
    }
    std::sync::Arc::new(move |host: &str| {
        if listed.iter().any(|h| h == host) {
            return true;
        }
        let mut state = tailnet.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.hosts.iter().any(|h| h == host) {
            return true;
        }
        if state
            .read_at
            .is_some_and(|at| at.elapsed() >= refresh_after)
        {
            tailnet.refresh(&mut state, &lookup);
        }
        while state.read_at.is_none() {
            let left = first_by.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            state = tailnet
                .read
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        state.hosts.iter().any(|h| h == host)
    })
}

/// The hosts this machine's `tailscale serve` relays to 127.0.0.1:`port` — none when Tailscale is
/// not here, is logged out, or serves nothing to this port; `None` when a read failed or timed
/// out, so the names read before stay (#s170).
fn tailnet_hosts(port: u16) -> Option<Vec<String>> {
    let Some(bin) = tailscale() else {
        return Some(Vec::new());
    };
    let status = tailscale_json_within(&bin, &["status", "--json"], TAILSCALE_TIMEOUT)?;
    let dns = status
        .pointer("/Self/DNSName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if dns.is_empty() {
        return Some(Vec::new());
    }
    let serve = tailscale_json_within(&bin, &["serve", "status", "--json"], TAILSCALE_TIMEOUT)?;
    Some(hosts_of(&trusted_bases(&serve, &dns, port)))
}

/// The serve this tool recommends: tailnet `port` → localhost `port`, over HTTPS when the tailnet
/// can issue this machine a certificate (`https`), plain HTTP otherwise — never the root on 443.
pub fn serve_command(port: u16, https: bool) -> String {
    let mode = if https { "--https" } else { "--http" };
    format!("tailscale serve --bg {mode} {port} {port}")
}

/// The `tailscale` CLI: on PATH, else where Homebrew and `go install` put it, else the macOS app's
/// bundled one.
fn tailscale() -> Option<std::path::PathBuf> {
    let var = |name: &str| std::env::var_os(name);
    tailscale_places(var("PATH"), var("GOBIN"), var("GOPATH"), var("HOME"))
        .into_iter()
        .find(|bin| bin.is_file())
}

/// Where [`tailscale`] looks, in order (#s170). A service launchd starts has PATH
/// `/usr/bin:/bin:/usr/sbin:/sbin`, so a Homebrew, `/usr/local/bin` or `go install` build was
/// never found there and no tailnet name was trusted. Absolute places only: a relative PATH entry
/// would find whatever `tailscale` sits in the directory the monitor was started from.
fn tailscale_places(
    path: Option<std::ffi::OsString>,
    gobin: Option<std::ffi::OsString>,
    gopath: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let split = |v: Option<std::ffi::OsString>| -> Vec<PathBuf> {
        v.map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default()
    };
    let mut dirs = split(path);
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.extend(gobin.map(PathBuf::from));
    dirs.extend(split(gopath).into_iter().map(|d| d.join("bin")));
    dirs.extend(home.map(|h| PathBuf::from(h).join("go/bin")));
    let mut places: Vec<PathBuf> = Vec::new();
    for bin in dirs
        .into_iter()
        .filter(|d| d.is_absolute())
        .map(|d| d.join("tailscale"))
    {
        if !places.contains(&bin) {
            places.push(bin);
        }
    }
    places.push(PathBuf::from(
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ));
    places
}

fn tailscale_json(bin: &std::path::Path, args: &[&str]) -> Option<Value> {
    let out = std::process::Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// [`tailscale_json`] bounded by `limit` (#s151): the JSON it printed, or `None` when the read
/// failed, printed no JSON, or did not finish in time (the process is then killed and reaped).
fn tailscale_json_within(
    bin: &std::path::Path,
    args: &[&str],
    limit: std::time::Duration,
) -> Option<Value> {
    use std::io::Read;
    use std::process::Stdio;
    let Ok(mut child) = std::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return None;
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    });
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok().and_then(Result::ok)?;
    if !status.success() {
        return None;
    }
    serde_json::from_slice(&out).ok()
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
            let trusted = trusted_bases(&serve, &dns, port);
            let Some(first) = trusted.first().or(bases.first()).cloned() else {
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
            let untrusted: Vec<&String> = bases.iter().filter(|b| !trusted.contains(b)).collect();
            if !untrusted.is_empty() {
                println!(
                    "\nA phone there can read the page but not files, and cannot write, because the \
                     address is open to Funnel (the internet) or serves something else as well:"
                );
                for b in untrusted {
                    println!("    {b}");
                }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// A lookup the case drives: each read takes the next answer the case sends, and waits for it,
    /// as a hung `tailscale` would; `reads` counts the reads started.
    fn driven_lookup() -> (
        Lookup,
        std::sync::mpsc::Sender<Option<Vec<String>>>,
        std::sync::Arc<AtomicUsize>,
    ) {
        let (send, answers) = std::sync::mpsc::channel::<Option<Vec<String>>>();
        let answers = std::sync::Mutex::new(answers);
        let reads = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = reads.clone();
        let lookup: Lookup = std::sync::Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            answers.lock().unwrap().recv().ok().flatten()
        });
        (lookup, send, reads)
    }

    fn eventually(what: &str, mut done: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < until, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// #s151: a `tailscale` that never answers holds a check only through the first wait after
    /// startup, and no check after it; the names count once the read does finish.
    #[test]
    fn a_hung_tailscale_holds_no_check_past_the_first_wait() {
        let (lookup, send, reads) = driven_lookup();
        let check = trusted_hosts_from(
            vec!["listed.example".into()],
            lookup,
            Duration::from_secs(30),
            Duration::from_millis(300),
        );
        assert!(check("listed.example"), "a listed host needs no read");
        let started = Instant::now();
        assert!(!check("box.tail0.ts.net:2727"));
        let first = started.elapsed();
        assert!(
            first >= Duration::from_millis(250) && first < Duration::from_secs(2),
            "the first check waits for the first read, and no longer than the first wait: {first:?}"
        );
        let started = Instant::now();
        assert!(!check("box.tail0.ts.net:2727"));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "after the first wait a check never waits: {:?}",
            started.elapsed()
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "one read, started with the gate"
        );
        send.send(Some(vec!["box.tail0.ts.net:2727".into()]))
            .unwrap();
        eventually("the names count once read", || {
            check("box.tail0.ts.net:2727")
        });
    }

    /// #s151: a miss on stale names reads them again in the background and answers from the names
    /// it has; one read at a time; a read that timed out keeps the names read before it.
    #[test]
    fn a_stale_miss_reads_again_in_the_background_and_a_timed_out_read_keeps_the_names() {
        let (lookup, send, reads) = driven_lookup();
        send.send(Some(vec!["a.ts.net".into()])).unwrap();
        let check = trusted_hosts_from(Vec::new(), lookup, Duration::ZERO, Duration::from_secs(2));
        assert!(check("a.ts.net"), "the first read's names");
        assert_eq!(reads.load(Ordering::SeqCst), 1, "a hit reads nothing");

        let started = Instant::now();
        assert!(!check("b.ts.net"));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a miss does not wait for the read it starts: {:?}",
            started.elapsed()
        );
        eventually("the miss started a read", || {
            reads.load(Ordering::SeqCst) == 2
        });
        assert!(!check("c.ts.net"));
        assert!(check("a.ts.net"), "the names already read still count");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "no second read while one is under way"
        );

        send.send(None).unwrap();
        eventually("a miss after the timed-out read reads again", || {
            check("d.ts.net");
            reads.load(Ordering::SeqCst) == 3
        });
        assert!(check("a.ts.net"), "the timed-out read kept the names");
        send.send(Some(vec!["a.ts.net".into(), "b.ts.net".into()]))
            .unwrap();
        eventually("a name served later counts once read", || check("b.ts.net"));
    }

    /// #s170: tailscale is found where a service launchd started can't see it on PATH — Homebrew,
    /// /usr/local/bin, GOBIN, each GOPATH's bin, ~/go/bin, then the app — and a relative PATH
    /// entry is never searched.
    #[test]
    fn tailscale_is_looked_for_beyond_a_service_s_path() {
        use std::path::PathBuf;
        let places = tailscale_places(
            Some("/usr/bin:relative/bin:/bin".into()),
            Some("/g/bin".into()),
            Some("/p1:/p2".into()),
            Some("/Users/someone".into()),
        );
        let expect: Vec<PathBuf> = [
            "/usr/bin",
            "/bin",
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/g/bin",
            "/p1/bin",
            "/p2/bin",
            "/Users/someone/go/bin",
        ]
        .iter()
        .map(|d| PathBuf::from(d).join("tailscale"))
        .chain([PathBuf::from(
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        )])
        .collect();
        assert_eq!(places, expect);
        let launchd = tailscale_places(
            Some("/usr/bin:/bin:/usr/sbin:/sbin".into()),
            None,
            None,
            None,
        );
        assert!(launchd.contains(&PathBuf::from("/opt/homebrew/bin/tailscale")));
        let twice = tailscale_places(Some("/opt/homebrew/bin".into()), None, None, None);
        assert_eq!(
            twice
                .iter()
                .filter(|p| p.starts_with("/opt/homebrew"))
                .count(),
            1,
            "a place is listed once"
        );
    }

    /// #s151: a status read that does not finish in time is abandoned, its process killed.
    #[cfg(unix)]
    #[test]
    fn a_tailscale_read_that_runs_too_long_is_abandoned() {
        let dir = std::env::temp_dir().join(format!("cr-tailscale-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("tailscale");
        crate::write_script(&bin, "#!/bin/sh\nexec sleep 30\n");
        let started = Instant::now();
        let read = tailscale_json_within(&bin, &["status", "--json"], Duration::from_millis(200));
        assert!(read.is_none(), "a read past its limit gives no names");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );

        crate::write_script(
            &bin,
            "#!/bin/sh\necho '{\"Self\":{\"DNSName\":\"box.\"}}'\n",
        );
        let read = tailscale_json_within(&bin, &["status", "--json"], Duration::from_secs(5));
        assert_eq!(
            read.and_then(|v| v.pointer("/Self/DNSName").cloned()),
            Some(Value::from("box."))
        );
        crate::write_script(&bin, "#!/bin/sh\nexit 1\n");
        let read = tailscale_json_within(&bin, &["status", "--json"], Duration::from_secs(5));
        assert_eq!(
            read, None,
            "a failed read gives no names, so the names before it stay (#s170)"
        );
        crate::write_script(&bin, "#!/bin/sh\necho 'not json'\n");
        let read = tailscale_json_within(&bin, &["status", "--json"], Duration::from_secs(5));
        assert_eq!(read, None, "nor does a read that prints no JSON");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #331: the hosts a relayed request carries — the `Host` a browser sends for each address the
    /// serve relays to this port, `name:port` or `name` on a default port — and the names an
    /// operator lists, all lower-cased. Nothing else becomes a trusted host.
    #[test]
    fn the_serve_s_addresses_become_the_hosts_the_gate_trusts() {
        let serve = serde_json::json!({
            "TCP": {"2727": {"HTTPS": true}, "443": {"HTTPS": true}},
            "Web": {
                "Box.tail0.ts.net:2727": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}},
                "box.tail0.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}},
                "box.tail0.ts.net:4600": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:4600"}}}
            }
        });
        assert_eq!(
            hosts_of(&serve_bases(&serve, "box.tail0.ts.net.", 2727)),
            vec![
                "box.tail0.ts.net".to_string(),
                "box.tail0.ts.net:2727".to_string()
            ],
            "this port's handlers only; another tool's port is not the monitor's"
        );
        assert_eq!(
            listed_hosts(" phone.test:2805 ,, Proxy.Example:8443"),
            vec![
                "phone.test:2805".to_string(),
                "proxy.example:8443".to_string()
            ]
        );
    }

    /// #s170 (the owner's yes on s171): the gate trusts a relay only when the tailnet alone reaches
    /// it and it serves the monitor alone, judged across the background and every foreground setup.
    #[test]
    fn only_a_private_relay_serving_the_monitor_alone_is_trusted() {
        use serde_json::json;
        let monitor = json!({"Handlers": {"/": {"Proxy": "http://127.0.0.1:2727"}}});
        let base = |extra: serde_json::Value| {
            let mut serve = json!({
                "TCP": {"2727": {"HTTPS": true}, "4600": {"HTTPS": true}},
                "Web": {
                    "box.tail0.ts.net:2727": monitor.clone(),
                    "box.tail0.ts.net:4600": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:4600"}}}
                }
            });
            if let (Some(s), Some(e)) = (serve.as_object_mut(), extra.as_object()) {
                for (k, v) in e {
                    s.insert(k.clone(), v.clone());
                }
            }
            serve
        };
        let trusted = |serve: &serde_json::Value| trusted_bases(serve, "box.tail0.ts.net.", 2727);
        let ours = vec!["https://box.tail0.ts.net:2727/".to_string()];

        assert_eq!(
            trusted(&base(json!({}))),
            ours,
            "private, the monitor alone; another tool's port is no matter"
        );
        assert_eq!(
            trusted(&base(
                json!({"AllowFunnel": {"box.tail0.ts.net:2727": true}})
            )),
            Vec::<String>::new(),
            "open to Funnel"
        );
        assert_eq!(
            trusted(&base(
                json!({"AllowFunnel": {"box.tail0.ts.net:4600": true}})
            )),
            ours,
            "Funnel on another port changes nothing"
        );
        let mut shared = base(json!({}));
        shared["Web"]["box.tail0.ts.net:2727"]["Handlers"]["/other"] =
            json!({"Proxy": "http://127.0.0.1:9000"});
        assert_eq!(
            trusted(&shared),
            Vec::<String>::new(),
            "another app under the same name and port"
        );
        let mut own_path = base(json!({}));
        own_path["Web"]["box.tail0.ts.net:2727"]["Handlers"]["/m"] =
            json!({"Proxy": "http://127.0.0.1:2727"});
        assert_eq!(
            trusted(&own_path),
            ours,
            "a second mount of the monitor itself is still the monitor alone"
        );
        let mut text = base(json!({}));
        text["Web"]["box.tail0.ts.net:2727"]["Handlers"]["/hi"] = json!({"Text": "hello"});
        assert_eq!(
            trusted(&text),
            Vec::<String>::new(),
            "a text handler beside it is something else"
        );

        let foreground_app = base(json!({"Foreground": {"s1": {
            "TCP": {"2727": {"HTTPS": true}},
            "Web": {"box.tail0.ts.net:2727": {"Handlers": {"/other": {"Proxy": "http://127.0.0.1:9000"}}}}
        }}}));
        assert_eq!(
            trusted(&foreground_app),
            Vec::<String>::new(),
            "a terminal's serve of another app at the same address counts"
        );
        let foreground_funnel = base(json!({"Foreground": {"s1": {
            "AllowFunnel": {"box.tail0.ts.net:2727": true}
        }}}));
        assert_eq!(
            trusted(&foreground_funnel),
            Vec::<String>::new(),
            "Funnel from a terminal's serve counts too"
        );

        let tcp = json!({"TCP": {"2727": {"TCPForward": "127.0.0.1:2727"}}});
        assert_eq!(
            trusted_bases(&tcp, "box.tail0.ts.net.", 2727),
            vec!["http://box.tail0.ts.net:2727/".to_string()],
            "a raw TCP forward to the monitor is the monitor alone"
        );
        let tcp_funnel = json!({
            "TCP": {"2727": {"TCPForward": "127.0.0.1:2727"}},
            "AllowFunnel": {"box.tail0.ts.net:2727": true}
        });
        assert_eq!(
            trusted_bases(&tcp_funnel, "box.tail0.ts.net.", 2727),
            Vec::<String>::new()
        );
        let default_port = json!({
            "TCP": {"443": {"HTTPS": true}},
            "Web": {"box.tail0.ts.net:443": monitor.clone()},
            "AllowFunnel": {"box.tail0.ts.net:443": true}
        });
        assert_eq!(
            trusted_bases(&default_port, "box.tail0.ts.net.", 2727),
            Vec::<String>::new(),
            "the default port is keyed as :443 for Funnel too"
        );
    }

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
