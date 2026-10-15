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

/// The target must exist, be readable and hit-testable, and stop moving across samples.
fn target_ready(selector: &str) -> String {
    format!(
        r#"(() => {{
        const selector={selector}, node=document.querySelector(selector);
        const samples=window.__quotationSamples ||= new Map();
        if (!node || !node.isConnected || !(node.textContent.trim() || (node.matches('button') && node.getAttribute('aria-label')))) {{samples.delete(selector);return false;}}
        const scroller=node.closest('.transcript');
        for (let parent=node;parent;parent=parent.parentElement) {{
            const style=getComputedStyle(parent);
            if (style.display==='none' || style.visibility==='hidden' || Number(style.opacity)===0) return false;
        }}
        const rect=node.getBoundingClientRect(), clip=scroller ? scroller.getBoundingClientRect() : {{left:0,right:innerWidth,top:0,bottom:innerHeight}};
        const inside=rect.width>0 && rect.height>0 && rect.left>=Math.max(0,clip.left) && rect.right<=Math.min(innerWidth,clip.right)
            && rect.top>=Math.max(0,clip.top) && rect.bottom<=Math.min(innerHeight,clip.bottom);
        const hit=inside && document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2);
        if (!hit || !node.contains(hit)) {{samples.delete(selector);return false;}}
        const geometry=[rect.x,rect.y,rect.width,rect.height,scroller?.scrollTop || 0];
        const previous=samples.get(selector);
        const stable=previous?.node===node && previous.geometry.every((value,i)=>Math.abs(value-geometry[i])<0.5);
        const count=stable ? previous.count+1 : 0;
        samples.set(selector,{{node,geometry,count}});
        return count>=2;
    }})()"#,
        selector = json!(selector)
    )
}

fn wait_target(tab: &headless_chrome::Tab, selector: &str) {
    harness::until(
        tab,
        &target_ready(selector),
        "a settled, visible quotation target",
        Duration::from_secs(10),
        "document.body.innerText",
    );
}

