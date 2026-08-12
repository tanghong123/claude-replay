//! `claude-monitor-fleet` — several machines' monitors behind one loopback page.
//!
//! `claude-monitor` answers "what is happening on THIS machine" and is documented as
//! single-machine on purpose (`design/claude-monitor.md`: "Not multi-machine. Everything assumes
//! one filesystem and one process table."). That is a good boundary and this crate does not move
//! it: the monitor gains no flag, no remote mode and no extension point. This is a separate
//! binary that opens the mechanism the design already named — an SSH tunnel — once per machine,
//! and serves a switcher whose tabs are those monitors' own pages in iframes.
//!
//! Nothing about the user's setup is assumed. There is no default host list (`config`), no default
//! port ladder (`tunnel`), and the port each monitor serves on is read from that monitor's own
//! lock file rather than guessed (`probe`). Discovery, when asked for, reads the user's own SSH
//! config and adds nothing to it. On a machine with no SSH config and no monitor running, every
//! command still works and reports finding nothing.
//!
//! Read-only and loopback, like the monitor: this process forwards ports and serves one HTML file.

mod config;
mod probe;
mod sshconf;
mod tunnel;

use anyhow::{Context, Result};
use claude_replay_html::{spawn_listener, HttpResponse};
use config::Env;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tunnel::Tunnel;

/// The switcher page — this crate's own markup, `{{VERSION}}` and `{{ENVS}}` substituted.
const FLEET_TEMPLATE: &str = include_str!("fleet.html");

const HELP: &str = "\
claude-monitor-fleet — one page over several machines' claude-monitor instances

USAGE:
  claude-monitor-fleet up [--port N] [--no-open] [--discover]
  claude-monitor-fleet discover [--add] [--host DEST]... [--ssh-config PATH]
  claude-monitor-fleet list | status
  claude-monitor-fleet add NAME [--ssh DEST] [--ssh-option ARG]... [--cache-root PATH] [--port N]
  claude-monitor-fleet remove NAME

  up          Open one SSH tunnel per configured environment and serve the switcher.
              --discover uses what discovery finds instead of the config, without saving it.
  discover    Probe this machine and every literal Host in your SSH config for a running
              monitor. --add writes what it finds to the config.
  status      Probe the configured environments and report, without opening anything.
  add         Add or update one environment. No --ssh means this machine.

The config is a JSON file you can edit: $CLAUDE_MONITOR_FLEET_CONFIG, else
$XDG_CONFIG_HOME/claude-monitor/fleet.json, else ~/.config/claude-monitor/fleet.json.
It ships empty — every host in it is one you or discovery put there.

A monitor's port is READ from its own lock (<cache root>/LOCK), never assumed, so a monitor on
a non-default --port, or a second one under its own $CLAUDE_MONITOR_CACHE, is found as it is.
Local tunnel ports are allocated by the kernel, so nothing collides with what you already run.

Discovery uses ssh BatchMode: a host that needs a passphrase typed is skipped rather than left
hanging. Load your key into an agent, or add that host with `add`, where prompts work.
";

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print!("{HELP}");
        return Ok(());
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("claude-monitor-fleet {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let cmd = args.remove(0);
    match cmd.as_str() {
        "up" => up(&args),
        "discover" => discover(&args),
        "list" => list(),
        "status" => status(),
        "add" => add(&args),
        "remove" => remove(&args),
        other => anyhow::bail!("unknown command {other:?} (try --help)"),
    }
}

/// One environment this process brought up: where it is, and what reaches it from here.
struct Live {
    name: String,
    via: String,
    detail: String,
    /// The loopback port on THIS machine — a tunnel's local end, or the local monitor's own port.
    port: u16,
    /// `None` for this machine: there is nothing to forward.
    tunnel: Option<Tunnel>,
}

