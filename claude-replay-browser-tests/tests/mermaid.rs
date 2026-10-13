//! Transcript Mermaid rendering on both pages, with the real bundled browser runtime.

mod harness;

use claude_replay_html::start_server;
use claude_replay_present::Args;
use harness::{assistant_at, base, copied_text, eval, serial, stub_clipboard, until, user_at};
use std::time::Duration;

const SID: &str = "eeeeeeee-0000-4000-8000-000000000090";
const FLOW: &str = "flowchart LR\n  INPUT[\"Example source\"] -->|Sample item| PARSER[\"Demo parser\"]\n  PARSER -->|Parsed item| VALIDATOR[\"Demo validator<br/>Check example fields\"]\n  VALIDATOR -->|Validated item| TRANSFORM[\"Demo transform\"]\n  TRANSFORM -->|Mock output| STORE[\"Sample store\"]\n  STORE -->|Example event| QUEUE[\"Sample queue\"]\n  QUEUE -->|Example dispatch| WORKER[\"Demo worker\"]\n  WORKER -->|Invented result| VIEW[\"Demo view\"]";
const SEQUENCE: &str = "sequenceDiagram\n  participant A as Agent\n  participant M as Monitor\n  A->>M: Inline renderer\n  M-->>A: Diagram ready";

fn assistant_markdown(text: &str, timestamp: &str) -> String {
    let encoded = serde_json::to_string(text).unwrap();
    assistant_at(&encoded[1..encoded.len() - 1], timestamp)
}

fn transcript() -> String {
    user_at("Please show the architecture inline.", &harness::at("15:00"))
        + &assistant_markdown(
            &format!("```mermaid\n{FLOW}\n```\n\n```mermaid\n{SEQUENCE}\n```\n\n```mermaid\nthis is not a diagram\n```\n\n```mermaid\npie title Diagram after a parse error\n  \"Ready\" : 1\n```\n\n```rust\nlet ordinary_code = 42;\n```"),
            &harness::at("15:01"),
        )
}

fn wait_diagrams(tab: &headless_chrome::Tab, count: usize) {
    until(
        tab,
        &format!("document.querySelectorAll('[data-mermaid-state=ready] svg').length === {count}"),
        "Mermaid diagrams to render inside their messages",
        Duration::from_secs(30),
        "[...document.querySelectorAll('[data-mermaid]')].map(e => ({state:e.dataset.mermaidState, error:e.querySelector('.mermaid-error').textContent}))",
    );
}