#[test]
#[ignore] // actual Chrome
fn quotation_visibility_requires_existing_readable_unobscured_content() {
    let _serial = serial();
    let (_browser, tab) = harness::chrome_tab();
    tab.navigate_to("data:text/html,<main class='transcript' style='height:200px;overflow:hidden'><blockquote class='reply-quote'>Readable selection</blockquote></main>").unwrap();
    tab.wait_until_navigated().unwrap();
    wait_target(&tab, ".reply-quote");
    for mutation in [
        "document.querySelector('.reply-quote').textContent=''",
        "document.querySelector('.reply-quote').textContent='Readable selection';document.querySelector('.reply-quote').style.visibility='hidden'",
        "document.querySelector('.reply-quote').style.visibility='visible';document.body.insertAdjacentHTML('beforeend', '<div id=cover style=\"position:fixed;inset:0;background:white;z-index:999\"></div>')",
        "document.getElementById('cover').remove();document.querySelector('.reply-quote').style.transform='translateY(5000px)'",
        "document.querySelector('.reply-quote').remove()",
    ] {
        eval(&tab,&format!("{mutation};true"));
        assert_eq!(eval(&tab,&target_ready(".reply-quote")),false,"{mutation}");
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
    let invalid_claude = "<?--\r\nreply 1 -->\r\n> <local-command-caveat>KEEP SELECTED TEXT</local-command-caveat>\r\n\r\nMy comment".to_string();
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
            + &(json!({"type":"user","cwd":"/r","timestamp":"2026-10-10T10:00:00Z","message":{"role":"user","content":invalid_claude}}).to_string() + "\n")
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
            assert_eq!(
                state["label"],
                format!("{}…", comment.chars().take(80).collect::<String>())
            );
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
        // A fresh phone context is configured before pairing or the first app load.
        // Resizing the desktop context cannot exercise state.js's phone defaults.
        for (orientation, width, height) in [("portrait", 390, 844), ("landscape", 844, 390)] {
            let (_phone_browser, phone_tab) = harness::chrome_tab();
            harness::phone(&phone_tab, width, height);
            monitor.pair(&phone_tab);
            phone_tab.navigate_to(&url).unwrap();
            phone_tab.wait_until_navigated().unwrap();
            harness::until(
                &phone_tab,
                "document.querySelectorAll('#navigatorTurns .outline-turn-row').length===3",
                "phone turns loaded",
                Duration::from_secs(15),
                "document.body.innerText",
            );
            assert_eq!(
                eval(
                    &phone_tab,
                    "document.querySelector('.workspace').classList.contains('navigator-off')"
                ),
                true,
                "fresh phone outline default"
            );
            wait_target(&phone_tab, "#phonePane-turns");
            phone_tab
                .find_element("#phonePane-turns")
                .unwrap()
                .click()
                .unwrap();
            wait_target(&phone_tab, "#phonePaneMenu .outline-turn-row");
            phone_tab
                .find_element("#phonePaneMenu .outline-turn-row")
                .unwrap()
                .click()
                .unwrap();
            wait_target(&phone_tab, ".turn.user[data-turn=\"1\"] .reply-quote");
            if let Ok(expand) = phone_tab.find_element(".turn.user[data-turn=\"1\"] .prompt-expand")
            {
                expand.click().unwrap();
            }
            for item in [1, 2] {
                let selector = format!(
                    ".turn.user[data-turn=\"1\"] .reply-item:nth-child({item}) .reply-quote"
                );
                harness::wheel_scroll(&phone_tab,harness::APP_SCROLLER,&format!("const q=document.querySelector({}); if(q) s.scrollTop+=q.getBoundingClientRect().top-s.getBoundingClientRect().top-60;",json!(selector)));
                wait_target(&phone_tab, &selector);
                assert_eq!(eval(&phone_tab,"document.querySelectorAll('.turn.user[data-turn=\"1\"] .reply-quote').length"),2);
                assert!(eval(
                    &phone_tab,
                    &format!("document.querySelector({}).textContent", json!(selector))
                )
                .as_str()
                .unwrap()
                .contains(if item == 1 {
                    "SELECTED FIRST"
                } else {
                    "SELECTED SECOND"
                }));
                for theme in ["light", "dark"] {
                    eval(
                        &phone_tab,
                        &format!(
                            "document.documentElement.dataset.theme={};true",
                            json!(theme)
                        ),
                    );
                    wait_target(&phone_tab, &selector);
                    if std::env::var_os("CR_QUOTED_SCREENSHOTS").is_some() {
                        let png=phone_tab.capture_screenshot(headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,None,None,true).unwrap();
                        let suffix = if item == 1 { "" } else { "-second" };
                        std::fs::write(
                            base.join(format!("{agent}-{orientation}-{theme}{suffix}.png")),
                            png,
                        )
                        .unwrap();
                    }
                }
            }
            // Navigate back to the reply before measuring its raw-source control.
            phone_tab
                .find_element("#phonePane-turns")
                .unwrap()
                .click()
                .unwrap();
            wait_target(&phone_tab, "#phonePaneMenu .outline-turn-row");
            phone_tab
                .find_element("#phonePaneMenu .outline-turn-row")
                .unwrap()
                .click()
                .unwrap();
            let raw = ".turn.user[data-turn=\"1\"] .raw-toggle";
            wait_target(&phone_tab, raw);
            let xy=harness::probe(&phone_tab,&format!("(() => {{const r=document.querySelector({}).getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2];}})()",json!(raw)));
            harness::finger_tap(&phone_tab, xy[0].as_f64().unwrap(), xy[1].as_f64().unwrap());
            harness::until(
                &phone_tab,
                "document.querySelector('.turn.user[data-turn=\"1\"] .turn-raw-text')!==null",
                "phone raw source",
                Duration::from_secs(5),
                "document.body.innerText",
            );
            assert_eq!(eval(&phone_tab,"document.querySelector('.turn.user[data-turn=\"1\"] .turn-raw-text').textContent"),original);
        }
    }
}