impl Live {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

/// The one-column answer to "where is this". Everything that prints an environment goes through
/// here, so "no `ssh` destination means this machine" is decided in exactly one place
/// ([`Env::is_local`]) rather than re-derived at each print site.
fn location(env: &Env) -> &str {
    match &env.ssh {
        Some(dest) if !env.is_local() => dest,
        _ => "this machine",
    }
}

// ---------------------------------------------------------------------------------------------
// up
// ---------------------------------------------------------------------------------------------

fn up(args: &[String]) -> Result<()> {
    let mut port = 0u16; // 0 ⇒ the kernel picks; a fixed default would be a guess about the user's ports
    let mut open_browser = true;
    let mut use_discovery = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => {
                port = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .context("--port needs a number")?
            }
            "--no-open" => open_browser = false,
            "--discover" => use_discovery = true,
            other => anyhow::bail!("up: unknown flag {other:?}"),
        }
    }

    let envs = if use_discovery {
        let found = survey(&Survey::default())?;
        found.into_iter().map(|f| f.env).collect()
    } else {
        let at = config::path()?;
        let fleet = config::load(&at)?;
        anyhow::ensure!(
            !fleet.environments.is_empty(),
            "no environments configured in {}\n  \
             run `claude-monitor-fleet discover --add` to find the ones you have, or \
             `claude-monitor-fleet up --discover` to use them once without saving",
            at.display()
        );
        fleet.environments
    };

    // Sequential on purpose: an SSH passphrase or host-key prompt needs the terminal to itself.
    let mut live: Vec<Live> = Vec::new();
    for env in &envs {
        match bring_up(env) {
            Ok(l) => {
                eprintln!("  {:<20} {:<24} → 127.0.0.1:{}", l.name, l.via, l.port);
                live.push(l);
            }
            // One unreachable machine must not cost the user the others: the fleet comes up
            // without it and says which one and why.
            Err(e) => eprintln!("  {:<20} skipped: {e:#}", env.name),
        }
    }
    anyhow::ensure!(
        !live.is_empty(),
        "nothing came up — no environment answered"
    );

    let envs_json = serde_json::to_string(&serde_json::Value::Array(
        live.iter()
            .map(|l| {
                serde_json::json!({
                    "name": l.name,
                    "url": l.url(),
                    "via": l.via,
                    "detail": l.detail,
                })
            })
            .collect(),
    ))?;
    let page = FLEET_TEMPLATE
        .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
        .replace("{{ENVS}}", &envs_json);

    // The tunnels live here for the rest of the process: dropping this vec takes them down on a
    // normal exit, and each tunnel's stdin pipe takes it down on the paths where no destructor runs
    // (`Ctrl-C`, `kill`) — see `tunnel`. Either way the user gets their ports back.
    let live = Arc::new(Mutex::new(live));
    let handler = {
        let live = live.clone();
        Arc::new(move |name: &str, _query: &str| -> HttpResponse {
            match name {
                "" | "index.html" => HttpResponse::html(page.clone()),
                // Health is probed HERE, not in the page: the tabs are cross-origin, so the
                // browser cannot ask them anything, and this process can.
                "api/fleet" => HttpResponse::json(health_json(&live)),
                _ => HttpResponse::not_found("the fleet serves / and /api/fleet"),
            }
        })
    };
    let bound = spawn_listener(port, handler).with_context(|| format!("bind 127.0.0.1:{port}"))?;
    let url = format!("http://127.0.0.1:{bound}/");
    eprintln!("fleet serving {url} (loopback only — Ctrl-C to stop, tunnels close with it)");
    println!("{url}");
    if open_browser {
        open_url(&url);
    }
    loop {
        std::thread::park();
    }
}

/// Resolve one configured environment to something reachable from here.
fn bring_up(env: &Env) -> Result<Live> {
    let (remote_port, root) = resolve_port(env)?;
    let detail = match (&env.ssh, &root) {
        (Some(h), Some(r)) => format!("{h} — monitor on port {remote_port}, cache root {r}"),
        (Some(h), None) => format!("{h} — monitor on port {remote_port}"),
        (None, Some(r)) => format!("this machine — port {remote_port}, cache root {r}"),
        (None, None) => format!("this machine — port {remote_port}"),
    };
    match &env.ssh {
        None => {
            anyhow::ensure!(
                tunnel::serves_http(remote_port),
                "nothing serves 127.0.0.1:{remote_port} on this machine"
            );
            Ok(Live {
                name: env.name.clone(),
                via: "this machine".into(),
                detail,
                port: remote_port,
                tunnel: None,
            })
        }
        Some(host) => {
            let t = Tunnel::open(host, &env.ssh_options, remote_port, 3)?;
            Ok(Live {
                name: env.name.clone(),
                via: format!("ssh {host}"),
                detail,
                port: t.local_port(),
                tunnel: Some(t),
            })
        }
    }
}