fn assert_diagrams(tab: &headless_chrome::Tab) {
    wait_diagrams(tab, 3);
    assert_eq!(eval(tab, "(() => { const svg = document.querySelector('[data-mermaid-state=ready] svg'); return getComputedStyle(svg.querySelector('.node rect')).fill === 'rgb(255, 255, 255)' && [...svg.querySelectorAll('.node text')].every(text => getComputedStyle(text).fontSize === '14px'); })()"), true);
    assert_eq!(
        eval(
            tab,
            "document.querySelectorAll('[data-mermaid-state=error]').length"
        ),
        1
    );
    assert_eq!(
        eval(
            tab,
            "document.querySelector('[data-mermaid-state=error] pre').hidden"
        ),
        false
    );
    assert_eq!(
        eval(
            tab,
            "document.querySelector('[data-mermaid-state=error] .mermaid-error').hidden"
        ),
        false
    );
    assert_eq!(
        eval(
            tab,
            "document.querySelector('[data-mermaid-state=ready] pre').hidden"
        ),
        true
    );
    assert_eq!(
        eval(
            tab,
            "document.querySelector('.fence:not([data-mermaid]) pre').textContent"
        ),
        "let ordinary_code = 42;"
    );
    assert!(eval(
        tab,
        "document.querySelector('[data-mermaid-state=ready] svg').textContent"
    )
    .as_str()
    .unwrap()
    .contains("Demo view"));
    assert_eq!(
        eval(tab, "document.querySelectorAll('.mdrev-host').length"),
        0
    );
    assert_eq!(eval(tab, "performance.getEntriesByType('resource').filter(e => e.name.includes('/mdrev/') || e.name.includes('/api/mdrev')).length"), 0);
    assert_eq!(eval(tab, "[...document.querySelectorAll('[data-mermaid] svg')].map(e=>e.id).length === new Set([...document.querySelectorAll('[data-mermaid] svg')].map(e=>e.id)).size"), true);

    stub_clipboard(tab);
    eval(
        tab,
        "document.querySelector('[data-mermaid] .cpy').click(); true",
    );
    until(
        tab,
        "window.__copied !== null",
        "copy to capture the Mermaid source",
        Duration::from_secs(5),
        "window.__copied",
    );
    assert_eq!(copied_text(tab), FLOW);
    eval(
        tab,
        "document.querySelector('[data-mermaid] .mermaid-source').click(); true",
    );
    assert_eq!(
        eval(tab, "document.querySelector('[data-mermaid] pre').hidden"),
        false
    );
    assert_eq!(
        eval(
            tab,
            "document.querySelector('[data-mermaid] .mermaid-diagram').hidden"
        ),
        true
    );
    eval(
        tab,
        "document.querySelector('[data-mermaid] .mermaid-source').click(); true",
    );
    assert_eq!(
        eval(tab, "document.querySelector('[data-mermaid] pre').hidden"),
        true
    );

    let light_id = eval(tab, "document.querySelector('[data-mermaid] svg').id");
    eval(tab, "document.documentElement.dataset.theme = 'dark'; true");
    until(tab, &format!("document.querySelector('[data-mermaid-state=ready] svg')?.id !== {} && document.querySelectorAll('[data-mermaid-state=ready] svg').length === 3", light_id), "diagrams to redraw in the dark theme", Duration::from_secs(30), "[...document.querySelectorAll('[data-mermaid]')].map(e=>e.dataset.mermaidState)");
    assert_eq!(eval(tab, "[...document.querySelectorAll('[data-mermaid] svg')].every(e=>e.getBoundingClientRect().height > 10)"), true);
    assert_eq!(eval(tab, "getComputedStyle(document.querySelector('[data-mermaid-state=ready] svg .node rect')).fill === 'rgb(35, 42, 56)'"), true);
    assert_eq!(
        eval(
            tab,
            "document.querySelectorAll('[id^=dtranscript-mermaid-]').length"
        ),
        0
    );
    eval(tab, "(() => { const input = document.getElementById('transcriptSearchInput') || document.getElementById('q'); input.value = 'flowchart'; input.dispatchEvent(new Event('input', {bubbles:true})); (document.getElementById('findNext') || document.getElementById('qnext')).click(); return true; })()");
    until(tab, "[...document.querySelectorAll('[data-mermaid] pre mark')].some(mark=>mark.getBoundingClientRect().height > 0)", "a search for diagram syntax to reveal its source", Duration::from_secs(10), "[...document.querySelectorAll('[data-mermaid] pre')].map(e=>({hidden:e.hidden, text:e.textContent}))");
    eval(tab, "(() => { const input = document.getElementById('transcriptSearchInput') || document.getElementById('q'); input.value = ''; input.dispatchEvent(new Event('input', {bubbles:true})); return true; })()");
    assert_eq!(
        eval(
            tab,
            "document.querySelectorAll('mark.hl, mark.search-mark').length"
        ),
        0
    );
    eval(tab, "(() => { const input = document.getElementById('transcriptSearchInput') || document.getElementById('q'); input.value = 'Rendering diagram'; input.dispatchEvent(new Event('input', {bubbles:true})); (document.getElementById('findNext') || document.getElementById('qnext')).click(); return true; })()");
    until(
        tab,
        "(document.getElementById('transcriptSearchCount') || document.getElementById('qcount')).textContent === '0 hits'",
        "generated rendering status to stay outside the searchable transcript",
        Duration::from_secs(5),
        "(document.getElementById('transcriptSearchCount') || document.getElementById('qcount')).textContent",
    );
    eval(tab, "(() => { const input = document.getElementById('transcriptSearchInput') || document.getElementById('q'); input.value = ''; input.dispatchEvent(new Event('input', {bubbles:true})); return true; })()");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_transcript_renders_mermaid_inline_and_preserves_source() {
    let _serial = serial();
    let base = base("mermaid-classic");
    let stores = harness::Stores::new(&base);
    let path = stores.claude_session(SID, &transcript());
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    let server = start_server(
        &Args {
            no_cache: true,
            ..Default::default()
        },
        &[path],
    )
    .unwrap();
    let (_browser, tab) = harness::chrome_tab();
    tab.navigate_to(&format!("{}&mountall=1", server.url_for_root(0).unwrap()))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    assert_diagrams(&tab);
    assert_eq!(eval(&tab, "performance.getEntriesByType('resource').filter(e=>e.name.endsWith('/mermaid.js')).length"), 1);
}

#[test]
#[ignore = "needs a local Chrome and both built monitors"]
fn both_monitors_render_mermaid_in_the_app_transcript_and_live_updates() {
    let _serial = serial();
    for (kind, port) in [(harness::Kind::V1, 3041), (harness::Kind::V2, 3042)] {
        let base = base(&format!("mermaid-app-{port}"));
        let stores = harness::Stores::new(&base);
        let path = stores.claude_session(SID, &transcript());
        let monitor = harness::Monitor::spawn(kind, port, &base, Some(&stores), true);
        let (_browser, tab) = harness::chrome_tab();
        monitor.pair(&tab);
        monitor.open(&tab, &format!("?session={SID}&ui=app&mountall=1"));
        assert_diagrams(&tab);
        assert_eq!(
            eval(
                &tab,
                "document.querySelectorAll('#app [data-mermaid] svg').length"
            ),
            3
        );
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(
                assistant_markdown(
                    "```mermaid\nflowchart TD\n  A[Incremental message] --> B[Fresh diagram]\n```",
                    &harness::at("15:02"),
                )
                .as_bytes(),
            )
            .unwrap();
        wait_diagrams(&tab, 4);
        assert_eq!(eval(&tab, "[...document.querySelectorAll('[data-mermaid] svg')].some(e=>e.textContent.includes('Incremental') && e.textContent.includes('Fresh'))"), true);
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn offline_html_renders_mermaid_without_a_server() {
    let _serial = serial();
    let base = base("mermaid-offline");
    let stores = harness::Stores::new(&base);
    let path = stores.claude_session(SID, &transcript());
    let output = base.join("transcript.html");
    claude_replay_html::dump_html(
        &Args {
            dump_html: Some(Some(base.join("transcript").to_string_lossy().into_owned())),
            no_cache: true,
            ..Default::default()
        },
        &path,
    )
    .unwrap();
    let (_browser, tab) = harness::chrome_tab();
    tab.navigate_to(&format!("file://{}?mountall=1", output.display()))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    assert_diagrams(&tab);
    assert_eq!(
        eval(
            &tab,
            "performance.getEntriesByType('resource').filter(e=>/^https?:/.test(e.name)).length"
        ),
        0
    );
}

fn assert_source_toggle_preserves_the_readers_position(tab: &headless_chrome::Tab) {
    wait_diagrams(tab, 1);
    until(
        tab,
        "window.__viewportTrace?.at(-1)?.following === true",
        "the short diagram to settle while following the tail",
        Duration::from_secs(10),
        "window.__viewportTrace?.slice(-3)",
    );
    std::thread::sleep(Duration::from_millis(500));
    let before = harness::probe(tab, "(() => { const scroller = document.querySelector('.transcript') || document.scrollingElement; const button = document.querySelector('.mermaid-source'); window.__mermaidToggleProbe = {scroller,button,card:button.closest('[data-mermaid]')}; return {top:scroller.scrollTop, header:button.getBoundingClientRect().top}; })()");
    eval(
        tab,
        "document.querySelector('.mermaid-source').click(); true",
    );
    std::thread::sleep(Duration::from_millis(600));
    let after = harness::probe(tab, "(() => { const {scroller,button,card} = window.__mermaidToggleProbe; return {top:scroller.scrollTop, header:button.getBoundingClientRect().top, sourceShown:!card.querySelector('pre').hidden, following:window.__viewportTrace.at(-1)?.following, grew:scroller.scrollHeight > scroller.clientHeight + 500}; })()");
    assert_eq!(after["sourceShown"], true);
    assert_eq!(
        after["grew"], true,
        "the source must grow beyond the viewport"
    );
    assert_eq!(
        after["following"], false,
        "a source toggle releases the tail"
    );
    for coordinate in ["top", "header"] {
        assert!(
            (after[coordinate].as_f64().unwrap() - before[coordinate].as_f64().unwrap()).abs()
                < 2.0,
            "source toggle moved {coordinate}: before={before}, after={after}"
        );
    }
}

#[test]
#[ignore = "needs a local Chrome and both built monitors"]
fn opening_tall_mermaid_source_releases_follow_and_keeps_its_header_visible() {
    let _serial = serial();
    let base = base("mermaid-source-position");
    let stores = harness::Stores::new(&base);
    let diagram = "flowchart LR\n A[Sample input] --> B[Mock output]\n".to_owned()
        + &"%% Invented comment that increases source height without changing the diagram.\n"
            .repeat(80);
    let path = stores.claude_session(
        SID,
        &(user_at("Show a synthetic example.", &harness::at("15:00"))
            + &assistant_markdown(
                &format!("```mermaid\n{diagram}\n```"),
                &harness::at("15:01"),
            )),
    );
    for (kind, port, shell) in [
        (harness::Kind::V1, 3046, "app"),
        (harness::Kind::V2, 3047, "app"),
        (harness::Kind::V2, 3048, "classic"),
    ] {
        let monitor_root = base.join(format!("{port}"));
        let monitor = harness::Monitor::spawn(kind, port, &monitor_root, Some(&stores), true);
        let (_browser, tab) = harness::chrome_tab();
        monitor.pair(&tab);
        monitor.open(
            &tab,
            &format!("?session={SID}&ui={shell}&mountall=1&trace=viewport"),
        );
        assert_source_toggle_preserves_the_readers_position(&tab);
    }
    let output = base.join("transcript.html");
    claude_replay_html::dump_html(
        &Args {
            dump_html: Some(Some(base.join("transcript").to_string_lossy().into_owned())),
            no_cache: true,
            ..Default::default()
        },
        &path,
    )
    .unwrap();
    let (_browser, tab) = harness::chrome_tab();
    tab.navigate_to(&format!(
        "file://{}?mountall=1&trace=viewport",
        output.display()
    ))
    .unwrap();
    tab.wait_until_navigated().unwrap();
    assert_source_toggle_preserves_the_readers_position(&tab);
}

#[test]
#[ignore = "needs a local Chrome"]
fn mermaid_cannot_relax_the_transcripts_security_configuration() {
    let _serial = serial();
    let base = base("mermaid-security");
    let stores = harness::Stores::new(&base);
    let body = "%%{init: {'securityLevel':'loose','htmlLabels':true,'theme':'forest','look':'handDrawn','themeVariables':{'fontSize':'40px','primaryColor':'#ff0000'}}}%%\nflowchart LR\n  A[\"<img src=x onerror=alert(1)>\"] --> B[Safe label]\n  click B call alert()";
    let path = stores.claude_session(
        SID,
        &(user_at("Show this diagram safely.", &harness::at("15:00"))
            + &assistant_markdown(&format!("```mermaid\n{body}\n```"), &harness::at("15:01"))),
    );
    let server = start_server(
        &Args {
            no_cache: true,
            ..Default::default()
        },
        &[path],
    )
    .unwrap();
    let (_browser, tab) = harness::chrome_tab();
    tab.navigate_to(&format!("{}&mountall=1", server.url_for_root(0).unwrap()))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    wait_diagrams(&tab, 1);
    assert_eq!(eval(&tab, "(() => { const svg = document.querySelector('[data-mermaid] svg'); return getComputedStyle(svg.querySelector('.node rect')).fill === 'rgb(255, 255, 255)' && getComputedStyle(svg.querySelector('.node text')).fontSize === '14px'; })()"), true);
    assert_eq!(eval(&tab, "document.querySelectorAll('[data-mermaid] img, [data-mermaid] foreignObject, [data-mermaid] script').length"), 0);
    assert_eq!(eval(&tab, "(() => { window.__mermaidCallback = false; window.alert = () => { window.__mermaidCallback = true; }; document.querySelector('[data-mermaid] .clickable')?.dispatchEvent(new MouseEvent('click', {bubbles:true})); return window.__mermaidCallback; })()"), false);
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('[data-mermaid] pre').textContent"
        ),
        body
    );
}

struct ImageRequestProbe {
    url: String,
    requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ImageRequestProbe {
    fn start() -> Self {
        use std::io::{BufRead, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/invented.png", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let counter = requests.clone();
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            let image = harness::base64_bytes("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=");
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                                break;
                            }
                        }
                        counter.fetch_add(1, Ordering::Relaxed);
                        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", image.len()).unwrap();
                        socket.write_all(&image).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accepting a synthetic image request failed: {error}"),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for ImageRequestProbe {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn assert_no_automatic_image_requests(tab: &headless_chrome::Tab, probe: &ImageRequestProbe) {
    until(
        tab,
        "document.querySelectorAll('[data-mermaid]').length === 3 && [...document.querySelectorAll('[data-mermaid]')].every(card => ['ready','error'].includes(card.dataset.mermaidState))",
        "resource-bearing and ordinary diagrams to finish",
        Duration::from_secs(30),
        "JSON.stringify({url:location.href,iframes:document.querySelectorAll('iframe').length,cards:[...document.querySelectorAll('[data-mermaid]')].map(card=>({state:card.dataset.mermaidState,error:card.querySelector('.mermaid-error').textContent}))})",
    );
    assert_eq!(probe.requests.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(
        eval(
            tab,
            "document.querySelectorAll('[data-mermaid-state=error]').length"
        ),
        2
    );
    assert_eq!(eval(tab, "[...document.querySelectorAll('[data-mermaid-state=error]')].every(card => !card.querySelector('pre').hidden && card.querySelector('.mermaid-error').textContent.includes('not supported'))"), true);
    wait_diagrams(tab, 1);
    assert_eq!(eval(tab, "document.querySelector('[data-mermaid-state=ready] svg').textContent.includes('Ordinary control')"), true);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn transcript_diagrams_do_not_load_flowchart_images_or_sequence_actor_icons() {
    let _serial = serial();
    let probe = ImageRequestProbe::start();
    let base = base("mermaid-network-resources");
    let stores = harness::Stores::new(&base);
    let body = format!("```mermaid\nflowchart LR\n A@{{ img: \"{}\", label: \"Invented image\", w: 40, h: 40 }} --> B[Sample]\n```\n\n```mermaid\nsequenceDiagram\n participant A as Example actor\n properties A: {{\"icon\":\"{}\"}}\n A->>A: Invented message\n```\n\n```mermaid\nflowchart LR\n A[Ordinary control] --> B[Mock output]\n```", probe.url, probe.url);
    let path = stores.claude_session(
        SID,
        &(user_at("Show synthetic diagrams.", &harness::at("15:00"))
            + &assistant_markdown(&body, &harness::at("15:01"))),
    );
    let monitor = harness::Monitor::spawn(harness::Kind::V2, 3049, &base, Some(&stores), true);
    for shell in ["app", "classic"] {
        let (_browser, tab) = harness::chrome_tab();
        monitor.pair(&tab);
        monitor.open(&tab, &format!("?session={SID}&ui={shell}&mountall=1"));
        assert_no_automatic_image_requests(&tab, &probe);
    }
    claude_replay_html::dump_html(
        &Args {
            dump_html: Some(Some(base.join("transcript").to_string_lossy().into_owned())),
            no_cache: true,
            ..Default::default()
        },
        &path,
    )
    .unwrap();
    let (_browser, tab) = harness::chrome_tab();
    tab.navigate_to(&format!(
        "file://{}?mountall=1",
        base.join("transcript.html").display()
    ))
    .unwrap();
    tab.wait_until_navigated().unwrap();
    assert_no_automatic_image_requests(&tab, &probe);
}

#[test]
#[ignore = "needs a local Chrome"]
fn mermaid_loads_lazily_and_a_failed_load_keeps_source_and_can_retry() {
    let _serial = serial();
    let base = base("mermaid-lazy-failure");
    let stores = harness::Stores::new(&base);
    let path = stores.claude_session(
        SID,
        &(user_at("Show a diagram when it arrives.", &harness::at("15:00"))
            + &assistant_at("Waiting for the next message.", &harness::at("15:01"))),
    );
    let server = start_server(
        &Args {
            no_cache: true,
            ..Default::default()
        },
        std::slice::from_ref(&path),
    )
    .unwrap();
    let (_browser, tab) = harness::chrome_tab();
    tab.navigate_to(&format!("{}&mountall=1", server.url_for_root(0).unwrap()))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    until(
        &tab,
        "document.querySelector('#stream .blk') !== null",
        "the initial prose to arrive",
        Duration::from_secs(15),
        "document.body.textContent.slice(-200)",
    );
    assert_eq!(eval(&tab, "performance.getEntriesByType('resource').filter(e=>e.name.endsWith('/mermaid.js')).length"), 0);
    eval(&tab, "(() => { const append = document.head.append.bind(document.head); document.head.append = node => { if (node.src?.endsWith('/mermaid.js')) { queueMicrotask(() => node.onerror()); return; } return append(node); }; window.__restoreMermaidLoad = () => { document.head.append = append; }; return true; })()");
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(
            assistant_markdown(
                "```mermaid\nflowchart TD\n A[Recovered] --> B[Diagram]\n```",
                &harness::at("15:02"),
            )
            .as_bytes(),
        )
        .unwrap();
    until(
        &tab,
        "document.querySelector('[data-mermaid-state=error]') !== null",
        "a failed load to show an error and the original source",
        Duration::from_secs(20),
        "document.querySelector('[data-mermaid]')?.outerHTML",
    );
    assert_eq!(
        eval(&tab, "document.querySelector('[data-mermaid] pre').hidden"),
        false
    );
    assert_eq!(eval(&tab, "document.querySelector('[data-mermaid] .mermaid-error').textContent.includes('Could not load')"), true);
    eval(
        &tab,
        "window.__restoreMermaidLoad(); document.documentElement.dataset.theme = 'dark'; true",
    );
    wait_diagrams(&tab, 1);
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('[data-mermaid] .mermaid-diagram').hidden"
        ),
        false
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('[data-mermaid] .mermaid-error').hidden"
        ),
        true
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn mermaid_is_visible_in_the_main_page_on_desktop_and_phone() {
    let _serial = serial();
    let base = base("mermaid-main-page");
    let stores = harness::Stores::new(&base);
    let diagram = "flowchart TD\n  INPUT[\"Invented sample input\"] --> FORMAT[\"Check sample format\"]\n  FORMAT --> NORMALIZE[\"Normalize synthetic item\"]\n  NORMALIZE --> VALIDATE[\"Validate example fields\"]\n  VALIDATE --> OUTPUT[\"Mock result\"]\n  VALIDATE --> ARCHIVE[\"Demo archive\"]";
    stores.claude_session(SID, &(user_at("Show the synthetic data-processing example.", &harness::at("15:00")) + &assistant_markdown(&format!("All labels in this diagram are invented. The arrows indicate transformations of sample data.\n\n```mermaid\n{diagram}\n```"), &harness::at("15:01"))));
    let monitor = harness::Monitor::spawn(harness::Kind::V1, 3043, &base, Some(&stores), true);
    let (_browser, tab) = harness::chrome_tab();
    harness::resize(&tab, 1600.0, 1050.0);
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?session={SID}&ui=app&mountall=1"));
    wait_diagrams(&tab, 1);
    eval(&tab, "document.documentElement.dataset.theme = ''; document.getElementById('sidebarCollapse').click(); document.querySelector('.transcript').scrollTop = 0; true");
    until(&tab, "(() => { const svg = document.querySelector('[data-mermaid-state=ready] svg'); const box = svg?.getBoundingClientRect(); return box && box.height > 250 && box.bottom < innerHeight; })()", "the entire diagram to be visible in the main transcript", Duration::from_secs(10), "document.querySelector('[data-mermaid] svg')?.getBoundingClientRect().toJSON()");
    if let Some(path) = std::env::var_os("CR_MERMAID_SCREENSHOT") {
        let png = tab
            .capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                None,
                true,
            )
            .unwrap();
        std::fs::write(path, png).unwrap();
    }
    harness::phone(&tab, 390, 844);
    wait_diagrams(&tab, 1);
    until(&tab, "(() => { const svg = document.querySelector('[data-mermaid-state=ready] svg'); const parent = svg?.closest('.mermaid-diagram'); return svg && Math.abs(svg.getBoundingClientRect().width - svg.viewBox.baseVal.width) < 1 && parent.getBoundingClientRect().right <= innerWidth + 1 && svg.getBoundingClientRect().height > 10; })()", "the phone diagram to retain readable native pixels inside a bounded scrolling container", Duration::from_secs(10), "document.querySelector('[data-mermaid] svg')?.getBoundingClientRect().toJSON()");
    assert_eq!(eval(&tab, "[...document.querySelectorAll('[data-mermaid] .mermaid-source, [data-mermaid] .cpy')].every(e => e.getBoundingClientRect().height >= 44 && e.getBoundingClientRect().width >= 44)"), true);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn dense_mermaid_groups_use_the_monitor_palette_and_scroll_without_shrinking_labels() {
    let _serial = serial();
    let base = base("mermaid-dense-palette");
    let stores = harness::Stores::new(&base);
    let diagram = "flowchart LR\n subgraph NS[Synthetic services]\n  subgraph PROCESSOR[Processing worker]\n   A[Demo inbox] --> B[Demo worker] --> C[Mock artifact]\n  end\n  subgraph SCHEDULER[Example scheduler]\n   D[Sample timer] --> E[Example dispatcher] --> F[Demo evaluator]\n  end\n  E --> A\n  E --> G[Result checker]\n end\n H[Demo browser] --> I[Example gateway] --> J[Sample store]\n I --> C";
    stores.claude_session(
        SID,
        &(user_at(
            "Show a grouped architecture diagram.",
            &harness::at("15:00"),
        ) + &assistant_markdown(
            &format!("```mermaid\n{diagram}\n```"),
            &harness::at("15:01"),
        )),
    );
    let monitor = harness::Monitor::spawn(harness::Kind::V2, 3044, &base, Some(&stores), true);
    for shell in ["app", "classic"] {
        let (_browser, tab) = harness::chrome_tab();
        monitor.pair(&tab);
        monitor.open(&tab, &format!("?session={SID}&ui={shell}&mountall=1"));
        wait_diagrams(&tab, 1);
        assert_eq!(eval(&tab, "getComputedStyle(document.querySelector('[data-mermaid] .cluster rect')).fill === 'rgb(241, 244, 249)'"), true);
        harness::phone(&tab, 390, 844);
        assert_eq!(eval(&tab, "(() => { const svg = document.querySelector('[data-mermaid] svg'); const viewport = svg.closest('.mermaid-diagram'); return Math.abs(svg.getBoundingClientRect().width - svg.viewBox.baseVal.width) < 1 && svg.getBoundingClientRect().width > viewport.clientWidth && viewport.getBoundingClientRect().right <= innerWidth + 1 && [...svg.querySelectorAll('.node text')].every(text => getComputedStyle(text).fontSize === '14px'); })()"), true);
        let light_id = eval(&tab, "document.querySelector('[data-mermaid] svg').id");
        eval(
            &tab,
            "document.documentElement.dataset.theme = 'dark'; true",
        );
        until(
            &tab,
            &format!("document.querySelector('[data-mermaid-state=ready] svg')?.id !== {light_id}"),
            "the grouped diagram to redraw in the dark palette",
            Duration::from_secs(30),
            "document.querySelector('[data-mermaid]')?.dataset.mermaidState",
        );
        assert_eq!(eval(&tab, "getComputedStyle(document.querySelector('[data-mermaid] .cluster rect')).fill === 'rgb(32, 39, 51)'"), true);
        assert_eq!(eval(&tab, "getComputedStyle(document.querySelector('[data-mermaid] .node rect')).fill === 'rgb(35, 42, 56)'"), true);
    }
}
