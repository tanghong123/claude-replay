//! Native controls use the established Monitor/Stores/Chrome harness and synthetic protocol data.
mod harness;
#[path = "harness/native.rs"]
mod native;
use harness::{
    base, chrome_tab, eval, finger_tap, phone, probe, serial, until, Kind, Monitor, Stores,
};
use std::{sync::atomic::Ordering, time::Duration};

fn wait(tab: &headless_chrome::Tab, condition: &str) {
    until(
        tab,
        condition,
        condition,
        Duration::from_secs(15),
        "document.body.innerText.slice(-2400)",
    );
}
fn click(tab: &headless_chrome::Tab, selector: &str) {
    wait(tab, &format!("!!document.querySelector({selector:?})"));
    eval(
        tab,
        &format!("document.querySelector({selector:?}).click(); true"),
    );
}
fn tap(tab: &headless_chrome::Tab, selector: &str) {
    wait(tab, &format!("(() => {{const e=document.querySelector({selector:?});if(!e || e.disabled)return false;const r=e.getBoundingClientRect();return r.width>0 && r.height>0 && r.left>=0 && r.top>=0 && r.right<=innerWidth && r.bottom<=innerHeight && e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2));}})()"));
    let point = probe(tab, &format!("(() => {{const r=document.querySelector({selector:?}).getBoundingClientRect();return {{x:r.x+r.width/2,y:r.y+r.height/2}};}})()"));
    if selector == "#composeInput" {
        // The synthetic gesture API rejects this lower-field point on macOS
        // Chrome despite a successful viewport hit test. Dispatch the established
        // touch/down/up and delayed compatibility-click sequence instead.
        harness::finger_tap_late_click(
            tab,
            point["x"].as_f64().unwrap(),
            point["y"].as_f64().unwrap(),
            Duration::from_millis(20),
        );
    } else {
        finger_tap(
            tab,
            point["x"].as_f64().unwrap(),
            point["y"].as_f64().unwrap(),
        );
    }
}

/// Exercise the real controller/dialog against synthetic ownership and stop failures.
/// The protocol fixture stays external: these tests never launch or stop an actual Codex process.
fn shutdown_confirmation(tab: &headless_chrome::Tab, root: &std::path::Path) {
    eval(
        tab,
        r#"(() => {
      window.shutdownFetch = window.fetch; window.shutdownStops = 0; window.shutdownFailure = true; window.shutdownErrors = [];
      window.addEventListener('error', e=>window.shutdownErrors.push(e.message));
      window.addEventListener('unhandledrejection', e=>window.shutdownErrors.push(String(e.reason)));
      window.fetch = async (url, options) => {
        if (String(url) === '/api/codex/stop') {
          window.shutdownStops++;
          if (window.shutdownFailure) return new Response(JSON.stringify({ok:false,error:{code:'native_disconnect_failed',message:'Synthetic stop failure'}}),{status:503});
        }
        const response = await window.shutdownFetch(url, options);
        if (String(url) !== '/api/codex/status') return response;
        const status = await response.json();
        return new Response(JSON.stringify({...status,homeProvenance:'monitor-started',ownedServerPid:123,activeWork:true,activeTurns:[{threadId:'review-thread',threadName:'Synthetic review',turnId:'review-turn-7'}]}),{status:response.status});
      }; return true;
    })()"#,
    );
    for trigger in ["#nativeEnable", "#writeSwitch"] {
        if trigger == "#nativeEnable" {
            click(tab, "#codexNativeButton");
            wait(
                tab,
                "document.getElementById('codexNativeMenu').matches(':popover-open')",
            );
        }
        click(tab, trigger);
        wait(
            tab,
            "document.getElementById('nativeStopDialog')?.open === true",
        );
        assert_eq!(eval(tab, "document.activeElement.id"), "nativeStopCancel");
        assert_eq!(eval(tab, "document.getElementById('nativeStopDescription').innerText.includes('review-turn-7') && document.getElementById('nativeStopDescription').innerText.includes('Synthetic review')"), true);
        assert_eq!(eval(tab, "window.shutdownStops"), 0);
        assert_eq!(
            eval(
                tab,
                "document.getElementById('writeSwitch').getAttribute('aria-checked')"
            ),
            "true"
        );
        assert_eq!(
            eval(
                tab,
                "document.getElementById('codexNativeButton').dataset.nativeEnabled"
            ),
            "true"
        );
        assert_eq!(eval(tab, "(() => {const d=document.getElementById('nativeStopDialog'),r=d.getBoundingClientRect();return d.checkVisibility() && r.left>=0 && r.right<=innerWidth && r.top>=0 && r.bottom<=innerHeight && ['nativeStopCancel','nativeStopConfirm'].every(id=>{const e=document.getElementById(id),b=e.getBoundingClientRect();return b.height>=44 && e.contains(document.elementFromPoint(b.x+b.width/2,b.y+b.height/2));});})()"), true);
        if trigger == "#nativeEnable" {
            std::fs::write(
                root.join("native-stop-confirmation.png"),
                tab.capture_screenshot(
                    headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                    None,
                    None,
                    true,
                )
                .unwrap(),
            )
            .unwrap();
            click(tab, "#nativeStopCancel");
        } else {
            tab.press_key("Escape").unwrap();
        }
        wait(tab, "!document.getElementById('nativeStopDialog') && !document.getElementById('codexNativeButton').disabled");
        assert_eq!(eval(tab, "window.shutdownStops"), 0);
        assert_eq!(
            eval(
                tab,
                "document.getElementById('writeSwitch').getAttribute('aria-checked')"
            ),
            "true"
        );
    }
    // The whole Write switch stays on when the confirmed request fails.
    click(tab, "#writeSwitch");
    wait(
        tab,
        "document.getElementById('nativeStopDialog')?.open === true",
    );
    click(tab, "#nativeStopConfirm");
    until(tab, "window.shutdownStops === 1 && !document.getElementById('codexNativeButton').disabled", "failed shutdown settles", Duration::from_secs(15),
        "JSON.stringify({errors:window.shutdownErrors,stops:window.shutdownStops,disabled:document.getElementById('codexNativeButton').disabled,enabled:document.getElementById('codexNativeButton').dataset.nativeEnabled,toast:document.getElementById('toast').innerText,dialog:document.getElementById('nativeStopDialog')?.open})");
    assert_eq!(
        eval(
            tab,
            "document.getElementById('writeSwitch').getAttribute('aria-checked')"
        ),
        "true"
    );
    assert_eq!(
        eval(
            tab,
            "document.getElementById('codexNativeButton').dataset.nativeEnabled"
        ),
        "true"
    );
    eval(tab, "window.shutdownFailure = false; true");
    for trigger in ["#nativeEnable", "#writeSwitch"] {
        if trigger == "#nativeEnable" {
            click(tab, "#codexNativeButton");
            wait(
                tab,
                "document.getElementById('codexNativeMenu').matches(':popover-open')",
            );
        }
        click(tab, trigger);
        wait(
            tab,
            "document.getElementById('nativeStopDialog')?.open === true",
        );
        click(tab, "#nativeStopConfirm");
        wait(
            tab,
            "document.getElementById('codexNativeButton').dataset.nativeEnabled === 'false'",
        );
        assert_eq!(
            eval(
                tab,
                "document.getElementById('writeSwitch').getAttribute('aria-checked')"
            ),
            if trigger == "#nativeEnable" {
                "true"
            } else {
                "false"
            }
        );
        if trigger == "#nativeEnable" {
            click(tab, "#codexNativeButton");
            wait(
                tab,
                "document.getElementById('nativeEnable')?.getAttribute('aria-checked') === 'false'",
            );
            click(tab, "#nativeEnable");
            wait(
                tab,
                "document.getElementById('codexNativeButton').dataset.nativeEnabled === 'true'",
            );
        }
    }
    assert_eq!(eval(tab, "window.shutdownStops"), 3);
    eval(tab, "window.fetch = window.shutdownFetch; true");
}
fn run(mobile: bool, unix: bool) {
    run_sized(mobile, unix, 390, 844);
}