/// The port a configured environment's monitor is on: pinned if the user pinned it, otherwise read
/// from that machine's lock.
///
/// Two monitors and no `cache_root` is refused rather than guessed. Picking one silently is how an
/// aggregator ends up showing yesterday's build next to a tab labelled with today's.
fn resolve_port(env: &Env) -> Result<(u16, Option<String>)> {
    if let Some(p) = env.port {
        return Ok((p, env.cache_root.clone()));
    }
    let found = probe::probe(
        env.ssh.as_deref(),
        &env.ssh_options,
        env.cache_root.as_deref(),
        true,
    )?;
    if let Some(root) = &env.cache_root {
        let hit = found
            .iter()
            .find(|f| &f.root == root)
            .with_context(|| format!("no monitor lock under {root}"))?;
        return Ok((hit.port, Some(hit.root.clone())));
    }
    match found.as_slice() {
        [] => anyhow::bail!(
            "no running monitor found — start one there, or pin it with `add {} --port N`",
            env.name
        ),
        [one] => Ok((one.port, Some(one.root.clone()))),
        many => {
            let list = many
                .iter()
                .map(|f| format!("{} (port {})", f.root, f.port))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "{} runs {} monitors — say which: `add {} --cache-root <one of: {list}>`",
                env.name,
                many.len(),
                env.name
            )
        }
    }
}

fn health_json(live: &Mutex<Vec<Live>>) -> String {
    let mut envs = Vec::new();
    if let Ok(mut guard) = live.lock() {
        for l in guard.iter_mut() {
            let tunnel_up = match &mut l.tunnel {
                Some(t) => t.is_up(),
                None => true,
            };
            let code = if tunnel_up {
                tunnel::status_code(l.port)
            } else {
                None
            };
            envs.push(serde_json::json!({
                "name": l.name,
                "up": code.is_some(),
                "code": code,
                "why": if !tunnel_up {
                    "the ssh tunnel exited"
                } else if code.is_none() {
                    "the monitor is not answering"
                } else { "" },
            }));
        }
    }
    serde_json::json!({ "envs": envs }).to_string()
}

