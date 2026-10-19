//! Native controls use the established Monitor/Stores/Chrome harness and synthetic protocol data.
mod harness;
#[path = "harness/native.rs"]
mod native;
use harness::{base, chrome_tab, eval, phone, probe, serial, until, Kind, Monitor, Stores};
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
        return new Response(JSON.stringify({...status,homeProvenance:'monitor-started',ownedServerPid:123}),{status:response.status});
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
    let _serial = serial();
    let root = base(match (mobile, unix) {
        (false, false) => "native-desktop",
        (true, false) => "native-phone",
        (false, true) => "native-unix-desktop",
        (true, true) => "native-unix-phone",
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
    // Unix cases exercise the real default, with no endpoint flag or inherited override.
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
    let args = if unix {
        vec!["--codex-allowed-root", project.to_str().unwrap()]
    } else {
        vec![
            "--codex-server-url",
            native.endpoint.as_str(),
            "--codex-allowed-root",
            project.to_str().unwrap(),
        ]
    };
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
    let (_browser, tab) = chrome_tab();
    if mobile {
        phone(&tab, 390, 844);
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
      window.nativeTestFetch = window.fetch; window.heldNativeScans = []; window.holdNativeScans = false; window.heldNativeSends = []; window.holdNativeSends = false;
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
    assert!(
        native.calls.lock().unwrap()[before..]
            .iter()
            .all(|method| matches!(method.as_str(), "thread/read" | "thread/resume")),
        "retry only checks ownership"
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
    assert_eq!(prompt.contains("Authorization: Bearer"), !unix);
    assert!(prompt.contains("one-line terminal command"));
    assert!(prompt.contains("Do not execute it or quit/stop any App or server from this chat"));
    let token_file = root
        .join(if mobile { "state-19842" } else { "state-19841" })
        .join("codex-server.token");
    if unix {
        assert!(!token_file.exists());
    } else {
        assert!(prompt.contains(token_file.to_str().unwrap()));
        assert!(!prompt.contains(std::fs::read_to_string(&token_file).unwrap().trim()));
    }
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
    assert_eq!(
        eval(&tab, "window.nativeUiErrors.length"),
        0,
        "{:?}",
        eval(&tab, "window.nativeUiErrors")
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
    click(&tab, "#nativeEnable");
    wait(&tab, "document.getElementById('codexNativeButton')?.dataset.nativeEnabled === 'false' && !document.body.hasAttribute('data-shared-codex')");
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