fn run_sized(mobile: bool, unix: bool, width: u32, height: u32) {
    let _serial = serial();
    let root = base(match (mobile, unix, width > height) {
        (false, false, _) => "native-desktop",
        (true, false, false) => "native-phone",
        (true, false, true) => "native-phone-landscape",
        (false, true, _) => "native-unix-desktop",
        (true, true, false) => "native-unix-phone",
        (true, true, true) => "native-unix-phone-landscape",
    });
    let stores = Stores::new(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    stores.claude_session(
        "ordinary-claude",
        &(harness::user("Ordinary reader fixture", 0)
            + &harness::assistant("Claude remains ordinary", 1)),
    );
    let history = [
        serde_json::json!({"type":"session_meta","payload":{"id":"native-owned","cwd":project,"originator":"codex_cli_rs"}}),
        serde_json::json!({"timestamp":"2026-10-19T04:00:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Review the workspace changes."}]}}),
        serde_json::json!({"timestamp":"2026-10-19T04:00:01Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"The existing session history remains readable while another Codex instance owns the conversation."}]}}),
    ].iter().map(|value| value.to_string() + "\n").collect::<String>();
    let rollout = stores.codex_session("native-owned", &history);
    let native = native::NativeServer::start_transport(
        &project,
        &root.join(if mobile { "state-19842" } else { "state-19841" }),
        &rollout,
        unix,
    );
    // Unix sharing is explicitly selected; default endpoint selection has a separate configuration probe.
    let expected_endpoint = if unix {
        let socket = stores
            .root
            .join("codex/app-server-control/app-server-control.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(native.endpoint.strip_prefix("unix://").unwrap(), &socket)
            .unwrap();
        format!("unix://{}", socket.display())
    } else {
        native.endpoint.clone()
    };
    let args = vec![
        "--codex-server-url",
        expected_endpoint.as_str(),
        "--codex-allowed-root",
        project.to_str().unwrap(),
    ];
    let monitor = Monitor::spawn_with_args(
        Kind::V1,
        if mobile { 19842 } else { 19841 },
        &root,
        Some(&stores),
        true,
        &[
            ("HOME", root.to_str().unwrap()),
            (
                "CODEX_NATIVE_BINARY",
                "/nonexistent/native-fixture-must-not-launch-codex",
            ),
        ],
        &args,
    );
    let (browser, tab) = chrome_tab();
    if mobile {
        phone(&tab, width, height);
        harness::safe_area(
            &tab,
            if width > height {
                harness::SIDEWAYS_INSETS
            } else {
                harness::PORTRAIT_INSETS
            },
        );
    }
    monitor.pair(&tab);
    monitor.open(&tab, "/?ui=app");
    // This fixture is historical and idle; choose Everything instead of racing
    // the default recent/blocked filter while the first scan settles.
    wait(&tab, "!!document.getElementById('codexNativeButton')");
    click(&tab, "#filterBtn");
    click(&tab, "[data-filter-reset]");
    click(&tab, "#filterBtn");
    wait(
        &tab,
        "!!document.querySelector('[data-session=ordinary-claude]')",
    );
    assert_eq!(
        eval(&tab, "document.body.hasAttribute('data-codex-native')"),
        false
    );
    assert_eq!(eval(&tab, "!!document.querySelector('link[href$=\"/monitor.css\"]') || !!document.getElementById('sharedModelPicker')"), false);
    assert_eq!(eval(&tab, "performance.getEntriesByType('resource').some(e => /native-mode.js|monitor-controls.js|api\\/codex/.test(e.name))"), false);
    assert!(native.calls.lock().unwrap().is_empty());
    assert!(!monitor.state.join("codex-gateway.sqlite3").exists());
    click(&tab, "#writeSwitch");
    wait(
        &tab,
        "!document.getElementById('codexNativeButton').hidden && document.getElementById('writeBtn').contains(document.getElementById('codexNativeButton'))",
    );
    assert!(
        native.calls.lock().unwrap().is_empty(),
        "ordinary Write does not open native RPC"
    );
    click(&tab, "#codexNativeButton");
    wait(
        &tab,
        "document.getElementById('nativeEnable')?.getAttribute('aria-checked') === 'false'",
    );
    assert_eq!(
        eval(&tab, "!!document.getElementById('codexNativeStyles')"),
        false
    );
    assert!(native.calls.lock().unwrap().is_empty());
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('nativeServerStatus').dataset.state"
        ),
        "running"
    );
    assert_eq!(eval(&tab, "document.getElementById('nativeServerStatus').innerText.includes('Using an existing server.') && document.getElementById('nativeServerStatus').innerText.includes('Native mode is off.')"), true);
    eval(
        &tab,
        r#"(() => {
      window.nativeIndexResponses = []; window.nativeIndexErrors = [];
      const originalWarn = console.warn; console.warn = (...args) => { window.nativeIndexErrors.push(args.map(a=>a?.stack || String(a)).join(' ')); originalWarn(...args); };
      const originalFetch = window.fetch; window.nativeTestFetch = async (...args) => {
        const response = await originalFetch(...args);
        if (String(args[0]) === '/api/sessions') window.nativeIndexResponses.push({status:response.status,body:await response.clone().text()});
        return response;
      }; window.heldNativeScans = []; window.holdNativeScans = false; window.heldNativeSends = []; window.holdNativeSends = false;
      window.initialNativeMenu = document.getElementById('codexNativeMenu'); window.nativeMenuCloses = 0;
      window.initialNativeMenu.addEventListener('beforetoggle', e => { if (e.newState === 'closed') window.nativeMenuCloses++; });
      window.fetch = (url, options) => window.holdNativeScans && String(url).startsWith('/api/codex/threads')
        ? new Promise((resolve, reject) => window.heldNativeScans.push({resolve,reject,url,options}))
        : window.holdNativeSends && String(url).endsWith("/api/codex/send")
        ? new Promise((resolve,reject) => window.heldNativeSends.push({resolve,reject,url,options}))
        : window.nativeTestFetch(url, options);
      return true;
    })()"#,
    );
    click(&tab, "#nativeEnable");
    wait(
        &tab,
        "document.getElementById('codexNativeButton')?.dataset.nativeEnabled === 'true'",
    );
    wait(&tab, "document.getElementById('codexNativeMenu').matches(':popover-open') && document.getElementById('nativeEnable').getAttribute('aria-checked') === 'true' && !document.getElementById('nativeEnable').disabled && document.getElementById('nativeServerStatus').dataset.state === 'running'");
    assert_eq!(eval(&tab, "document.getElementById('codexNativeMenu') === window.initialNativeMenu && window.nativeMenuCloses === 0"), true, "First activation retains the same open menu without a close/reopen flash");
    std::fs::write(
        root.join("native-enabled-menu.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    wait(
        &tab,
        "!!document.querySelector('[data-session=rollout-native-owned]')",
    );
    tab.press_key("Escape").unwrap();
    wait(
        &tab,
        "!document.getElementById('codexNativeMenu').matches(':popover-open')",
    );
    click(&tab, "[data-session=rollout-native-owned]");
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('codexNativeMenu').matches(':popover-open')"
        ),
        false,
        "The user can dismiss the enabled menu with Escape"
    );
    wait(&tab, "document.body.dataset.sharedCodex === 'true' && document.getElementById('sharedModelPicker')?.disabled === false");
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('meta[name=viewport]').content.includes('maximum-scale')"
        ),
        false
    );
    if mobile {
        phone_native_controls(&tab, &root);
    }
    click(&tab, "#sharedModelPicker");
    wait(
        &tab,
        "document.getElementById('sharedSettingsDialog')?.matches(':popover-open')",
    );
    eval(
        &tab,
        "document.querySelectorAll(':popover-open').forEach(p=>p.hidePopover()); true",
    );
    // Switching to an occupied session offers a bounded read retry, never an automatic action.
    eval(
        &tab,
        r#"(() => {
        const input = document.getElementById('composeInput'); input.value = 'Keep this unsent draft'; input.dispatchEvent(new Event('input'));
        const bytes = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jBz8AAAAASUVORK5CYII='), c=>c.charCodeAt(0));
        const files = new DataTransfer(); files.items.add(new File([bytes], 'draft.png', {type:'image/png'}));
        const picker = document.getElementById('sharedImageInput'); picker.files = files.files; picker.dispatchEvent(new Event('change')); return true;
    })()"#,
    );
    wait(
        &tab,
        "document.querySelectorAll('#sharedImages .shared-image img').length === 1",
    );
    native.occupied.store(true, Ordering::SeqCst);
    click(&tab, "[data-session=ordinary-claude]");
    wait(&tab, "!document.body.hasAttribute('data-shared-codex')");
    click(&tab, "[data-session=rollout-native-owned]");
    wait(
        &tab,
        "document.getElementById('sharedSessionConflict')?.hidden === false",
    );
    wait(&tab, "document.getElementById('transcript').innerText.includes('The existing session history remains readable')");
    assert_eq!(eval(&tab, "document.getElementById('sharedError').hidden && document.getElementById('sharedControlStatus').hidden"), true);
    assert_eq!(
        eval(
            &tab,
            "getComputedStyle(document.querySelector('#composer .compose-foot')).display"
        ),
        "none"
    );
    assert_eq!(
        eval(&tab, "document.getElementById('composeInput').value"),
        "Keep this unsent draft"
    );
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('sharedModelPicker').disabled"
        ),
        true
    );
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('composer').innerText.includes('thread/resume')"
        ),
        false
    );
    let geometry = probe(&tab, "(() => {const r=document.getElementById('composer').getBoundingClientRect();return {left:r.left,right:r.right,bottom:r.bottom,width:innerWidth,height:innerHeight};})()");
    assert!(
        geometry["left"].as_f64().unwrap() >= 0.0
            && geometry["right"].as_f64().unwrap() <= geometry["width"].as_f64().unwrap()
            && geometry["bottom"].as_f64().unwrap() <= geometry["height"].as_f64().unwrap(),
        "{geometry}"
    );
    std::fs::write(
        root.join("native-conflict.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    let before = native.calls.lock().unwrap().len();
    click(&tab, "#sharedConflictRetry");
    wait(&tab, "document.getElementById('sharedConflictRetry')?.disabled === false && !document.getElementById('sharedSessionConflict').hidden");
    let retry_calls = native.calls.lock().unwrap()[before..].to_vec();
    assert!(
        retry_calls.iter().any(|method| method == "thread/resume"),
        "retry must check ownership: {retry_calls:?}"
    );
    assert!(
        retry_calls.iter().all(|method| matches!(
            method.as_str(),
            "thread/read"
                | "thread/resume"
                | "thread/list"
                | "thread/queue/list"
                | "model/list"
                | "project/list"
        )),
        "retry and background polling must not mutate work: {retry_calls:?}"
    );
    click(&tab, "#sharedConflictHelp");
    wait(
        &tab,
        "document.getElementById('sharedConnectionHelp')?.open === true",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.shared-connection-steps').children.length"
        ),
        3
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('.shared-connection-technical').open"
        ),
        false
    );
    std::fs::write(
        root.join("native-conflict-help.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    if unix {
        click(&tab, ".shared-connection-sharing summary");
        let prompt = eval(
            &tab,
            "document.querySelector('.shared-connection-prompt').value",
        )
        .as_str()
        .unwrap()
        .to_owned();
        assert!(prompt.contains(&expected_endpoint));
        assert!(prompt.contains(stores.root.join("codex").to_str().unwrap()));
        assert!(!prompt.contains("Bearer") && !prompt.contains("tokenFile"));
        assert!(prompt.contains("one-line terminal command"));
        assert!(prompt.contains("Do not execute it or quit/stop any App or server from this chat"));
        let token_file = root
            .join(if mobile { "state-19842" } else { "state-19841" })
            .join("codex-server.token");
        assert!(!token_file.exists());
        eval(&tab, "Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>{window.copiedSetupPrompt=text}}});true");
        click(&tab, "#sharedCopySetup");
        wait(
            &tab,
            "document.querySelector('.shared-copy-status').textContent === 'Copied'",
        );
        assert_eq!(eval(&tab, "window.copiedSetupPrompt"), prompt);
        eval(
            &tab,
            "document.querySelector('.shared-connection-sharing').scrollIntoView({block:'start'});true",
        );
        std::fs::write(
            root.join("native-setup.png"),
            tab.capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                None,
                true,
            )
            .unwrap(),
        )
        .unwrap();
        eval(&tab, "Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async()=>{throw new Error('HTTP clipboard unavailable')}}});true");
        click(&tab, "#sharedCopySetup");
        wait(&tab, "document.querySelector('.shared-copy-status').textContent === 'Select and copy the prompt above.'");
        assert_eq!(eval(&tab, "(() => {const p=document.querySelector('.shared-connection-prompt');return p.selectionEnd-p.selectionStart===p.value.length;})()"), true);
    } else {
        assert_eq!(
            eval(
                &tab,
                "document.querySelector('.shared-connection-sharing').hidden"
            ),
            true
        );
        assert_eq!(
            eval(
                &tab,
                "document.querySelector('.shared-connection-prompt').value"
            ),
            ""
        );
        assert_eq!(
            eval(
                &tab,
                "document.getElementById('sharedConnectionHelp').innerText.includes('Token file')"
            ),
            false
        );
    }
    native.occupied.store(false, Ordering::SeqCst);
    click(&tab, "#sharedConnectionRetry");
    wait(&tab, "document.getElementById('sharedSessionConflict').hidden && !document.getElementById('sharedModelPicker').disabled");
    assert_eq!(
        eval(&tab, "document.getElementById('composeInput').value"),
        "Keep this unsent draft"
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('#sharedImages .shared-image img').length"
        ),
        1
    );
    assert!(
        !native.calls.lock().unwrap().iter().any(|method| matches!(
            method.as_str(),
            "turn/start" | "turn/steer" | "turn/interrupt" | "thread/start"
        )),
        "recovery does not send, interrupt, or replace the session"
    );
    if mobile {
        phone_approval_controls(&tab, &native, &root);
    }
    if mobile {
        let geometry = probe(&tab, "(() => {const r=document.getElementById('composer').getBoundingClientRect(); return {left:r.left,right:r.right,width:innerWidth};})()");
        assert!(
            geometry["left"].as_f64().unwrap() >= -1.0
                && geometry["right"].as_f64().unwrap() <= geometry["width"].as_f64().unwrap() + 1.0,
            "{geometry}"
        );
    }
    assert_eq!(
        eval(
            &tab,
            "getComputedStyle(document.querySelector('#composer>.compose-note')).display"
        ),
        "none"
    );
    assert_eq!(
        eval(&tab, "document.getElementById('sharedNewSession') === null"),
        true
    );
    wait(
        &tab,
        "!!document.querySelector('[data-native-add-project]')",
    );
    click(&tab, "[data-native-add-project]");
    wait(&tab, "!!document.getElementById('sharedProjectName')");
    eval(&tab, "(() => {document.getElementById('sharedProjectName').value='New empty project';document.getElementById('sharedProjectDirectory').value=document.querySelector('[data-native-cwd]').dataset.nativeCwd;return true;})()");
    click(&tab, "#sharedAddProject");
    wait(&tab, "!document.getElementById('sharedNewDialog').open && !!document.querySelector('[data-native-project=added-project]')");
    assert_eq!(eval(&tab, "getComputedStyle(document.querySelector('[data-native-project=added-project]')).backgroundColor"), "rgba(0, 0, 0, 0)");
    let action_geometry = probe(&tab, "(() => {const r=document.querySelector('[data-native-project=added-project]').getBoundingClientRect();return {width:r.width,height:r.height};})()");
    if mobile {
        assert!(
            action_geometry["width"].as_f64().unwrap() >= 44.0
                && action_geometry["height"].as_f64().unwrap() >= 44.0,
            "{action_geometry}"
        );
    }
    click(&tab, "[data-native-project=added-project]");
    wait(
        &tab,
        "document.getElementById('sharedNewDialog')?.open === true",
    );
    assert_eq!(eval(&tab, "document.querySelectorAll('#sharedNewModel,#sharedNewTier,[name=sharedNewEffort]').length"), 0);
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('sharedNewProject').textContent.includes('New empty project')"
        ),
        true
    );
    assert_eq!(eval(&tab, "!!document.getElementById('sharedNewProject') && !!document.getElementById('sharedNewCwd')"), true);
    let dialog_geometry = probe(&tab, "(() => {const r=document.getElementById('sharedNewDialog').getBoundingClientRect();return {left:r.left,right:r.right,width:innerWidth};})()");
    assert!(
        dialog_geometry["left"].as_f64().unwrap() >= 0.0
            && dialog_geometry["right"].as_f64().unwrap()
                <= dialog_geometry["width"].as_f64().unwrap()
    );
    let shot = tab
        .capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap();
    std::fs::write(root.join("native-create.png"), shot).unwrap();
    // Hold the history scan independently of creation; successful creation must open
    // its own empty session immediately, not wait for every native history page.
    eval(
        &tab,
        r#"(() => {
      window.heldNativeScans = []; window.holdNativeScans = true;
      window.nativeUiErrors = []; window.nativeUiStarted = performance.now();
      new MutationObserver(() => {
        const error = document.body.innerText.match(/Cannot read this session|The monitor will retry|Session scan failed/);
        if (error) window.nativeUiErrors.push({message:error[0], afterMs:performance.now()-window.nativeUiStarted});
      }).observe(document.body, {childList:true,subtree:true,characterData:true});
      return true;
    })()"#,
    );
    click(&tab, "#sharedCreateSession");
    wait(&tab, "document.getElementById('sharedNewDialog')?.open === false && document.getElementById('sharedModelPicker')?.disabled === false");
    assert_eq!(
        eval(&tab, "new URL(location.href).searchParams.get('session')"),
        "rollout-native-created"
    );
    wait(
        &tab,
        "document.getElementById('transcript').innerText.includes('Start this session')",
    );
    assert_eq!(
        eval(&tab, "window.heldNativeScans.length > 0"),
        true,
        "Creation finished while its index read was held"
    );
    // This barrier tests creation independence. Release it before screenshot/send
    // work so an unrelated held scan cannot time out on a slower browser host.
    eval(&tab, "(() => {window.holdNativeScans=false; for(const r of window.heldNativeScans) window.nativeTestFetch(r.url,r.options).then(r.resolve,r.reject);return true;})()");
    // Send through the real HTTP gateway to a synthetic native server, never an LLM.
    eval(&tab, "(() => {const i=document.getElementById('composeInput');i.value='Synthetic first message';i.dispatchEvent(new Event('input',{bubbles:true}));return true;})()");
    eval(&tab, "(() => {window.holdNativeSends=true;document.getElementById('sendBtn').addEventListener('click',()=>{window.previewClickAt=performance.now();},{once:true,capture:true});new MutationObserver(()=>{if(window.previewDelayMs==null && document.querySelector('[data-outgoing-state=sending]')) window.previewDelayMs=performance.now()-window.previewClickAt;}).observe(document.getElementById('sharedOutgoing'),{childList:true,subtree:true});return true;})()");
    if mobile {
        tap(&tab, "#composeInput");
    }
    click(&tab, "#sendBtn");
    wait(&tab, "window.heldNativeSends.length === 1 && document.getElementById('sharedOutgoing').innerText.includes('Synthetic first message') && document.querySelector('[data-outgoing-state=sending]') !== null");
    wait(
        &tab,
        "document.querySelector('[data-outgoing-state=sending]')?.checkVisibility() === true",
    );
    let outgoing_geometry = probe(&tab, "(() => {const n=document.querySelector('[data-outgoing-state=sending]'),r=n.getBoundingClientRect(),c=document.getElementById('composer').getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,width:innerWidth,composerTop:c.top,hit:n.contains(document.elementFromPoint(r.left+r.width/2,r.top+r.height/2))};})()");
    assert!(
        outgoing_geometry["left"].as_f64().unwrap() >= 0.0
            && outgoing_geometry["right"].as_f64().unwrap()
                <= outgoing_geometry["width"].as_f64().unwrap()
            && outgoing_geometry["top"].as_f64().unwrap() >= 0.0
            && outgoing_geometry["bottom"].as_f64().unwrap()
                <= outgoing_geometry["composerTop"].as_f64().unwrap()
            && outgoing_geometry["hit"] == true,
        "{outgoing_geometry}"
    );
    let preview_delay = eval(&tab, "window.previewDelayMs");
    assert!(preview_delay.as_f64().unwrap() < 250.0, "{preview_delay}");
    println!("NATIVE_OUTGOING_PREVIEW_MS {preview_delay} mobile={mobile} unix={unix}");
    std::fs::write(
        root.join("native-outgoing.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    eval(&tab, "(() => {window.holdNativeSends=false;for(const r of window.heldNativeSends) window.nativeTestFetch(r.url,r.options).then(r.resolve,r.reject);return true;})()");
    wait(
        &tab,
        "document.getElementById('transcript').innerText.includes('Loading session history…')",
    );
    wait(&tab, "document.getElementById('sharedOutgoing').innerText.includes('Synthetic first message') && document.getElementById('sharedOutgoing').innerText.includes('Sent')");
    if mobile && width > height {
        wait(&tab, "document.getElementById('composer').dataset.sharedCollapsed==='true' && document.getElementById('composeInput').value===''");
    }
    assert_eq!(
        eval(&tab, "window.nativeUiErrors.length"),
        0,
        "{:?}",
        probe(&tab, "({ui:window.nativeUiErrors,index:window.nativeIndexErrors,responses:window.nativeIndexResponses.slice(-2)})")
    );
    let new_history = history
        .replace("native-owned", "native-created")
        .replace("Review the workspace changes.", "Synthetic first message");
    stores.codex_session("native-created", &new_history);
    wait(
        &tab,
        "document.getElementById('transcript').innerText.includes('Synthetic first message') && document.getElementById('sharedOutgoing').children.length === 0",
    );
    assert_eq!(eval(&tab, "window.nativeUiErrors.length"), 0);
    assert!(native
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|method| method == "thread/start"));
    click(&tab, "#sharedModelPicker");
    wait(
        &tab,
        "document.getElementById('sharedSettingsDialog')?.matches(':popover-open') === true",
    );
    assert_eq!(eval(&tab, "!!document.getElementById('sharedModel') && !!document.getElementById('sharedEffortRange') && !!document.getElementById('sharedPickerFast')"), true);
    eval(
        &tab,
        "document.getElementById('sharedSettingsDialog').hidePopover(); true",
    );
    let shot = tab
        .capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap();
    std::fs::write(root.join("native-active.png"), shot).unwrap();
    println!("NATIVE_REVIEW_EVIDENCE {}", root.display());
    native.active.store(true, Ordering::SeqCst);
    if mobile && width > height {
        click(&tab, "[data-session=rollout-native-owned]");
        wait(&tab, "!document.getElementById('sharedInterrupt').hidden && !document.getElementById('sharedInterrupt').disabled");
        eval(&tab, "(()=>{const i=document.getElementById('composeInput');i.value='Synthetic landscape queue';i.dispatchEvent(new Event('input'));return true;})()");
        tap(&tab, "#composeInput");
        click(&tab, "#sendBtn");
        wait(&tab, "document.getElementById('sharedQueue').innerText.includes('Synthetic landscape queue')");
        eval(
            &tab,
            "document.getElementById('jumpToBottom').classList.add('show');true",
        );
        wait(&tab, "(()=>{const j=document.getElementById('jumpToBottom'),r=j.getBoundingClientRect();return r.height>0 && j.contains(document.elementFromPoint(r.left+r.width/2,r.top+r.height/2));})()");
        std::fs::write(
            root.join("native-queued-jump.png"),
            tab.capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                None,
                true,
            )
            .unwrap(),
        )
        .unwrap();
        click(&tab, "[data-queue-action=cancel]");
        wait(
            &tab,
            "document.getElementById('sharedQueue').children.length===0",
        );
        click(&tab, "[data-session=rollout-native-created]");
        wait(
            &tab,
            "document.getElementById('sharedModelPicker').disabled===false",
        );
    }
    click(&tab, "#codexNativeButton");
    wait(
        &tab,
        "document.getElementById('nativeEnable')?.getAttribute('aria-checked') === 'true'",
    );
    assert_eq!(eval(&tab, "document.getElementById('nativeServerStatus').innerText.includes('Monitor is connected.') && document.getElementById('nativeServerStatus').innerText.includes('externally managed server')"), true);
    let menu_geometry = probe(&tab, "(() => {const r=document.getElementById('codexNativeMenu').getBoundingClientRect();return {left:r.left,right:r.right,bottom:r.bottom,width:innerWidth,height:innerHeight};})()");
    assert!(
        menu_geometry["left"].as_f64().unwrap() >= 0.0
            && menu_geometry["right"].as_f64().unwrap() <= menu_geometry["width"].as_f64().unwrap()
            && menu_geometry["bottom"].as_f64().unwrap()
                <= menu_geometry["height"].as_f64().unwrap()
    );
    std::fs::write(
        root.join("native-server-status.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    eval(
        &tab,
        "(() => { window.heldNativeScans=[]; window.holdNativeScans=true; return true; })()",
    );
    wait(&tab, "window.heldNativeScans.length > 0");
    eval(&tab, "(() => {window.externalCloseFetch=window.fetch;window.fetch=async(url,options)=>{const response=await window.externalCloseFetch(url,options);if(String(url)!=='/api/codex/status')return response;const status=await response.json();return new Response(JSON.stringify({...status,activeWork:true,activeTurns:[{threadId:'native-created',threadName:'Synthetic external work',turnId:'external-turn-9'}]}),{status:response.status});};return true;})()");
    click(&tab, "#nativeEnable");
    wait(
        &tab,
        "document.getElementById('nativeStopDialog')?.open === true",
    );
    assert_eq!(eval(&tab, "document.getElementById('nativeStopDescription').innerText.includes('external-turn-9') && document.getElementById('nativeStopDescription').innerText.includes('external server and its turns keep running')"), true);
    std::fs::write(
        root.join("native-external-confirmation.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    click(&tab, "#nativeStopCancel");
    wait(&tab, "!document.getElementById('nativeStopDialog') && !document.getElementById('codexNativeButton').disabled");
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('codexNativeButton').dataset.nativeEnabled"
        ),
        "true"
    );
    click(&tab, "#codexNativeButton");
    click(&tab, "#nativeEnable");
    wait(
        &tab,
        "document.getElementById('nativeStopDialog')?.open === true",
    );
    click(&tab, "#nativeStopConfirm");
    wait(&tab, "document.getElementById('codexNativeButton')?.dataset.nativeEnabled === 'false' && !document.body.hasAttribute('data-shared-codex')");
    eval(&tab, "window.fetch=window.externalCloseFetch;true");
    eval(&tab, "(() => {window.holdNativeScans=false;for(const r of window.heldNativeScans) r.reject(new Error('Stopped native scan'));return true;})()");
    wait(
        &tab,
        "!!document.querySelector('[data-session=ordinary-claude]')",
    );
    assert_eq!(eval(&tab, "window.nativeUiErrors.length"), 0);
    assert_eq!(
        eval(&tab, "!!document.getElementById('codexNativeStyles')"),
        false
    );
    assert_eq!(eval(&tab, "(async()=>{const s=await (await fetch('/api/codex/status')).json();return s.runtime==='available' && s.serverListening;})()"), true, "disconnect preserves shared server");
    assert!(
        native
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "thread/unsubscribe"),
        "Mode off releases the Monitor subscription even with active native work"
    );
    click(&tab, "[data-session=ordinary-claude]");
    assert_eq!(
        eval(&tab, "document.body.hasAttribute('data-shared-codex')"),
        false
    );
    click(&tab, "#codexNativeButton");
    wait(
        &tab,
        "document.getElementById('nativeServerStatus')?.innerText.includes('Native mode is off.')",
    );
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('nativeServerStatus').dataset.state"
        ),
        "running"
    );
    eval(
        &tab,
        "(() => {window.heldNativeScans=[];window.holdNativeScans=true;return true;})()",
    );
    click(&tab, "#nativeEnable");
    wait(&tab, "document.getElementById('codexNativeButton')?.dataset.nativeEnabled === 'true' && document.getElementById('codexNativeMenu').matches(':popover-open')");
    wait(&tab, "document.getElementById('nativeEnable')?.getAttribute('aria-checked') === 'true' && !document.getElementById('nativeEnable').disabled");
    eval(&tab, "(() => {window.holdNativeScans=false;for(const r of window.heldNativeScans) window.nativeTestFetch(r.url,r.options).then(r.resolve,r.reject);document.getElementById('codexNativeMenu').hidePopover();return true;})()");
    assert_eq!(
        eval(&tab, "document.body.hasAttribute('data-shared-codex')"),
        false,
        "Claude stays ordinary even while native is enabled"
    );
    assert!(
        native
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|method| *method == "initialize")
            .count()
            >= 2
    );
    shutdown_confirmation(&tab, &root);
    if !mobile && !unix {
        delivery_recovery(&browser, &tab, &monitor, &native, &root);
    }
}
/// Render actual gateway requests inside the production shell, whose transcript
/// also has an unrelated `.approval` class. Touch both advertised decisions.
fn phone_approval_controls(
    tab: &headless_chrome::Tab,
    native: &native::NativeServer,
    root: &std::path::Path,
) {
    // Exercise real request rendering while varying actionability, without relying
    // on WebKit's child-insertion :has() invalidation or a stylesheet reinsertion.
    eval(
        tab,
        r#"window.requestNavigation = null; import('/codex-workspace/monitor-controls.js').then(({MonitorControls}) => {
      const requests=document.getElementById('sharedRequests'), saved=requests.innerHTML, prior=document.body.dataset.sharedPending;
      let connected=true;
      const controller=Object.assign(Object.create(MonitorControls.prototype), {active:true,state:{pending:[],queue:[]},requests,queue:document.createElement('section'),requestDrafts:new Map(),canControl:()=>connected});
      const results=[];
      try {
        for (const [name,method,canRespond,online,expected] of [
          ['approval','item/commandExecution/requestApproval',true,true,true],
          ['permissions','item/permissions/requestApproval',true,true,true],
          ['questions','item/tool/requestUserInput',true,true,true],
          ['desktop','item/tool/call',false,true,false],
          ['unsupported','unknown/request',true,true,false],
          ['disconnected','item/commandExecution/requestApproval',true,false,false],
          ['reconnected','item/commandExecution/requestApproval',true,true,true],
          ['resolved',null,true,true,false]]) {
          connected=online;
          controller.state.pending=method ? [{id:9,requestToken:name,method,canRespond,params:{threadId:'synthetic',command:'cargo test -p parser',questions:[{id:'q',question:'Continue?'}],permissions:{}}}] : [];
          controller.paintPending();
          document.getElementById('jumpToBottom').classList.add('show');
          const pending=document.body.dataset.sharedPending==='true', jump=document.getElementById('jumpToBottom').checkVisibility();
          results.push({name,pending,jump,pass:pending===expected && jump!==expected});
        }
        window.requestNavigation=results;
      } finally {requests.innerHTML=saved;if(prior===undefined)delete document.body.dataset.sharedPending;else document.body.dataset.sharedPending=prior;}
    }).catch(error=>{window.requestNavigationError=String(error)});true"#,
    );
    until(
        tab,
        "window.requestNavigation?.length===8 && window.requestNavigation.every(r=>r.pass)",
        "actionable requests alone suppress phone navigation",
        Duration::from_secs(10),
        "JSON.stringify({results:window.requestNavigation,error:window.requestNavigationError})",
    );
    std::fs::write(
        root.join("native-request-navigation.json"),
        serde_json::to_vec_pretty(&probe(tab, "window.requestNavigation")).unwrap(),
    )
    .unwrap();
    let viewport = probe(tab, "({width:innerWidth,height:innerHeight})");
    let width = viewport["width"].as_u64().unwrap() as u32;
    let height = viewport["height"].as_u64().unwrap() as u32;
    if width > height {
        phone(tab, width, 292);
    }
    for (theme, decision) in [("light", "decline"), ("dark", "accept")] {
        eval(tab, &format!("document.documentElement.dataset.theme={theme:?};document.getElementById('jumpToBottom').classList.add('show');true"));
        native.approval_requested.store(true, Ordering::SeqCst);
        wait(tab, "document.querySelector('#sharedRequests [data-testid=approval-card]')?.checkVisibility() === true");
        let geometry = r#"(()=>{const card=document.querySelector('#sharedRequests [data-testid=approval-card]'),pre=card.querySelector('pre'),r=card.getBoundingClientRect(),p=pre.getBoundingClientRect();return {display:getComputedStyle(card).display,cardWidth:r.width,commandWidth:p.width,command:pre.textContent,jump:document.getElementById('jumpToBottom').checkVisibility()};})()"#;
        until(tab, &format!("(()=>{{const g={geometry};return ['block','grid'].includes(g.display) && g.command==='cargo test -p parser' && g.commandWidth>=240 && g.commandWidth>=g.cardWidth-40 && !g.jump && document.body.dataset.sharedPending==='true';}})()"),
            "phone approval command uses the card width without a jump overlay", Duration::from_secs(10), &format!("JSON.stringify({geometry})"));
        std::fs::write(
            root.join(format!("native-approval-{theme}-geometry.json")),
            serde_json::to_vec_pretty(&probe(tab, geometry)).unwrap(),
        )
        .unwrap();
        // Do not scroll to reveal either the command or the decisions: the initial
        // card must expose them even with Safari's 292px landscape content area.
        wait(
            tab,
            r#"(()=>{const pre=document.querySelector('#sharedRequests pre'),r=pre.getBoundingClientRect();return r.height>=16 && pre.contains(document.elementFromPoint(r.left+r.width/2,r.top+8));})()"#,
        );
        std::fs::write(
            root.join(format!("native-approval-{theme}-command.png")),
            tab.capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                None,
                true,
            )
            .unwrap(),
        )
        .unwrap();
        // Both decisions must already be visible and hit-testable without scrolling.
        wait(
            tab,
            r#"[...document.querySelectorAll('#sharedRequests [data-decision]')].length===2 && [...document.querySelectorAll('#sharedRequests [data-decision]')].every(b=>{const r=b.getBoundingClientRect();return !b.disabled && r.width>=44 && r.height>=44 && [0.2,0.5,0.8].every(x=>[0.1,0.5,0.9].every(y=>b.contains(document.elementFromPoint(r.left+r.width*x,r.top+r.height*y))));})"#,
        );
        assert_eq!(
            eval(
                tab,
                "document.getElementById('jumpToBottom').checkVisibility()"
            ),
            false
        );
        std::fs::write(
            root.join(format!("native-approval-{theme}-actions.png")),
            tab.capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                None,
                true,
            )
            .unwrap(),
        )
        .unwrap();
        let before = native.approval_decisions.lock().unwrap().len();
        tap(tab, &format!("#sharedRequests [data-decision={decision}]"));
        wait(
            tab,
            "document.getElementById('sharedRequests').children.length===0",
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while native.approval_decisions.lock().unwrap().len() == before
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(native.approval_decisions.lock().unwrap().len(), before + 1);
        assert_eq!(
            native.approval_decisions.lock().unwrap()[before]["decision"],
            decision
        );
        eval(
            tab,
            "document.getElementById('jumpToBottom').classList.add('show');true",
        );
        wait(
            tab,
            "document.getElementById('jumpToBottom').checkVisibility() && document.body.dataset.sharedPending==='false'",
        );
    }
    eval(tab, "document.documentElement.dataset.theme='';true");
    if width > height {
        phone(tab, width, height);
    }
}
/// Chrome models viewport geometry and touch events; iOS keyboard/hover acceptance
/// still requires a device or simulator and is not inferred from these checks.
fn phone_native_controls(tab: &headless_chrome::Tab, root: &std::path::Path) {
    let sideways = eval(tab, "matchMedia('(orientation:landscape)').matches") == true;
    if sideways {
        // Start collapsed; a touch expands, an outside touch folds without losing text.
        tap(tab, "#sharedReaderTools");
        wait(
            tab,
            "document.getElementById('composer').dataset.sharedCollapsed==='true'",
        );
        eval(tab, "(()=>{const i=document.getElementById('composeInput');i.value='Retain sideways draft';i.dispatchEvent(new Event('input'));return true;})()");
        let collapsed = probe(tab, "(()=>{const c=document.getElementById('composer'),r=c.getBoundingClientRect();return {height:r.height,send:document.getElementById('sendBtn').checkVisibility(),input:document.getElementById('composeInput').getBoundingClientRect().height};})()");
        assert!(
            collapsed["height"].as_f64().unwrap() <= 60.0
                && collapsed["send"] == false
                && collapsed["input"].as_f64().unwrap() >= 24.0,
            "{collapsed}"
        );
        std::fs::write(
            root.join("native-landscape-collapsed.png"),
            tab.capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                None,
                true,
            )
            .unwrap(),
        )
        .unwrap();
        // Portrait editing must not leave an unfocused composer expanded on rotation.
        let width = eval(tab, "innerWidth").as_u64().unwrap() as u32;
        let height = eval(tab, "innerHeight").as_u64().unwrap() as u32;
        phone(tab, height, width);
        tap(tab, "#composeInput");
        eval(tab, "document.getElementById('composeInput').blur();true");
        phone(tab, width, height);
        wait(
            tab,
            "document.getElementById('composer').dataset.sharedCollapsed==='true'",
        );
        tap(tab, "#composeInput");
        wait(tab, "document.getElementById('composer').dataset.sharedCollapsed==='false' && document.getElementById('sendBtn').checkVisibility() && document.activeElement.id==='composeInput' && !document.body.hasAttribute('data-shared-reader-open')");
        tap(tab, "#sharedReaderTools");
        wait(
            tab,
            "document.getElementById('composer').dataset.sharedCollapsed==='true'",
        );
        assert_eq!(
            eval(tab, "document.getElementById('composeInput').value"),
            "Retain sideways draft"
        );
        tap(tab, "#sharedReaderTools");
    }

    assert!(
        eval(
            tab,
            "parseFloat(getComputedStyle(document.getElementById('composeInput')).fontSize)"
        )
        .as_f64()
        .unwrap()
            >= 16.0
    );
    assert_eq!(
        eval(
            tab,
            "getComputedStyle(document.getElementById('phoneDock')).display"
        ),
        "none"
    );
    tap(tab, "#sharedReaderTools");
    wait(tab, "document.body.hasAttribute('data-shared-reader-open') && document.getElementById('phoneDock').checkVisibility()");
    let dock = probe(tab, "(()=>{const d=document.getElementById('phoneDock'),r=d.getBoundingClientRect();const sides=innerWidth>innerHeight?62:0;return {height:r.height,left:r.left,right:r.right,width:innerWidth,sides,overflow:d.scrollWidth>d.clientWidth+1};})()");
    assert!(
        dock["height"].as_f64().unwrap() <= 72.0
            && dock["left"].as_f64().unwrap() >= dock["sides"].as_f64().unwrap()
            && dock["right"].as_f64().unwrap()
                <= dock["width"].as_f64().unwrap() - dock["sides"].as_f64().unwrap()
            && dock["overflow"] == false,
        "single-row dock inside safe areas: {dock}"
    );
    std::fs::write(
        root.join("native-reader-tools.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    // The jump is centred on phones; it must sit above both native input and dock.
    eval(
        tab,
        "document.getElementById('jumpToBottom').classList.add('show');true",
    );
    wait(tab, "(()=>{const j=document.getElementById('jumpToBottom').getBoundingClientRect(),d=document.getElementById('phoneDock').getBoundingClientRect();return j.height>0 && j.bottom<=d.top;})()");
    tap(tab, ".header-searchbox");
    wait(
        tab,
        "document.querySelector('.header-searchbox').classList.contains('phone-open')",
    );
    tap(tab, "#filterTranscriptBtn");
    wait(
        tab,
        "document.getElementById('filterTranscriptBtn').getAttribute('aria-expanded')==='true'",
    );
    tap(tab, "#composeInput");
    wait(tab, "!document.body.hasAttribute('data-shared-reader-open') && document.getElementById('filterTranscriptBtn').getAttribute('aria-expanded')!=='true'");
    eval(tab, "(()=>{Object.defineProperty(visualViewport,'height',{configurable:true,get:()=>innerHeight-Math.min(300,innerHeight/3)});visualViewport.dispatchEvent(new Event('resize'));return true;})()");
    wait(
        tab,
        "document.getElementById('app').classList.contains('keyboard-up')",
    );
    // Model Safari's page scroll AND viewport offsets, including the observed 337 px.
    // All assertions use the app box, as do the shell's phone-top/phone-bottom rules.
    eval(tab, "(()=>{Object.defineProperty(visualViewport,'offsetTop',{configurable:true,get:()=>337});Object.defineProperty(visualViewport,'pageTop',{configurable:true,get:()=>337});visualViewport.dispatchEvent(new Event('scroll'));return true;})()");
    let geometry_script = "(()=>{const a=document.getElementById('app').getBoundingClientRect(),c=document.getElementById('composer').getBoundingClientRect(),i=document.getElementById('composeInput').getBoundingClientRect(),b=document.getElementById('sendBtn').getBoundingClientRect(),j=document.getElementById('jumpToBottom').getBoundingClientRect();return {left:c.left,right:c.right,bottom:c.bottom-a.top,input:i.height,button:b.height,contained:b.top>=c.top&&b.bottom<=c.bottom+1,jumpClear:j.bottom<=c.top,width:innerWidth,height:visualViewport.height};})()";
    until(tab, &format!("(()=>{{const g={geometry_script};return g.input>=30 && g.button>=44 && g.contained && g.jumpClear && g.bottom<=g.height+1;}})()"),
        "native input and send controls remain visible after Safari keyboard scrolling", Duration::from_secs(10), geometry_script);
    let geometry = probe(tab, geometry_script);
    std::fs::write(
        root.join("native-keyboard-geometry.json"),
        serde_json::to_vec_pretty(&geometry).unwrap(),
    )
    .unwrap();
    assert!(
        geometry["left"].as_f64().unwrap() >= 0.0
            && geometry["right"].as_f64().unwrap() <= geometry["width"].as_f64().unwrap(),
        "{geometry}"
    );
    if sideways {
        eval(tab, "document.getElementById('sharedReaderTools').dispatchEvent(new PointerEvent('pointerdown',{bubbles:true}));true");
        assert_eq!(
            eval(
                tab,
                "document.getElementById('composer').dataset.sharedCollapsed"
            ),
            "false",
            "starting a touch scroll cannot change transcript padding"
        );
        eval(tab, "document.getElementById('sharedReaderTools').dispatchEvent(new MouseEvent('click',{bubbles:true}));true");
        wait(
            tab,
            "document.getElementById('composer').dataset.sharedCollapsed==='true'",
        );
        eval(
            tab,
            "document.getElementById('composeInput').dispatchEvent(new Event('focus'));true",
        );
        wait(tab, "document.getElementById('composer').dataset.sharedCollapsed==='false' && document.getElementById('sendBtn').checkVisibility()");
    }
    // The offset probe above shadows Safari's scroll coordinates without physically
    // scrolling Chrome. Restore those shadows for a screenshot/hit test in its real viewport.
    eval(tab, "(()=>{delete visualViewport.offsetTop;delete visualViewport.pageTop;visualViewport.dispatchEvent(new Event('scroll'));document.getElementById('jumpToBottom').classList.add('show');return true;})()");
    wait(tab, "(()=>{const b=document.getElementById('sendBtn'),r=b.getBoundingClientRect(),j=document.getElementById('jumpToBottom'),c=document.getElementById('composer').getBoundingClientRect();return r.height>=44 && r.bottom<=visualViewport.height && b.contains(document.elementFromPoint(r.left+r.width/2,r.top+r.height/2)) && j.getBoundingClientRect().bottom<=c.top;})()");
    std::fs::write(
        root.join("native-keyboard.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    if sideways {
        eval(tab, "(()=>{Object.defineProperty(visualViewport,'height',{configurable:true,get:()=>16});visualViewport.dispatchEvent(new Event('resize'));return true;})()");
        let minimal = "(()=>{const a=document.getElementById('app').getBoundingClientRect(),c=document.getElementById('composer'),r=c.getBoundingClientRect(),i=document.getElementById('composeInput');return {inputOnly:c.dataset.sharedInputOnly,top:r.top-a.top,bottom:r.bottom-a.top,height:r.height,inputHeight:i.clientHeight,lineHeight:parseFloat(getComputedStyle(i).lineHeight),send:document.getElementById('sendBtn').checkVisibility()};})()";
        until(tab, &format!("(()=>{{const g={minimal};return g.inputOnly==='true' && g.top>=0 && g.bottom<=17 && g.height<=16 && g.lineHeight<=g.inputHeight && !g.send && !document.querySelector('.workspace>.topbar').checkVisibility({{visibilityProperty:true}}) && (()=>{{const i=document.getElementById('composeInput'),r=i.getBoundingClientRect();return i.contains(document.elementFromPoint(r.left+r.width/2,r.top+r.height/2));}})();}})()"),
            "tiny Safari viewport clips to one bottom input", Duration::from_secs(10), minimal);
        std::fs::write(
            root.join("native-tiny-keyboard-geometry.json"),
            serde_json::to_vec_pretty(&probe(tab, minimal)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("native-tiny-keyboard.png"),
            tab.capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                None,
                true,
            )
            .unwrap(),
        )
        .unwrap();
        eval(tab, "(()=>{Object.defineProperty(visualViewport,'height',{configurable:true,get:()=>innerHeight-Math.min(300,innerHeight/3)});visualViewport.dispatchEvent(new Event('resize'));return true;})()");
        wait(tab, "document.getElementById('composer').dataset.sharedInputOnly==='false' && document.getElementById('sendBtn').checkVisibility()");
    }
    eval(tab, "(()=>{Object.defineProperty(visualViewport,'scale',{configurable:true,get:()=>2});Object.defineProperty(visualViewport,'pageTop',{configurable:true,get:()=>120});visualViewport.dispatchEvent(new Event('scroll'));return true;})()");
    wait(tab, "document.getElementById('app').style.transform==='' && !document.getElementById('app').classList.contains('keyboard-up')");
    eval(tab, "(()=>{delete visualViewport.height;delete visualViewport.scale;delete visualViewport.pageTop;delete visualViewport.offsetTop;document.activeElement.blur();visualViewport.dispatchEvent(new Event('resize'));return true;})()");
}
/// Reload uses the real browser journal and gateway receipts, including an accepted
/// response lost by the network and an independent second tab claiming the same input.
fn delivery_recovery(
    browser: &headless_chrome::Browser,
    tab: &headless_chrome::Tab,
    monitor: &Monitor,
    native: &native::NativeServer,
    root: &std::path::Path,
) {
    fn enable(tab: &headless_chrome::Tab) {
        wait(tab, "!!document.getElementById('codexNativeButton')");
        if eval(
            tab,
            "document.getElementById('writeSwitch').getAttribute('aria-checked')",
        ) != "true"
        {
            click(tab, "#writeSwitch");
        }
        click(tab, "#codexNativeButton");
        click(tab, "#nativeEnable");
        wait(
            tab,
            "document.getElementById('codexNativeButton').dataset.nativeEnabled === 'true'",
        );
        eval(
            tab,
            "document.getElementById('codexNativeMenu').hidePopover();true",
        );
        wait(
            tab,
            "!!document.querySelector('[data-session=rollout-native-created]')",
        );
        click(tab, "[data-session=rollout-native-created]");
        wait(
            tab,
            "document.getElementById('sharedModelPicker')?.disabled === false",
        );
    }
    let count = || {
        native
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.as_str() == "thread/queue/add")
            .count()
    };
    monitor.open(tab, "/?ui=app&session=rollout-native-created");
    eval(
        tab,
        r#"(() => {
      const original=window.fetch; window.missingReceipt=true;
      window.fetch=async(url,options)=>{
        if(String(url).startsWith('/api/codex/receipt') && window.missingReceipt) return new Response(JSON.stringify({ok:true,result:null}));
        const response=await original(url,options);
        if(String(url)==='/api/codex/send') {await response.clone().json(); throw new TypeError('Synthetic lost HTTP response after acceptance');}
        return response;
      }; return true;
    })()"#,
    );
    enable(tab);
    let before = count();
    eval(
        tab,
        r#"(() => {
      const input=document.getElementById('composeInput'); input.value='Synthetic lost receipt'; input.dispatchEvent(new Event('input'));
      const bytes=Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jBz8AAAAASUVORK5CYII='),c=>c.charCodeAt(0));
      const files=new DataTransfer(); files.items.add(new File([bytes],'recovery.png',{type:'image/png'}));
      const picker=document.getElementById('sharedImageInput');picker.files=files.files;picker.dispatchEvent(new Event('change'));return true;
    })()"#,
    );
    wait(tab, "document.querySelectorAll('#sharedImages img').length===1 && !document.getElementById('sendBtn').disabled");
    click(tab, "#sendBtn");
    wait(tab, "!!document.querySelector('[data-outgoing-state=uncertain]') && !document.getElementById('sendBtn').disabled");
    assert_eq!(count(), before + 1);
    let id = eval(
        tab,
        "document.querySelector('[data-check-outgoing]').dataset.checkOutgoing",
    );
    click(tab, "[data-check-outgoing]");
    wait(
        tab,
        "document.getElementById('sharedError').innerText.includes('No receipt was found')",
    );
    assert_eq!(count(), before + 1, "missing receipts never replay a send");
    let second = browser.new_tab().unwrap();
    monitor.open(&second, "/?ui=app");
    let claimed = eval(
        &second,
        r#"(async()=>{
      const {DeliveryJournal}=await import('/codex-workspace/delivery-journal.js');
      const {createActionId}=await import('/codex-workspace/api.js');
      const s=await (await fetch('/api/codex/status')).json();
      const j=new DeliveryJournal(JSON.stringify([s.endpoint,s.codexHome,s.allowedRoots]),DeliveryJournal.tab(createActionId));
      const saved=await j.load();const entry=saved.attempts.find(e=>e.payload.text==='Synthetic lost receipt');
      if(!entry) throw new Error('Missing durable attempt');
      const claim=await j.prepare(createActionId(),entry.payload,entry.preview);
      return claim.recovered && claim.id;
    })()"#,
    );
    assert_eq!(
        claimed, id,
        "second tab atomically reuses the durable action ID"
    );
    second.close(true).unwrap();
    monitor.open(tab, "/?ui=app&session=rollout-native-created");
    enable(tab);
    wait(tab, "!!document.querySelector('[data-outgoing-state=uncertain]') && document.querySelectorAll('#sharedImages img').length===1");
    assert_eq!(
        eval(
            tab,
            "document.querySelector('[data-check-outgoing]').dataset.checkOutgoing"
        ),
        id
    );
    assert_eq!(
        eval(tab, "document.getElementById('composeInput').value"),
        "Synthetic lost receipt"
    );
    assert_eq!(count(), before + 1, "reload restores state without sending");
    click(tab, "[data-check-outgoing]");
    wait(tab, "!!document.querySelector('[data-outgoing-state=accepted]') && document.getElementById('composeInput').value==='' && document.querySelectorAll('#sharedImages img').length===0");
    assert_eq!(count(), before + 1, "receipt recovery is read-only");
    std::fs::write(
        root.join("native-recovered.png"),
        tab.capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .unwrap(),
    )
    .unwrap();
    // Repeating accepted text is a deliberate new action, not an old receipt lookup.
    eval(tab, "(()=>{const i=document.getElementById('composeInput');i.value='Synthetic lost receipt';i.dispatchEvent(new Event('input'));return true;})()");
    click(tab, "#sendBtn");
    wait(tab, "document.getElementById('composeInput').value==='' && !document.querySelector('[data-outgoing-state=sending]')");
    assert_eq!(count(), before + 2);
    eval(tab, "(()=>{const i=document.getElementById('composeInput');i.value='Keep through disconnect';i.dispatchEvent(new Event('input'));return true;})()");
    native.disconnected.store(true, Ordering::SeqCst);
    wait(tab, "document.getElementById('sharedControlStatus').hidden===false && document.getElementById('sendBtn').disabled");
    native.disconnected.store(false, Ordering::SeqCst);
    click(tab, "#sharedControlStatus button");
    wait(tab, "document.getElementById('sharedModelPicker').disabled===false && document.getElementById('sendBtn').disabled===false");
    assert_eq!(
        eval(tab, "document.getElementById('composeInput').value"),
        "Keep through disconnect"
    );
    assert_eq!(count(), before + 2, "reconnect never replays accepted work");
    native.active.store(true, Ordering::SeqCst);
    click(tab, "[data-session=rollout-native-owned]");
    wait(tab,"!document.getElementById('sharedInterrupt').hidden && !document.getElementById('sharedInterrupt').disabled");
    eval(tab,"(()=>{document.querySelectorAll('.shared-image-remove').forEach(b=>b.click());const i=document.getElementById('composeInput');i.value='Synthetic queued message';i.dispatchEvent(new Event('input'));return true;})()");
    click(tab, "#sendBtn");
    wait(tab,"document.getElementById('sharedQueue').innerText.includes('Synthetic queued message') && !document.getElementById('sharedInterrupt').disabled");
    click(tab, "#sharedInterrupt");
    wait(tab, "document.getElementById('sharedInterrupt').hidden");
    assert!(native
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|m| m == "turn/interrupt"));
    click(tab, "[data-queue-action=cancel]");
    wait(
        tab,
        "document.getElementById('sharedQueue').children.length===0",
    );
    assert!(native
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|m| m == "thread/queue/delete"));
}
/// #s257 (found by the review of #s248): native mode's composer stays open over the transcript's
/// foot, and a search step only counted the ordinary box as covering it. A step lands the match's
/// record near the top; the only match here is paragraph 14 of a 30-paragraph answer, with a few
/// draft lines in the composer, so the landing alone leaves it under the composer. The step must
/// leave it in sight above the composer, its record's head above the column (only the reveal does
/// that).
#[test]
#[ignore = "requires release native Monitor and Chrome"]
fn native_desktop_search_step_never_leaves_a_match_under_the_composer() {
    let _serial = serial();
    let root = base("native-desktop-search");
    let stores = Stores::new(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let tall = (1..=30)
        .map(|k| {
            if k == 14 {
                "paragraph 14 holds the needle the search looks for.".to_string()
            } else if k == 8 {
                "paragraph 8 holds the marker the second search looks for.".to_string()
            } else {
                format!("paragraph {k} of the long answer.")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut items = vec![
        serde_json::json!({"type":"session_meta","payload":{"id":"native-search","cwd":project,"originator":"codex_cli_rs"}}),
    ];
    let turns = [
        ("give me the long answer".to_string(), tall),
        ("and a short one".into(), "short.".into()),
        ("another".into(), "also short.".into()),
        ("last".into(), "done.".into()),
    ];
    for (k, (ask, answer)) in turns.iter().enumerate() {
        items.push(serde_json::json!({"timestamp":format!("2026-10-19T04:0{k}:00Z"),"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":ask}]}}));
        items.push(serde_json::json!({"timestamp":format!("2026-10-19T04:0{k}:01Z"),"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":answer}]}}));
    }
    let history = items
        .iter()
        .map(|value| value.to_string() + "\n")
        .collect::<String>();
    let rollout = stores.codex_session("native-search", &history);
    let native = native::NativeServer::start(&project, &root.join("state-19844"), &rollout);
    let endpoint = native.endpoint.clone();
    let args = vec![
        "--codex-server-url",
        endpoint.as_str(),
        "--codex-allowed-root",
        project.to_str().unwrap(),
    ];
    let monitor = Monitor::spawn_with_args(
        Kind::V1,
        19844,
        &root,
        Some(&stores),
        true,
        &[
            ("HOME", root.to_str().unwrap()),
            (
                "CODEX_NATIVE_BINARY",
                "/nonexistent/native-fixture-must-not-launch-codex",
            ),
        ],
        &args,
    );
    let (_browser, tab) = chrome_tab();
    harness::resize(&tab, 1280.0, 900.0);
    monitor.pair(&tab);
    monitor.open(&tab, "/?ui=app");
    wait(
        &tab,
        "!!document.getElementById('codexNativeButton') && innerWidth >= 1200",
    );
    click(&tab, "#filterBtn");
    click(&tab, "[data-filter-reset]");
    click(&tab, "#filterBtn");
    click(&tab, "#writeSwitch");
    wait(&tab, "!document.getElementById('codexNativeButton').hidden");
    click(&tab, "#codexNativeButton");
    wait(
        &tab,
        "document.getElementById('nativeEnable')?.getAttribute('aria-checked') === 'false'",
    );
    click(&tab, "#nativeEnable");
    wait(
        &tab,
        "document.getElementById('codexNativeButton')?.dataset.nativeEnabled === 'true'",
    );
    wait(
        &tab,
        "!!document.querySelector('[data-session=rollout-native-search]')",
    );
    tab.press_key("Escape").unwrap();
    click(&tab, "[data-session=rollout-native-search]");
    wait(&tab, "document.body.dataset.sharedCodex === 'true' && document.getElementById('composer').classList.contains('production-show') && document.getElementById('transcript').innerText.includes('paragraph 30')");
    eval(
        &tab,
        "(() => { const i = document.getElementById('composeInput'); i.value = 'draft line 1\\ndraft line 2\\ndraft line 3\\ndraft line 4\\ndraft line 5'; i.dispatchEvent(new Event('input', { bubbles: true })); return true; })()",
    );
    eval(&tab, "(() => { const b = document.getElementById('transcriptSearchInput'); b.value = 'needle'; b.dispatchEvent(new Event('input', { bubbles: true })); return true; })()");
    wait(
        &tab,
        "!!document.querySelector('.virtual-window mark.search-mark.current')",
    );
    harness::pause("the search to settle", Duration::from_millis(600));
    click(&tab, "#findNext");
    // The composer and, when it shows, the runtime panel above it are what cover the foot.
    let seen = "(() => { const m = document.querySelector('.virtual-window mark.search-mark.current'), t = document.getElementById('transcript').getBoundingClientRect(); if (!m) return 'none'; const covers = [document.getElementById('composer'), document.querySelector('.shared-runtime')].filter(e => e && getComputedStyle(e).display !== 'none' && e.getBoundingClientRect().height > 0).map(e => e.getBoundingClientRect().top); const floor = Math.min(t.bottom, ...covers); const r = m.getBoundingClientRect(), rec = m.closest('[data-block-index]').getBoundingClientRect(); return JSON.stringify({ above: r.bottom <= floor + 0.5 && r.top >= t.top - 0.5, revealed: rec.top < t.top - 0.5, mark: [Math.round(r.top), Math.round(r.bottom)], record: Math.round(rec.top), floor: Math.round(floor), col: [Math.round(t.top), Math.round(t.bottom)] }); })()";
    until(
        &tab,
        &format!("(() => {{ const g = {seen}; return g !== 'none' && JSON.parse(g).above && JSON.parse(g).revealed; }})()"),
        "a step to leave the match in sight above native mode's composer",
        Duration::from_secs(5),
        seen,
    );
    // With the runtime panel showing above the composer (a queued message, a pending approval), the
    // panel is what covers the foot. "marker" (paragraph 8) lands between the panel's top and the
    // composer's: under the panel, though above the composer, so only a floor that counts the panel
    // reveals it.
    eval(&tab, "(() => { const q = document.querySelector('.shared-runtime .shared-queue'); const d = document.createElement('div'); d.style.height = '150px'; d.textContent = 'queued'; q.append(d); return true; })()");
    wait(&tab, "getComputedStyle(document.querySelector('.shared-runtime')).display !== 'none' && document.querySelector('.shared-runtime').getBoundingClientRect().height > 100");
    eval(&tab, "(() => { const b = document.getElementById('transcriptSearchInput'); b.value = 'marker'; b.dispatchEvent(new Event('input', { bubbles: true })); return true; })()");
    wait(
        &tab,
        "!!document.querySelector('.virtual-window mark.search-mark.current') && /marker/.test(document.querySelector('.virtual-window mark.search-mark.current').textContent)",
    );
    harness::pause("the search to settle", Duration::from_millis(600));
    click(&tab, "#findNext");
    until(
        &tab,
        &format!("(() => {{ const g = {seen}; return g !== 'none' && JSON.parse(g).above && JSON.parse(g).revealed; }})()"),
        "a step to leave the match in sight above native mode's runtime panel",
        Duration::from_secs(5),
        seen,
    );
}
#[test]
#[ignore = "requires release native Monitor and Chrome"]
fn native_desktop_lifecycle() {
    run(false, false);
}
#[test]
#[ignore = "requires release native Monitor and Chrome"]
fn native_phone_lifecycle() {
    run(true, false);
}