/// Best effort, never fatal — the URL is on stdout either way.
fn open_url(url: &str) {
    // `$BROWSER` first: on a machine where the desktop opener is wrong or absent, this is the
    // only thing the user can actually set.
    if let Some(browser) = std::env::var_os("BROWSER").filter(|b| !b.is_empty()) {
        let _ = std::process::Command::new(browser)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        return;
    }
    #[cfg(target_os = "macos")]
    let prog = "open";
    #[cfg(target_os = "windows")]
    let prog = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let prog = "xdg-open";
    let _ = std::process::Command::new(prog)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ---------------------------------------------------------------------------------------------
// discover / status / list / add / remove
// ---------------------------------------------------------------------------------------------

/// What to look at. Empty by default in every field that could name a machine.
#[derive(Default)]
struct Survey {
    /// Extra destinations to probe beyond the SSH config's own literal hosts.
    hosts: Vec<String>,
    /// Override the SSH config to read. `None` ⇒ the default location.
    ssh_config: Option<std::path::PathBuf>,
}

/// One monitor discovery found, as a config entry plus what it took to describe it.
struct Discovered {
    env: Env,
    port: u16,
    alive: Option<bool>,
}

/// Probe this machine and every candidate host, in parallel, and report every monitor found.
///
/// The candidate list is the user's own: literal `Host` entries from their SSH config, plus
/// anything passed with `--host`. This function invents no destination — with no SSH config and no
/// local monitor it returns nothing, which is the correct answer on such a machine.
fn survey(what: &Survey) -> Result<Vec<Discovered>> {
    let ssh_config = what
        .ssh_config
        .clone()
        .or_else(sshconf::default_path)
        .unwrap_or_default();
    let mut hosts = sshconf::candidates(&ssh_config);
    for h in &what.hosts {
        if !hosts.contains(h) {
            hosts.push(h.clone());
        }
    }
    eprintln!(
        "probing this machine and {} host(s) from {}",
        hosts.len(),
        ssh_config.display()
    );

    // One thread per host. They are all blocked on ssh, and doing them in series makes a
    // twenty-host config a two-minute wait.
    let mut out = Vec::new();
    // One machine reached through two `Host` aliases (`box` and `box.internal`, a direct route and
    // one through a jump host) is still ONE monitor, and offering it twice would give the user two
    // tabs showing the identical page. The first destination to report a monitor keeps it: this
    // machine is probed first, so a monitor here is never offered as a tunnel back to itself, and
    // remote aliases fall back to the order the user's own ssh config lists them in.
    let mut seen: HashMap<(String, String, u32), String> = HashMap::new();
    std::thread::scope(|scope| {
        let mut jobs = Vec::new();
        // `None` is this machine, and it is a candidate like any other — not assumed to have a
        // monitor, not assumed to be interesting.
        jobs.push((None, scope.spawn(|| probe::probe(None, &[], None, false))));
        for host in &hosts {
            let h = host.clone();
            jobs.push((
                Some(host.clone()),
                scope.spawn(move || probe::probe(Some(&h), &[], None, false)),
            ));
        }
        for (host, job) in jobs {
            let Ok(Ok(found)) = job.join() else { continue };
            let multiple = found.len() > 1;
            for f in found {
                let key = probe::machine_key(host.as_deref(), &f);
                let here = host.clone().unwrap_or_else(|| "this machine".into());
                if let Some(kept) = seen.get(&key) {
                    eprintln!(
                        "  {here} is the same monitor as {kept} (one machine, two names) — skipped"
                    );
                    continue;
                }
                seen.insert(key, here);
                out.push(Discovered {
                    env: Env {
                        name: probe::suggest_name(host.as_deref(), &f.root),
                        ssh: host.clone(),
                        ssh_options: Vec::new(),
                        // Recorded only when it is needed to tell two monitors on one machine
                        // apart. Pinning it otherwise would break the day the user moves it.
                        cache_root: multiple.then(|| f.root.clone()),
                        port: None,
                    },
                    port: f.port,
                    alive: f.alive,
                });
            }
        }
    });
    out.sort_by(|a, b| a.env.name.cmp(&b.env.name));
    Ok(out)
}

fn discover(args: &[String]) -> Result<()> {
    let mut what = Survey::default();
    let mut save = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--add" => save = true,
            "--host" => what
                .hosts
                .push(it.next().context("--host needs a destination")?.clone()),
            "--ssh-config" => {
                what.ssh_config = Some(
                    it.next()
                        .context("--ssh-config needs a path")?
                        .clone()
                        .into(),
                )
            }
            other => anyhow::bail!("discover: unknown flag {other:?}"),
        }
    }

    let found = survey(&what)?;
    if found.is_empty() {
        println!("no monitors found");
        println!(
            "  a monitor publishes its port when it binds; if one is running somewhere this did \
             not look, add it with `add NAME --ssh DEST` or `--cache-root PATH`"
        );
        return Ok(());
    }
    for d in &found {
        println!(
            "{:<22} {:<20} port {:<6} {}{}",
            d.env.name,
            location(&d.env),
            d.port,
            d.env.cache_root.as_deref().unwrap_or(""),
            match d.alive {
                Some(false) => "  (lock pid not signalable from there)",
                _ => "",
            }
        );
    }
    if !save {
        println!(
            "\n{} found — `discover --add` writes them to the config",
            found.len()
        );
        return Ok(());
    }
    let at = config::path()?;
    let mut fleet = config::load(&at)?;
    for d in found {
        let name = d.env.name.clone();
        if fleet.upsert(d.env) {
            println!("~ updated {name}");
        } else {
            println!("+ added {name}");
        }
    }
    config::save(&at, &fleet)?;
    println!("wrote {}", at.display());
    Ok(())
}

fn list() -> Result<()> {
    let at = config::path()?;
    let fleet = config::load(&at)?;
    if fleet.environments.is_empty() {
        println!("no environments in {}", at.display());
        println!("  `claude-monitor-fleet discover --add` looks through your own SSH config");
        return Ok(());
    }
    for e in &fleet.environments {
        println!(
            "{:<22} {:<20} {}{}",
            e.name,
            location(e),
            match e.port {
                Some(p) => format!("port {p} (pinned)  "),
                None => "port discovered  ".to_string(),
            },
            e.cache_root.as_deref().unwrap_or("")
        );
    }
    println!("\n{}", at.display());
    Ok(())
}

