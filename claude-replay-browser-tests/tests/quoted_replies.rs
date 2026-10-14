//! Quotation replies through actual native normalization, wire emission, and App-shell DOM.
mod harness;
use harness::{base, eval, serial, Kind, Monitor, Stores};
use serde_json::json;
use std::time::Duration;

const CODEX: &str =
    include_str!("../../claude-replay-agents/tests/fixtures/codex-quoted-reply.txt");
const CLAUDE: &str = "<!-- reply 2 -->\n> SELECTED FIRST\n>   code | table\n\nPlease explain the first selection.\n\n<!-- reply -->\n> SELECTED SECOND\n\nFINAL FOLLOWING TEXT\n\n\nREQUEST-LIKE PARAGRAPH\n```rust\nlet value = 1;";

fn user(agent: &str, text: &str, image: bool) -> String {
    let mut content = vec![if agent == "codex" {
        json!({"type":"input_text","text":text})
    } else {
        json!({"type":"text","text":text})
    }];
    if image {
        content.push(if agent == "codex" {
            json!({"type":"input_image","image_url":format!("data:image/png;base64,{}",harness::TINY_PNG_B64)})
        } else { json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":harness::TINY_PNG_B64}}) });
    }
    if agent == "codex" {
        json!({"type":"response_item","timestamp":"2026-10-10T10:00:00Z","payload":{"type":"message","role":"user","content":content}}).to_string() + "\n"
    } else {
        json!({"type":"user","cwd":"/r","timestamp":"2026-10-10T10:00:00Z","message":{"role":"user","content":content}}).to_string() + "\n"
    }
}

#[test]
#[ignore] // actual Chrome and freshly built monitor binaries
fn quoted_replies_show_pairs_raw_source_and_navigable_previews_in_both_themes() {
    let _serial = serial();
    let base = base("quoted-replies");
    let stores = Stores::new(&base);
    let codex_id = "aaaaaaaa-0010-4000-8000-000000000001";
    let claude_id = "cccccccc-0010-4000-8000-000000000001";
    let comment = format!(
        "Please explain the first selection. {}",
        "detail ".repeat(30)
    );
    let claude = CLAUDE.replace("Please explain the first selection.", &comment);
    let codex = CODEX
        .replace(
            "\"source\":{",
            "\"annotation\":\"Please explain the first selection.\",\"source\":{",
        )
        .replace(
            "This is the first excerpt from Codex's response.",
            "SELECTED FIRST\\r\\n  code | table",
        );
    let codex = codex.replace("Please explain the first selection.", &comment);
    let note = base.join("note.md");
    std::fs::write(&note, "Synthetic file attachment for quotation replies.\n").unwrap();
    let codex = codex.replace(
        "\n## My request:\n",
        &format!(
            "\n# Files mentioned by the user:\n## note.md: {}\n## My request:\n",
            note.display()
        ),
    );
    let invalid_codex = codex
        .replace("Each item contains", "Each item includes")
        .replace('\n', "\r\n");
    let invalid_claude = "<!-\r\nreply 1 -->\r\n> <local-command-caveat>KEEP SELECTED TEXT</local-command-caveat>\r\n\r\nMy comment".to_string();
    let codex_only = CODEX
        .split("\n\n## My request:")
        .next()
        .unwrap()
        .replace(",\"annotation\":\"Please clarify this point.\"", "");
    let claude_only = "<!-- reply 1 -->\n> QUOTE ONLY\n\n";
    let head = json!({"type":"session_meta","payload":{"id":codex_id,"cwd":"/r","timestamp":"2026-10-10T10:00:00Z"}}).to_string() + "\n";
    stores.codex_session(
        codex_id,
        &(head
            + &user("codex", &codex, true)
            + &user("codex", &invalid_codex, false)
            + &user("codex", &codex_only, false)),
    );
    stores.claude_session(
        claude_id,
        &(user("claude", &claude, true)
            + &user("claude", &invalid_claude, false)
            + &user("claude", claude_only, false)),
    );
    let monitor = Monitor::spawn(Kind::V1, 3195, &base, Some(&stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    for (agent, id, original) in [
        ("codex", codex_id, codex.as_str()),
        ("claude", claude_id, claude.as_str()),
    ] {
        let monitor_id = if agent == "codex" {
            format!("rollout-{id}")
        } else {
            id.to_string()
        };
        let url = format!("http://127.0.0.1:3195/?ui=app&session={monitor_id}");
        tab.navigate_to(&url).unwrap();
        tab.wait_until_navigated().unwrap();
        harness::until(
            &tab,
            "document.querySelectorAll('#navigatorTurns .outline-turn-row').length === 3",
            "three user turns",
            Duration::from_secs(20),
            "document.body.innerText",
        );
        for theme in ["light", "dark"] {
            eval(
                &tab,
                &format!(
                    "document.documentElement.dataset.theme={}; true",
                    json!(theme)
                ),
            );
            tab.find_element("#navigatorTurns .outline-turn-row")
                .unwrap()
                .click()
                .unwrap();
            harness::until(
                &tab,
                "document.querySelector('.turn.user[data-turn=\"1\"] .reply-quote') !== null",
                "first quoted turn",
                Duration::from_secs(10),
                "document.body.innerText",
            );
            let state = harness::probe(
                &tab,
                r#"(() => {
                const root = document.querySelector('.turn.user[data-turn="1"]');
                const first = document.querySelector('#navigatorTurns .outline-turn-row');
                const quote = root.querySelector('.reply-quote');
                const count = first.querySelector('.outline-reply-count');
                const box = count.getBoundingClientRect(), row = first.getBoundingClientRect();
                return {quote: quote.textContent, pairs: root.querySelectorAll('.reply-item').length,
                    label: first.querySelector('.outline-label').textContent,
                    marker: first.querySelector('.outline-quote-marker').getAttribute('aria-label'),
                    count: count.textContent, countInside: box.right <= row.right && box.width > 0,
                    countOnLine: Math.abs((box.y+box.height/2) - (() => {const r=first.querySelector(".outline-label").getBoundingClientRect(); return r.y+r.height/2;})()) < 2,
                    whitespace: getComputedStyle(quote).whiteSpace,
                    border: getComputedStyle(quote).borderLeftWidth,
                    text: root.innerText,
                    images: root.querySelectorAll('[data-attachment]').length};
            })()"#,
            );
            assert_eq!(state["pairs"], 2);
            assert_eq!(state["label"], comment.chars().take(80).collect::<String>());
            assert_eq!(state["marker"], "Quoted reply");
            assert_eq!(state["count"], "+1");
            assert_eq!(state["countInside"], true);
            assert_eq!(state["countOnLine"], true);
            assert_eq!(
                state["quote"],
                if agent == "codex" {
                    "SELECTED FIRST\r\n  code | table"
                } else {
                    "SELECTED FIRST\n  code | table"
                }
            );
            assert_eq!(state["whitespace"], "pre-wrap");
            assert_eq!(state["border"], "2px");
            assert!(state["images"].as_u64().unwrap() > 0, "{state}");
            if agent == "codex" {
                assert_eq!(
                    eval(&tab, "(() => { const a=document.querySelector('.turn.user[data-turn=\"1\"] .prompt-file[data-name=\"note.md\"]'); return !!a && a.dataset.attachmentAction === 'preview' && !!a.dataset.fsig && !!a.dataset.sig; })()"),
                    true
                );
            }
            let rendered = state["text"].as_str().unwrap();
            assert_eq!(rendered.matches("SELECTED FIRST").count(), 1);
            assert_eq!(
                rendered
                    .matches("Please explain the first selection.")
                    .count(),
                1
            );
            assert!(!rendered.contains("Response annotations:"));
            if agent == "claude" {
                assert!(rendered.contains("FINAL FOLLOWING TEXT\n\n\nREQUEST-LIKE PARAGRAPH"));
                assert_eq!(
                    eval(&tab, "document.querySelectorAll('.turn.user[data-turn=\"1\"] .reply-response')[1].textContent"),
                    "FINAL FOLLOWING TEXT\n\n\nREQUEST-LIKE PARAGRAPH\n```rust\nlet value = 1;"
                );
            }
            if std::env::var_os("CR_QUOTED_SCREENSHOTS").is_some() {
                let png = tab
                    .capture_screenshot(
                        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                        None,
                        None,
                        true,
                    )
                    .unwrap();
                std::fs::write(base.join(format!("{agent}-{theme}.png")), png).unwrap();
            }
            // The native button's keyboard activation opens THIS reply's raw source.
            eval(
                &tab,
                "document.querySelector('.turn.user[data-turn=\"1\"] .raw-toggle').focus(); true",
            );
            tab.press_key("Enter").unwrap();
            harness::until(
                &tab,
                "document.querySelector('.turn.user[data-turn=\"1\"] .turn-raw-text') !== null",
                "raw original text",
                Duration::from_secs(5),
                "document.body.innerText",
            );
            assert_eq!(eval(&tab, "document.querySelector('.turn.user[data-turn=\"1\"] .turn-raw-text').textContent"), original);
            tab.find_element(".turn.user[data-turn=\"1\"] .raw-toggle")
                .unwrap()
                .click()
                .unwrap();
            // Search uses the visible quote and comment, without the hidden envelope.
            eval(&tab, "document.getElementById('transcriptSearchInput').value='SELECTED FIRST'; document.getElementById('transcriptSearchInput').dispatchEvent(new Event('input',{bubbles:true})); true");
            harness::until(
                &tab,
                "document.querySelector('.turn.user[data-turn=\"1\"] .search-mark') !== null",
                "quoted-text search mark",
                Duration::from_secs(5),
                "document.body.innerText",
            );
            eval(&tab, "document.getElementById('transcriptSearchInput').value=''; document.getElementById('transcriptSearchInput').dispatchEvent(new Event('input',{bubbles:true})); true");
        }
        // A format mutation has a complete readable fallback, without a quotation marker.
        let ordinary = eval(&tab, "document.querySelectorAll('#navigatorTurns .outline-turn-row')[1].querySelector('.outline-quote-marker') === null");
        assert_eq!(ordinary, true);
        tab.find_elements("#navigatorTurns .outline-turn-row")
            .unwrap()[1]
            .click()
            .unwrap();
        harness::until(
            &tab,
            "document.querySelector('.turn.user[data-turn=\"2\"]') !== null",
            "fallback reply",
            Duration::from_secs(5),
            "document.body.innerText",
        );
        assert_eq!(
            eval(
                &tab,
                "document.querySelector('.turn.user[data-turn=\"2\"] .quoted-reply') === null"
            ),
            true
        );
        tab.find_element(".turn.user[data-turn=\"2\"] .raw-toggle")
            .unwrap()
            .click()
            .unwrap();
        harness::until(
            &tab,
            "document.querySelector('.turn.user[data-turn=\"2\"] .turn-raw-text') !== null",
            "fallback raw source",
            Duration::from_secs(5),
            "document.body.innerText",
        );
        assert_eq!(
            eval(
                &tab,
                "document.querySelector('.turn.user[data-turn=\"2\"] .turn-raw-text').textContent"
            ),
            if agent == "codex" {
                invalid_codex.as_str()
            } else {
                invalid_claude.as_str()
            }
        );
        let only = eval(&tab, "document.querySelectorAll('#navigatorTurns .outline-turn-row')[2].querySelector('.outline-label').textContent");
        assert_eq!(
            only,
            if agent == "codex" {
                "This is the first excerpt from Codex's response."
            } else {
                "QUOTE ONLY"
            }
        );
        // Touch activates the same raw control without hover at the App shell's desktop width.
        tab.find_elements("#navigatorTurns .outline-turn-row")
            .unwrap()[0]
            .click()
            .unwrap();
        let xy = harness::probe(&tab, "(() => { const b=document.querySelector('.turn.user[data-turn=\"1\"] .raw-toggle').getBoundingClientRect(); return [b.x+b.width/2,b.y+b.height/2]; })()");
        harness::finger_tap(&tab, xy[0].as_f64().unwrap(), xy[1].as_f64().unwrap());
        harness::until(
            &tab,
            "document.querySelector('.turn.user[data-turn=\"1\"] .turn-raw-text') !== null",
            "touch raw toggle",
            Duration::from_secs(5),
            "document.body.innerText",
        );
        tab.find_element(".turn.user[data-turn=\"1\"] .raw-toggle")
            .unwrap()
            .click()
            .unwrap();
        for (orientation, width, height) in [("portrait", 390, 844), ("landscape", 844, 390)] {
            harness::phone(&tab, width, height);
            harness::until(
                &tab,
                &format!("innerWidth === {width} && document.querySelectorAll('.turn.user[data-turn=\"1\"] .reply-item').length === 2"),
                "phone quotation layout",
                Duration::from_secs(5),
                "document.body.innerText",
            );
            assert_eq!(eval(&tab, "[...document.querySelectorAll('.turn.user[data-turn=\"1\"] .reply-quote')].every(q => { const r=q.getBoundingClientRect(); return r.width > 0 && r.left >= 0 && r.right <= innerWidth; })"), true);
            for theme in ["light", "dark"] {
                eval(
                    &tab,
                    &format!(
                        "document.documentElement.dataset.theme={}; true",
                        json!(theme)
                    ),
                );
                if std::env::var_os("CR_QUOTED_SCREENSHOTS").is_some() {
                    let png = tab.capture_screenshot(
                        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                        None, None, true,
                    ).unwrap();
                    std::fs::write(base.join(format!("{agent}-{orientation}-{theme}.png")), png)
                        .unwrap();
                }
            }
        }
        use headless_chrome::protocol::cdp::Emulation;
        tab.call_method(Emulation::ClearDeviceMetricsOverride(None))
            .unwrap();
        tab.call_method(Emulation::SetTouchEmulationEnabled {
            enabled: false,
            max_touch_points: None,
        })
        .unwrap();
    }
}