#[test]
#[ignore = "requires release native Monitor"]
fn native_cli_configuration_does_not_implicitly_pair() {
    let _serial = serial();
    let root = base("native-no-implicit-pair");
    let stores = Stores::new(&root);
    let native = native::NativeServer::start(
        &root,
        &root.join("state-19843"),
        &root.join("rollout-native-owned.jsonl"),
    );
    let monitor = Monitor::spawn_with_args(
        Kind::V1,
        19843,
        &root,
        Some(&stores),
        false,
        &[("HOME", root.to_str().unwrap())],
        &["--codex-server-url", &native.endpoint],
    );
    assert!(monitor.token().is_none());
    assert!(native.calls.lock().unwrap().is_empty());
    assert!(!monitor.state.join("codex-gateway.sqlite3").exists());
}

#[test]
#[ignore = "requires release native Monitor and Chrome"]
fn native_unix_desktop_lifecycle() {
    run(false, true);
}
#[test]
#[ignore = "requires release native Monitor and Chrome"]
fn native_unix_phone_lifecycle() {
    run(true, true);
}

#[test]
#[ignore = "requires release native Monitor and Chrome"]
fn native_phone_landscape_lifecycle() {
    run_sized(true, false, 844, 390);
}
#[test]
#[ignore = "requires release native Monitor and Chrome"]
fn native_wide_phone_unix_lifecycle() {
    run_sized(true, true, 926, 428);
}