fn status() -> Result<()> {
    let at = config::path()?;
    let fleet = config::load(&at)?;
    anyhow::ensure!(
        !fleet.environments.is_empty(),
        "no environments in {} — try `discover --add`",
        at.display()
    );
    for e in &fleet.environments {
        match resolve_port(e) {
            Ok((port, root)) => println!(
                "{:<22} {:<20} port {:<6} {}",
                e.name,
                location(e),
                port,
                root.unwrap_or_default()
            ),
            Err(err) => println!("{:<22} {:<20} {err:#}", e.name, location(e)),
        }
    }
    Ok(())
}

fn add(args: &[String]) -> Result<()> {
    let mut it = args.iter();
    let name = it.next().context("add needs a NAME")?.clone();
    let mut env = Env {
        name: name.clone(),
        ..Default::default()
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ssh" => env.ssh = Some(it.next().context("--ssh needs a destination")?.clone()),
            "--ssh-option" => env
                .ssh_options
                .push(it.next().context("--ssh-option needs an argument")?.clone()),
            "--cache-root" => {
                env.cache_root = Some(it.next().context("--cache-root needs a path")?.clone())
            }
            "--port" => {
                env.port = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .context("--port needs a number")?,
                )
            }
            other => anyhow::bail!("add: unknown flag {other:?}"),
        }
    }
    let at = config::path()?;
    let mut fleet = config::load(&at)?;
    let updated = fleet.upsert(env);
    config::save(&at, &fleet)?;
    println!(
        "{} {name} in {}",
        if updated { "updated" } else { "added" },
        at.display()
    );
    Ok(())
}

fn remove(args: &[String]) -> Result<()> {
    let name = args.first().context("remove needs a NAME")?;
    let at = config::path()?;
    let mut fleet = config::load(&at)?;
    anyhow::ensure!(fleet.remove(name), "no environment called {name:?}");
    config::save(&at, &fleet)?;
    println!("removed {name} from {}", at.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pinned port is used as given, and never probed for — the escape hatch for a monitor whose
    /// lock this machine cannot read (another user's, a container's).
    #[test]
    fn a_pinned_port_is_taken_at_its_word() {
        let env = Env {
            name: "pinned".into(),
            ssh: Some("nowhere.invalid".into()),
            port: Some(45678),
            ..Default::default()
        };
        assert_eq!(resolve_port(&env).unwrap(), (45678, None));
    }

    /// With nothing running and nothing pinned, the failure names the remedy instead of picking a
    /// port. This is the test that would fail if a default like 2727 ever crept back in.
    #[test]
    fn an_absent_monitor_is_reported_not_guessed() {
        let root = std::env::temp_dir().join(format!("cmf-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let env = Env {
            name: "here".into(),
            cache_root: Some(root.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let e = resolve_port(&env).expect_err("no lock there");
        assert!(
            e.to_string().contains("no monitor lock under"),
            "must not fall back to a port: {e}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The page is data-driven: with no environments it says so, and it never contains a host or a
    /// port this crate chose.
    #[test]
    fn the_page_carries_only_what_the_server_puts_in_it() {
        let page = FLEET_TEMPLATE
            .replace("{{VERSION}}", "0.0.0-test")
            .replace("{{ENVS}}", "[]");
        assert!(!page.contains("{{"), "every placeholder is substituted");
        assert!(page.contains("const ENVS = [];"));
        assert!(
            page.contains("discover --add"),
            "an empty fleet tells the user how to fill it"
        );
    }

    /// Health is reported per environment, and a local entry with nothing behind it reads as down
    /// rather than as "no tunnel, therefore fine".
    #[test]
    fn health_reports_a_dead_target_as_down() {
        let dead = tunnel::free_port().unwrap();
        let live = Mutex::new(vec![Live {
            name: "gone".into(),
            via: "this machine".into(),
            detail: String::new(),
            port: dead,
            tunnel: None,
        }]);
        let json: serde_json::Value = serde_json::from_str(&health_json(&live)).unwrap();
        let env = &json["envs"][0];
        assert_eq!(env["name"], "gone");
        assert_eq!(env["up"], false);
        assert_eq!(env["why"], "the monitor is not answering");
    }
}
