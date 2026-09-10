//! **#174 P2 — the rendering-control audit, by MEASURED EFFECT.**
//!
//! Every previous equivalence audit in this repo was built on a hand-made list: someone
//! enumerates the controls, enumerates the renderings, checks the cells. That method has the
//! bug it is auditing for — if a person has to remember to add a rendering to the list, the
//! audit inherits the omission that produced #161 (wrap missed bash output), #173 (size never
//! reached `.codebox .lines`) and #98 (a scroll rule with no case on one surface).
//!
//! So nothing here is enumerated.
//!
//! * The CORPUS is derived (P1): `claude_replay_html::html_export::audit::audit_jsonl()` is the
//!   Claude-shaped twin of `audit_corpus()`, and `the_two_corpora_cover_the_same_cells` (in the
//!   html crate, beside the corpus, because `parse_session_as` is crate-private below) proves
//!   the twin reaches every (kind, part) cell the derived corpus declares. This file serves that
//!   twin, so the DOM under measurement is the whole rendering matrix and not a sample.
//! * The EFFECT SET is measured, not read off the stylesheet: snapshot EVERY longhand computed
//!   property of EVERY element under the transcript, toggle the control, snapshot again, diff.
//!   Choosing which properties to watch is the same memory-based omission, so none are chosen —
//!   `for (let i = 0; i < cs.length; i++)` takes whatever the engine resolved.
//! * The CLAIM is written down as a predicate quantified over the DOM, with no selector list in
//!   it, so a rendering nobody thought of is inside the quantifier by construction. `.lines` sets
//!   `font:12px/1.8 var(--mono)` and the SHORTHAND resets font-size — a rule can be present and
//!   still lose, which only measured effect sees.
//!
//! Element identity is `<record id>/<child-index chain from that record's root>`, never a CSS
//! class: a control that re-renders replaces the nodes (the lesson #180 learned holding a node
//! by reference and reading a constant −900), the classes legitimately differ between the two
//! pages, and P3 has to compare across them. The walk stops at a NESTED record root so each
//! element is keyed once, under the record it belongs to.
//!
//! Run: `cargo build --release -p claude-monitor-v2 && cargo test -p claude-replay-browser-tests
//! --test rendering_audit -- --ignored`.

mod harness;

use claude_replay_html::html_export::audit::{
    audit_corpus, audit_jsonl, audit_kinds, audit_records,
};
use harness::{
    base, eval, jump_to_end, long_session, probe, serial, Kind, Monitor, Shape, Stores, Surface,
};
use std::path::PathBuf;
use std::time::Duration;

const SID: &str = "aaaaaaaa-0000-4000-8000-000000000174";

/// Kinds the corpus declares that reach the DOM only NESTED inside another record. Measured, not
/// assumed — and asserted as an equality, so a kind that starts or stops appearing at the top
/// level fails here rather than shrinking the audit in silence.
const NESTED_ONLY: [&str; 0] = [];

struct Fixture {
    base: PathBuf,
    path: PathBuf,
}

/// The audit corpus, served: padding first so the page's opener has its several viewports, then
/// the twin, so every cell of the matrix sits at the TAIL where a `jump_to_end` mounts it.
fn fixture_audit(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(16, Shape::default());
    jsonl += &audit_jsonl();
    let path = stores.claude_session(SID, &jsonl);
    Fixture { base, path }
}

struct Opened {
    tab: std::sync::Arc<headless_chrome::Tab>,
    _browser: headless_chrome::Browser,
    _monitor: Monitor,
}

/// Both surfaces on ONE v2 monitor over the SAME store and session — the classic page as the
/// splice (`?ui=classic`), the app shell as the default. P3 compares the two effect sets, so
/// they must come from one server and one transcript, not two fixtures that agree by luck.
fn open(surface: Surface, fx: &Fixture, port: u16) -> Opened {
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    let stores = Stores {
        root: fx.base.join("stores"),
    };
    let monitor = Monitor::spawn(Kind::V2, port, &fx.base, Some(&stores), true);
    monitor.pair(&tab);
    let sid = fx.path.file_stem().unwrap().to_string_lossy().to_string();
    let (query, ready, diag) = match surface {
        Surface::Classic => (format!("?ui=classic&session={sid}"), "document.querySelectorAll('#stream .blk').length >= 3 && document.body.scrollHeight > window.innerHeight * 3", "document.querySelectorAll('#stream [data-turn]').length"),
        Surface::AppShell => (format!("?ui=app&session={sid}"), "document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3 && document.querySelector('.transcript').scrollHeight > document.querySelector('.transcript').clientHeight * 3", "document.querySelector('.virtual-window') ? document.querySelector('.virtual-window').children.length : 'no window'"),
    };
    monitor.open(&tab, &query);
    harness::until(
        &tab,
        ready,
        "the page to render the audit corpus",
        Duration::from_secs(30),
        diag,
    );
    Opened {
        tab,
        _browser: browser,
        _monitor: monitor,
    }
}

fn settle() {
    std::thread::sleep(Duration::from_millis(700));
}

/// The measuring instrument, installed in the page: `capture` / `arm` / `report`.
///
/// The diff is computed IN THE PAGE and only the ANSWER crosses the wire. The naive shape —
/// ship every element's every property to Rust, twice, per control, per surface — moves
/// hundreds of elements times ~340 longhands each way; the effect set is small even when the
/// snapshot is not.
const AUDIT_JS: &str = r##"(function () {
  // ── the snapshot ──────────────────────────────────────────────────────────────────────
  // Every longhand the engine resolved, for every element under a record root. Iterating the
  // CSSStyleDeclaration by index is the whole point: a chosen property list is the same
  // memory-based omission this audit exists to remove, and it is exactly how #161 and #173 hid.
  function capture(sel, idAttr) {
    var snap = new Map();
    var owner = {};
    [].slice.call(document.querySelectorAll(sel)).forEach(function (root) {
      var id = idAttr === "id" ? root.id : root.getAttribute(idAttr);
      if (!id) return;
      // Which record CONTAINS this one. A record nested inside another (a Thinking that absorbed
      // its tool calls, a sub-agent's children) is inside its parent's body, so a control that
      // collapses the parent legitimately reaches it — that is containment, not leakage, and
      // only the tree can tell the two apart.
      var up = root.parentElement ? root.parentElement.closest(sel) : null;
      owner[id] = up ? (idAttr === "id" ? up.id : up.getAttribute(idAttr)) : null;
      (function walk(el, path) {
        var cs = getComputedStyle(el), props = {};
        for (var i = 0; i < cs.length; i++) {
          var p = cs[i];
          // A custom property is an INPUT to rendering, not a resolved effect, and it inherits:
          // moving --code-size on the app root would otherwise report every element on the page
          // as changed and drown the effect set it is supposed to reveal.
          if (p.slice(0, 2) === "--") continue;
          props[p] = cs.getPropertyValue(p);
        }
        var fam = (props["font-family"] || "").trim();
        snap.set(id + "/" + path, {
          props: props,
          tag: el.tagName,
          // For the FAILURE MESSAGE only — a class name never enters a predicate, but a key
          // like b24/.0.2 is unactionable without knowing what element it names.
          cls: (typeof el.className === "string" ? el.className : "").split(/\s+/).slice(0, 3).join("."),
          code: !!el.closest("[data-code]"),
          pre: el.tagName === "PRE",
          mono: /mono/i.test(fam)
        });
        var kids = el.children;
        for (var j = 0; j < kids.length; j++) {
          // Stop at a NESTED record root — it is keyed under its own id, so every element is
          // counted once and the key means the same thing on both pages. The index is the TRUE
          // child position, so skipping a nested root does not shift its siblings' keys.
          var child = kids[j];
          var childId = idAttr === "id" ? child.id : child.getAttribute(idAttr);
          if (childId && child.matches(sel)) continue;
          walk(child, path + "." + j);
        }
      })(root, "");
    });
    snap.owner = owner;
    return snap;
  }
  // ── the partition ─────────────────────────────────────────────────────────────────────
  // Derived from STRUCTURE and from the measured font-family, never from a class name. The
  // groups partition the DOM, so "and nothing else moved" is other.changed === 0 — a claim
  // about the complement, which is the half a selector-based check can never make.
  function groupOf(e) {
    if (e.code) return e.mono ? "code-mono" : "code-other";
    if (e.pre) return "pre";
    return e.mono ? "mono" : "other";
  }
  var GROUPS = ["code-mono", "code-other", "pre", "mono", "other"];
  window.__audit = {
    snap: null, sel: null, idAttr: null,
    arm: function (sel, idAttr) {
      this.sel = sel; this.idAttr = idAttr;
      this.snap = capture(sel, idAttr);
      var census = {};
      GROUPS.forEach(function (g) { census[g] = 0; });
      this.snap.forEach(function (e) { census[groupOf(e)]++; });
      return { elements: this.snap.size, census: census };
    },
    // The effect set as CELLS — `<record id>|<group>` — which is the only shape that compares
    // across the two pages (P3). Neither the class names nor the child-index paths agree between
    // them and they are not meant to; what does agree is the record (one transcript, one
    // emitter, one set of ids) and the group, which is derived from structure and from the
    // measured font, never from a class. A cell present on one page and absent on the other is
    // a rendering difference; a cell present on both where only one page's control reaches it
    // is an equivalence GAP, and that is what P3 exists to find.
    reportCells: function (prop) {
      var before = this.snap, after = capture(this.sel, this.idAttr);
      var present = {}, changed = {};
      before.forEach(function (a, key) {
        var id = key.slice(0, key.indexOf("/"));
        var cell = id + "|" + groupOf(a);
        present[cell] = (present[cell] || 0) + 1;
        var b = after.get(key);
        if (b && a.props[prop] !== b.props[prop]) changed[cell] = (changed[cell] || 0) + 1;
      });
      return { present: present, changed: changed };
    },
    // The same effect set, partitioned by RECORD instead of by kind of content. A control that
    // sits ON a record (a fold head, a pane's own bar, a turn's raw toggle) claims to act on
    // THAT record and no other — #173 is what happens when it does not — and the element key is
    // already <record id>/<path>, so the partition costs nothing and needs no selector.
    reportRecords: function () {
      var before = this.snap, after = capture(this.sel, this.idAttr);
      var records = {}, gone = 0, added = 0;
      after.forEach(function (_, key) { if (!before.has(key)) added++; });
      before.forEach(function (a, key) {
        var id = key.slice(0, key.indexOf("/"));
        var r = records[id] || (records[id] = { n: 0, changed: 0, props: {}, changedSample: [] });
        r.n++;
        var b = after.get(key);
        if (!b) { gone++; r.changed++; return; }
        var moved = [];
        for (var p in a.props) if (a.props[p] !== b.props[p]) moved.push(p + ":" + a.props[p] + "->" + b.props[p]);
        if (!moved.length) return;
        r.changed++;
        if (r.changedSample.length < 20) r.changedSample.push(key + " <" + b.tag.toLowerCase() + "." + (b.cls || "") + "> [" + moved.slice(0, 8).join(" | ") + "]");
        moved.forEach(function (p) { var name = p.slice(0, p.indexOf(":")); r.props[name] = (r.props[name] || 0) + 1; });
      });
      return { gone: gone, added: added, records: records, owner: before.owner };
    },
    report: function () {
      var before = this.snap, after = capture(this.sel, this.idAttr);
      var props = {}, groups = {};
      GROUPS.forEach(function (g) { groups[g] = { n: 0, changed: 0, props: {}, changedSample: [], unchangedSample: [] }; });
      var gone = [], added = [], changedTotal = 0;
      after.forEach(function (_, key) { if (!before.has(key)) added.push(key); });
      before.forEach(function (a, key) {
        var b = after.get(key);
        if (!b) { gone.push(key); return; }
        var g = groups[groupOf(a)];
        g.n++;
        var moved = [];
        for (var p in a.props) if (a.props[p] !== b.props[p]) moved.push(p);
        for (var q in b.props) if (!(q in a.props)) moved.push(q);
        if (!moved.length) { if (g.unchangedSample.length < 8) g.unchangedSample.push(key); return; }
        changedTotal++;
        g.changed++;
        if (g.changedSample.length < 8) g.changedSample.push(key + " <" + b.tag.toLowerCase() + "." + (b.cls || "") + "> [" + moved.slice(0, 8).join(",") + "]");
        moved.forEach(function (p) { props[p] = (props[p] || 0) + 1; g.props[p] = (g.props[p] || 0) + 1; });
      });
      return { elements: before.size, matched: before.size - gone.length,
               gone: gone.length, added: added.length,
               goneSample: gone.slice(0, 6), addedSample: added.slice(0, 6),
               changedTotal: changedTotal, props: props, groups: groups };
    }
  };
  return "armed";
})()"##;

/// Where each page stamps a record, and how the id is read off it. The ONLY per-surface
/// knowledge in this file: everything the audit measures is derived from the DOM these roots
/// contain, never from a list of the things inside them.
fn roots(surface: Surface) -> (&'static str, &'static str) {
    match surface {
        Surface::Classic => ("#stream .blk", "id"),
        Surface::AppShell => (".virtual-window [data-record-id]", "data-record-id"),
    }
}

/// Mount the WHOLE corpus: a rendering that is folded away cannot be measured, and a cell the
/// instrument never sees is exactly the omission this audit exists to remove.
///
/// ONE CLICK AT A TIME, re-querying between them. The obvious
/// `querySelectorAll(...).forEach(click)` is wrong on the app shell and silently so: the first
/// click calls `actions.rerender()`, which REPLACES every node in the window, so the rest of the
/// NodeList is detached and their clicks reach nothing. Measured before this loop existed —
/// fourteen renderers stayed folded through four rounds, and the size and wrap predicates had
/// been quantifying over a DOM with fourteen renderings hidden inside it. The coverage assertion
/// below is what caught it, which is the whole argument for having one.
///
/// A head's click is also a FOUR-STEP cycle (#129) on both pages — folded → output → output and
/// the whole command → output → folded — so only a CLOSED head is ever clicked. `|| !f.open` is
/// the tempting guard on the classic page and it is always true there, because a `.fold` is a
/// div and not a `<details>`: it cycles every head instead of opening it, which shows up as
/// `.tool-target` and `.anim` moving under a control that never touched them.
fn open_everything(tab: &headless_chrome::Tab, surface: Surface) {
    let (fold, more) = match surface {
        Surface::Classic => (
            "#stream .fold[data-open=\"0\"] > .fold-h",
            "#stream .morebtn",
        ),
        Surface::AppShell => (
            ".virtual-window .renderer.closed > button.renderer-head",
            ".virtual-window [data-cap-more], .virtual-window [data-process-more][aria-expanded=\"false\"]",
        ),
    };
    for _ in 0..4 {
        let opened = probe(
            tab,
            &format!(
                "(function(){{ var n = 0, m = 0, e; \
                 for (var i = 0; i < 400 && (e = document.querySelector('{fold}')); i++) {{ e.click(); n++; }} \
                 for (var j = 0; j < 400 && (e = document.querySelector('{more}')); j++) {{ e.click(); m++; }} \
                 return {{ folds: n, more: m }}; }})()"
            ),
        );
        settle();
        if opened["folds"].as_i64() == Some(0) && opened["more"].as_i64() == Some(0) {
            break;
        }
    }
}

/// Arm the instrument over everything currently mounted — and prove the page is QUIET first.
///
/// A page that is still settling moves properties on its own: an open animation finishing, a
/// re-measure landing, a font swapping in. The instrument cannot tell that from a control's
/// effect, and the confusion is not theoretical — one run of the fold case reported a
/// cross-record change that four later runs could not reproduce, in the run immediately after
/// the release binaries were replaced. So arming is a LOOP: snapshot, wait, and take the effect
/// set of doing nothing at all. Only when that is empty is the page a fair baseline.
fn arm(tab: &headless_chrome::Tab, surface: Surface) -> serde_json::Value {
    let (sel, id_attr) = roots(surface);
    eval(tab, AUDIT_JS);
    let mut census = probe(tab, &format!("window.__audit.arm('{sel}', '{id_attr}')"));
    for _ in 0..8 {
        settle();
        let idle = report(tab);
        if idle["changedTotal"].as_i64() == Some(0)
            && idle["gone"].as_i64() == Some(0)
            && idle["added"].as_i64() == Some(0)
        {
            return census;
        }
        census = probe(tab, &format!("window.__audit.arm('{sel}', '{id_attr}')"));
    }
    panic!(
        "{surface:?}: the page never went quiet — doing NOTHING kept changing computed style, so \
         nothing measured after this point could be attributed to a control. Last idle set: {}",
        report(tab)
    )
}

fn report(tab: &headless_chrome::Tab) -> serde_json::Value {
    probe(tab, "window.__audit.report()")
}

/// Press a page-wide reading control the way a reader does — the shared keymap (`shared/keymap.js`)
/// gives both pages the SAME keys, so one invocation drives both surfaces and no per-surface
/// control vocabulary enters the audit.
fn press(tab: &headless_chrome::Tab, key: &str) {
    eval(
        tab,
        &format!("(function(){{ document.body.focus(); document.dispatchEvent(new KeyboardEvent('keydown', {{ key: '{key}', bubbles: true, cancelable: true }})); return 'pressed'; }})()"),
    );
    settle();
}

/// Which record kinds a page has actually MOUNTED, and whether anything is still folded away.
/// Both pages stamp the kind on every top-level record they materialize — the classic page in
/// `matBlock` (`e.dataset.kind = b.kind`), the app shell on the turn wrapper
/// (`data-record-kind`) — so this reads each page's own stamp rather than a vocabulary of mine.
fn mounted(tab: &headless_chrome::Tab, surface: Surface) -> serde_json::Value {
    probe(
        tab,
        match surface {
            Surface::Classic => "(function(){ return { kinds: [...new Set([...document.querySelectorAll('#stream [data-kind]')].map(function (e) { return e.dataset.kind; }))].sort(), closed: document.querySelectorAll('#stream .fold[data-open=\"0\"]').length, more: document.querySelectorAll('#stream .morebtn').length }; })()",
            Surface::AppShell => "(function(){ return { kinds: [...new Set([...document.querySelectorAll('.virtual-window [data-record-kind]')].map(function (e) { return e.dataset.recordKind; }))].sort(), closed: document.querySelectorAll('.virtual-window .renderer.closed').length, closedSample: [...document.querySelectorAll('.virtual-window .renderer.closed')].slice(0, 10).map(function (r) { var h = r.querySelector(':scope > .renderer-head'); return (r.dataset.recordId || '?') + ' ' + (r.dataset.rendererKind || '?') + ' head=' + (h ? h.tagName : 'none') + ' body=' + (r.querySelector(':scope > .renderer-body') ? 'yes' : 'no'); }), more: document.querySelectorAll('.virtual-window [data-cap-more]').length }; })()",
        },
    )
}

/// ── STAGE B, first light ────────────────────────────────────────────────────────────────────
/// Not an assertion about a control yet: an assertion that the INSTRUMENT sees the corpus. If
/// this is wrong every predicate built on it is vacuous, and a vacuous audit is indistinguishable
/// from a clean one — which is how every previous pass at this task ended.
///
/// So the coverage claim is DERIVED, not counted. `audit_kinds(audit_records(audit_corpus()))`
/// is the closed set P1 built out of `BlockKind`'s own variants; this asserts every one of them
/// is mounted, and that nothing is left folded away for the predicates to miss. Adding a
/// `BlockKind` variant stops P1's build; a variant that renders but never reaches the DOM stops
/// this. Neither depends on anyone remembering to extend a list.
fn scenario_the_instrument_sees_the_whole_corpus(tab: &headless_chrome::Tab, surface: Surface) {
    jump_to_end(tab, surface);
    settle();
    open_everything(tab, surface);
    settle();
    settle();

    // NESTED records are the one honest gap, and it is the same on both pages: a `blocks` part
    // holds records inside another record (a Thinking that absorbed its tool calls), and neither
    // page stamps the kind on those. They are still in the DOM and still measured — the
    // instrument walks into them — so this is a limit of the CENSUS, not of the audit, and it
    // is asserted below rather than described, so it cannot quietly widen.
    let expected: std::collections::BTreeSet<String> = audit_kinds(&audit_records(&audit_corpus()))
        .into_iter()
        .collect();
    let seen = mounted(tab, surface);
    let got: std::collections::BTreeSet<String> = seen["kinds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| k.as_str().map(str::to_string))
        .collect();
    let missing: Vec<&String> = expected.difference(&got).collect();
    let extra: Vec<&String> = got.difference(&expected).collect();
    assert!(
        expected.len() >= 15,
        "the derived vocabulary is the whole of BlockKind, not a sample: {expected:?}"
    );
    assert_eq!(
        missing.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        NESTED_ONLY,
        "{surface:?}: every kind the corpus declares is MOUNTED, except the ones that only ever \
         appear nested inside another record (neither page stamps a kind on those). Missing: \
         {missing:?}; mounted: {seen}"
    );
    assert!(
        extra.is_empty(),
        "{surface:?}: …and the page mounted a kind the corpus does not declare, which means the \
         audit is measuring something P1 never derived: {extra:?}"
    );
    assert_eq!(
        seen["closed"].as_i64(),
        Some(0),
        "{surface:?}: nothing is left folded away — a rendering the instrument cannot see is \
         outside every predicate, and a predicate that passes over an empty domain is \
         indistinguishable from a clean audit: {seen}"
    );

    let census = arm(tab, surface)["census"].clone();
    assert!(
        census["code-mono"].as_i64().unwrap_or(0) > 0,
        "{surface:?}: the corpus mounts code panes — without them the size predicate is vacuous: {census}"
    );
    assert!(
        census["pre"].as_i64().unwrap_or(0) > 0,
        "{surface:?}: …and verbatim text outside a code pane, which is what wrap reaches beyond code: {census}"
    );
    assert!(
        census["other"].as_i64().unwrap_or(0) > 50,
        "{surface:?}: …and a large complement, which is what carries the 'and nothing else' half: {census}"
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_the_instrument_sees_the_whole_corpus() {
    let _serial = serial();
    let fx = fixture_audit("audit-instrument-classic");
    let page = open(Surface::Classic, &fx, 2951);
    scenario_the_instrument_sees_the_whole_corpus(&page.tab, Surface::Classic);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_instrument_sees_the_whole_corpus() {
    let _serial = serial();
    let fx = fixture_audit("audit-instrument-app");
    let page = open(Surface::AppShell, &fx, 2952);
    scenario_the_instrument_sees_the_whole_corpus(&page.tab, Surface::AppShell);
}

/// ── STAGE B, the two reading preferences ───────────────────────────────────────────────────
///
/// A control's claim is "control C governs property P over domain D", and P2 checks it BOTH
/// WAYS over the DOM the derived corpus produced:
///
///   * every element in D changed P, and
///   * no element outside D changed P.
///
/// Quantifying over the PROPERTY is what keeps a selector out of the claim. It also keeps the
/// claim readable in the presence of two kinds of movement that are not reach:
///
///   * GEOMETRY — `height`, `block-size`, `transform-origin` and their kin are USED values that
///     layout resolves, so every ancestor of an element that changed size reports them. Measured
///     here: pressing `−` moved `height` on 14 classic-page elements outside any code pane, all
///     of them ancestors of a pane that had just got shorter. A consequence, never a reach.
///   * AFFORDANCE — a control that mirrors its own state recolours its own chrome. Measured: the
///     classic page's four `.ms-wrap` buttons change seventeen colour properties on `w`.
///
/// Neither can touch `font-size` or the wrap property, so the predicate is exact without
/// excluding anything. The complement check below is the weaker, second claim — outside D
/// nothing moved EXCEPT geometry and colour — and it is where those two categories are named,
/// each with its reason, so a control that started moving something else fails loudly.
fn geometry_or_affordance(prop: &str) -> bool {
    // Used values resolved by layout, not set by the cascade.
    matches!(prop, "block-size" | "inline-size" | "height" | "width" | "perspective-origin" | "transform-origin")
        // A control showing its own state. A suffix, not a list of properties.
        || prop == "color"
        || prop.ends_with("-color")
}

fn group_n(report: &serde_json::Value, group: &str) -> i64 {
    report["groups"][group]["n"].as_i64().unwrap_or(0)
}
fn group_prop(report: &serde_json::Value, group: &str, prop: &str) -> i64 {
    report["groups"][group]["props"][prop].as_i64().unwrap_or(0)
}

/// The engine's name for the wrap longhand — `white-space` was split, and which half a build
/// reports is Chrome's business, not this audit's. Taken from the measurement rather than
/// assumed, so the case does not silently check a property nothing reports.
fn wrap_prop(report: &serde_json::Value) -> &'static str {
    if report["props"]["text-wrap-mode"].as_i64().unwrap_or(0) > 0 {
        "text-wrap-mode"
    } else {
        "white-space"
    }
}

/// The two-sided check, with the failure carrying the whole effect set: a bare count tells the
/// next reader nothing about WHICH element escaped the claim.
fn assert_governs(
    report: &serde_json::Value,
    surface: Surface,
    control: &str,
    prop: &str,
    domain: &[&str],
    outside: &[&str],
) {
    // The whole effect set, for re-taking the table without editing the case: it is the evidence
    // behind every assertion below and `--nocapture` is where a reader gets at it.
    println!("EFFECT-SET {surface:?} | {control} | governs {prop} | {report}");
    let total = report["props"][prop].as_i64().unwrap_or(0);
    let inside: i64 = domain.iter().map(|g| group_n(report, g)).sum();
    assert!(
        inside > 0,
        "{surface:?}: {control} has a domain to govern at all — an empty domain makes the whole \
         predicate vacuous, which is indistinguishable from a clean audit: {report}"
    );
    for group in domain {
        assert_eq!(
            group_prop(report, group, prop),
            group_n(report, group),
            "{surface:?}: {control} reaches EVERY element of `{group}` ({prop}) — this is the half \
             #173 failed, where `.codebox .lines` matched no reading rule and the pane the reader \
             was looking at ignored its own control: {report}"
        );
    }
    for group in outside {
        assert_eq!(
            group_prop(report, group, prop),
            0,
            "{surface:?}: {control} reaches NOTHING in `{group}` ({prop}) — a control that governs \
             more than it claims is the same defect seen from the other side: {report}"
        );
    }
    assert_eq!(
        total, inside,
        "{surface:?}: …and the totals agree, so no element escaped the partition: {report}"
    );
    // The weaker second claim: outside the domain, nothing moved that is not a layout consequence
    // or a control showing its own state.
    for group in outside {
        for (moved, _) in report["groups"][group]["props"]
            .as_object()
            .into_iter()
            .flatten()
        {
            assert!(
                geometry_or_affordance(moved),
                "{surface:?}: {control} moved `{moved}` on `{group}`, which is neither a layout \
                 consequence nor a control recolouring itself — it is reach nobody declared: \
                 {report}"
            );
        }
    }
    assert_eq!(
        report["gone"].as_i64(),
        Some(0),
        "{surface:?}: {control} unmounted nothing the reader was looking at: {report}"
    );
}

/// CLAIM. Pressing `−` changes the computed `font-size` of every element inside a `[data-code]`
/// container and of no element outside one.
///
/// `[data-code]` is not a class list: it is the emitter's PROVENANCE stamp — the owner's rule for
/// what a code surface is, "did these bytes come from a file on disk", recorded in #173 — carried
/// by `numbered_part` and `diff_part` and by nothing else. So a rendering added later is inside
/// the quantifier by construction, which is the only property that makes this an audit rather
/// than another list.
fn scenario_size_governs_exactly_the_code(tab: &headless_chrome::Tab, surface: Surface) {
    jump_to_end(tab, surface);
    settle();
    open_everything(tab, surface);
    settle();
    settle();
    arm(tab, surface);
    press(tab, "-");
    let report = report(tab);
    assert_governs(
        &report,
        surface,
        "the code-size control (−)",
        "font-size",
        &["code-mono", "code-other"],
        &["mono", "pre", "other"],
    );
}

/// CLAIM. Pressing `w` changes the computed wrap property of every `<pre>` in the stream AND
/// every element inside a `[data-code]` container, and of no other element.
///
/// Wrap is deliberately WIDER than code (#173): pasted art in a user turn is verbatim text a
/// reader wants wrapped even though it never came from a file, and #161 is what happens when the
/// domain is written as a selector list instead — bash output was simply not in it.
fn scenario_wrap_governs_exactly_the_verbatim_text(tab: &headless_chrome::Tab, surface: Surface) {
    jump_to_end(tab, surface);
    settle();
    open_everything(tab, surface);
    settle();
    settle();
    arm(tab, surface);
    press(tab, "w");
    let report = report(tab);
    let prop = wrap_prop(&report);
    assert_governs(
        &report,
        surface,
        "the wrap control (w)",
        prop,
        &["code-mono", "code-other", "pre"],
        &["mono", "other"],
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_size_governs_exactly_the_code() {
    let _serial = serial();
    let fx = fixture_audit("audit-size-classic");
    let page = open(Surface::Classic, &fx, 2953);
    scenario_size_governs_exactly_the_code(&page.tab, Surface::Classic);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_size_governs_exactly_the_code() {
    let _serial = serial();
    let fx = fixture_audit("audit-size-app");
    let page = open(Surface::AppShell, &fx, 2954);
    scenario_size_governs_exactly_the_code(&page.tab, Surface::AppShell);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_wrap_governs_exactly_the_verbatim_text() {
    let _serial = serial();
    let fx = fixture_audit("audit-wrap-classic");
    let page = open(Surface::Classic, &fx, 2955);
    scenario_wrap_governs_exactly_the_verbatim_text(&page.tab, Surface::Classic);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_wrap_governs_exactly_the_verbatim_text() {
    let _serial = serial();
    let fx = fixture_audit("audit-wrap-app");
    let page = open(Surface::AppShell, &fx, 2956);
    scenario_wrap_governs_exactly_the_verbatim_text(&page.tab, Surface::AppShell);
}

/// ── STAGE B, a control that sits ON a record ────────────────────────────────────────────────
///
/// CLAIM. Folding one record changes the rendering of that record and of NO other record.
///
/// This is the scope half of #173 stated generally: "a control on a pane is not a page-wide
/// control", where the classic page's bar wrote the page-wide preference and resized every pane
/// on the page. The instrument needs nothing new for it — the element key is already
/// `<record id>/<path>`, so the partition is by prefix, and the claim is checkable without any
/// notion of what a fold looks like on either page.
///
/// It also exercises the half of the key scheme Stage C depends on: a fold RE-RENDERS on the app
/// shell (`actions.rerender()` replaces every node in the window), so a key that survives it is
/// the thing that makes two surfaces comparable at all. A key held by node reference would read
/// a detached element here — the same mistake #180 made holding an anchor by node and reading a
/// constant −900.
fn scenario_a_fold_acts_on_its_own_record(tab: &headless_chrome::Tab, surface: Surface) {
    jump_to_end(tab, surface);
    settle();
    open_everything(tab, surface);
    settle();
    settle();
    let (sel, id_attr) = roots(surface);
    eval(tab, AUDIT_JS);
    probe(tab, &format!("window.__audit.arm('{sel}', '{id_attr}')"));
    // Fold ONE record — whichever the page offers a head for — and take its id from the DOM
    // rather than naming one, so the case does not depend on the corpus's ordering.
    let folded = probe(
        tab,
        match surface {
            Surface::Classic => "(function(){ var f = document.querySelector('#stream .fold[data-open=\"1\"]'); if (!f) return null; var h = f.querySelector(':scope > .fold-h'); if (!h) return null; h.click(); return f.id; })()",
            Surface::AppShell => "(function(){ var r = document.querySelector('.virtual-window .renderer[data-record-id]:not(.closed):not(.noninteractive)'); if (!r) return null; var h = r.querySelector(':scope > button.renderer-head'); if (!h) return null; var id = r.dataset.recordId; h.click(); return id; })()",
        },
    );
    let target = folded.as_str().unwrap_or_default().to_string();
    assert!(
        !target.is_empty(),
        "{surface:?}: the corpus offers a record with a fold head to act on"
    );
    settle();
    let report = probe(tab, "window.__audit.reportRecords()");
    let records = report["records"].as_object().cloned().unwrap_or_default();
    assert!(
        records.len() > 5,
        "{surface:?}: more than one record is under measurement, or the claim is vacuous: {report}"
    );
    // The claim's domain is the target AND WHAT IT CONTAINS. A record nested inside another —
    // a Thinking that absorbed its tool calls, a sub-agent's children — is inside its parent's
    // body, so collapsing the parent legitimately hides it. Measured, not assumed: the first run
    // of this case failed on exactly that, and the values said so plainly
    // (`display: block -> none`, and every used value falling back to `auto`, which is what
    // `getComputedStyle` reports for an element that is no longer rendered).
    let owner = report["owner"].as_object().cloned().unwrap_or_default();
    let contained = |mut id: String| -> bool {
        for _ in 0..16 {
            if id == target {
                return true;
            }
            match owner.get(&id).and_then(|v| v.as_str()) {
                Some(up) => id = up.to_string(),
                None => return false,
            }
        }
        false
    };
    let leaked: Vec<&String> = records
        .iter()
        .filter(|(id, r)| r["changed"].as_i64().unwrap_or(0) > 0 && !contained((*id).clone()))
        .map(|(id, _)| id)
        .collect();
    assert!(
        leaked.is_empty(),
        "{surface:?}: folding `{target}` reached {leaked:?}, which it does not contain — a control \
         that sits on a record and moves another is #173 seen from the outside: {report}"
    );
    assert!(
        records[&target]["changed"].as_i64().unwrap_or(0) > 0,
        "{surface:?}: …and it did change something, so the claim is not vacuous: {report}"
    );
    // …and the domain is a small part of the whole. If `contained` ever returned true for
    // everything the leak check above would pass over an empty complement, which is the failure
    // mode this whole task exists to remove.
    let inside = records.keys().filter(|id| contained((*id).clone())).count();
    assert!(
        inside * 4 < records.len(),
        "{surface:?}: the target and what it contains are a small part of the records under \
         measurement ({inside} of {}) — otherwise the leak check has nothing to be false about: \
         {report}",
        records.len()
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_a_fold_acts_on_its_own_record() {
    let _serial = serial();
    let fx = fixture_audit("audit-fold-classic");
    let page = open(Surface::Classic, &fx, 2957);
    scenario_a_fold_acts_on_its_own_record(&page.tab, Surface::Classic);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_fold_acts_on_its_own_record() {
    let _serial = serial();
    let fx = fixture_audit("audit-fold-app");
    let page = open(Surface::AppShell, &fx, 2958);
    scenario_a_fold_acts_on_its_own_record(&page.tab, Surface::AppShell);
}

/// ── STAGE C — P3: EQUIVALENCE IS A SET DIFFERENCE ───────────────────────────────────────────
///
/// The same corpus, the same control, both pages, compared. This is the property the owner
/// actually asked for — "we have done the exercise multiple times … and each time it fails at
/// the goal of discovering all equivalence gaps" — and the reason every previous pass could not
/// deliver it is that the comparison was always a person reading two renderings side by side.
///
/// The comparable unit is the CELL, `<record id>|<group>`. Neither page's class names nor its
/// child-index paths agree with the other's, and they are not meant to. What does agree is the
/// record — one transcript, one emitter, one set of ids — and the group, which is derived from
/// the DOM's structure and from the measured font rather than from any class. So:
///
///   * a cell present on one page and absent on the other is a RENDERING difference — one page
///     draws something there and the other does not;
///   * a cell present on BOTH where only one page's control reaches it is an EQUIVALENCE GAP,
///     and it is exactly the shape of #161 and #173.
///
/// The allowlist is the one place a human judgement enters, which is where it belongs: visible,
/// small, and reviewed. It is empty today for these two controls, and that is a measurement, not
/// an aspiration.
struct Cells {
    present: std::collections::BTreeMap<String, i64>,
    changed: std::collections::BTreeMap<String, i64>,
}

fn cells(tab: &headless_chrome::Tab, surface: Surface, key: &str, prop: &str) -> Cells {
    jump_to_end(tab, surface);
    settle();
    open_everything(tab, surface);
    settle();
    settle();
    arm(tab, surface);
    press(tab, key);
    let got = probe(tab, &format!("window.__audit.reportCells('{prop}')"));
    let map = |field: &str| -> std::collections::BTreeMap<String, i64> {
        got[field]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0)))
            .collect()
    };
    Cells {
        present: map("present"),
        changed: map("changed"),
    }
}

/// Deliberate, RECORDED differences between the two pages. Each entry is a cell and the reason
/// it is allowed to differ; anything not here is a finding. Empty for these controls today.
const ALLOWED_DIFFERENCES: [(&str, &str); 0] = [];

fn compare_surfaces(classic: &Cells, shell: &Cells, control: &str, prop: &str) {
    let allowed: std::collections::BTreeSet<&str> =
        ALLOWED_DIFFERENCES.iter().map(|(cell, _)| *cell).collect();
    // A cell BOTH pages draw, where only one page's control reaches it. This is the gap.
    let mut gaps: Vec<String> = Vec::new();
    for cell in classic.present.keys() {
        if !shell.present.contains_key(cell) || allowed.contains(cell.as_str()) {
            continue;
        }
        let on_classic = classic.changed.contains_key(cell);
        let on_shell = shell.changed.contains_key(cell);
        if on_classic != on_shell {
            gaps.push(format!(
                "{cell} (classic {}, shell {})",
                if on_classic { "reached" } else { "MISSED" },
                if on_shell { "reached" } else { "MISSED" }
            ));
        }
    }
    // Non-vacuity: the comparison has to be over a real shared population, or "no gaps" means
    // "nothing was compared" — which is how an audit passes while missing everything.
    let shared = classic
        .present
        .keys()
        .filter(|cell| shell.present.contains_key(*cell))
        .count();
    assert!(
        shared > 20,
        "{control}: the two pages share a population to compare ({shared} cells) — otherwise the \
         set difference is empty because nothing was in it: classic {} cells, shell {} cells",
        classic.present.len(),
        shell.present.len()
    );
    // …and the comparison covers the WHOLE of the smaller population. Measured: every cell the
    // app shell draws is also drawn by the classic page, so "no gaps" is a statement about all
    // of the shell's renderings and not about an overlap that happens to be clean. The reverse
    // is not true and does not need to be — the classic page mounts more of the PADDING above
    // the corpus, which neither control touches; if the shell ever draws a cell the classic page
    // does not, that is a rendering difference and belongs in front of a person.
    let shell_only: Vec<&String> = shell
        .present
        .keys()
        .filter(|cell| !classic.present.contains_key(*cell))
        .collect();
    assert!(
        shell_only.is_empty(),
        "{control}: the app shell draws {} cell(s) the classic page does not, so the comparison \
         no longer covers everything the shell renders: {shell_only:?}",
        shell_only.len()
    );
    assert!(
        !classic.changed.is_empty() && !shell.changed.is_empty(),
        "{control}: …and the control moved `{prop}` on BOTH pages, so a clean difference is not \
         two pages doing nothing: classic {} changed, shell {} changed",
        classic.changed.len(),
        shell.changed.len()
    );
    assert!(
        gaps.is_empty(),
        "{control}: the two pages disagree about `{prop}` on {} cell(s) they BOTH draw. A cell is \
         `<record id>|<group>`; `MISSED` is the page whose control did not reach a rendering the \
         other page's did. This is the shape of #161 and #173.\n  {}",
        gaps.len(),
        gaps.join("\n  ")
    );
}

/// The rendering differences the comparison turns up on the way — cells one page draws and the
/// other does not. Not a failure: the pages legitimately build different trees. Printed so the
/// set is visible and can be watched, rather than discovered again by the owner.
fn report_rendering_differences(classic: &Cells, shell: &Cells, control: &str) {
    let only_classic: Vec<&String> = classic
        .present
        .keys()
        .filter(|c| !shell.present.contains_key(*c))
        .collect();
    let only_shell: Vec<&String> = shell
        .present
        .keys()
        .filter(|c| !classic.present.contains_key(*c))
        .collect();
    println!(
        "RENDERING-DIFF {control}: classic-only {:?} | shell-only {:?}",
        only_classic, only_shell
    );
}

fn both_surfaces(fx: &Fixture, port: u16) -> (Opened, Opened) {
    // TWO tabs, but the SAME monitor and the same session: a comparison between two servers or
    // two fixtures would be a comparison of two coincidences.
    let classic = open(Surface::Classic, fx, port);
    let shell = open(Surface::AppShell, fx, port + 1);
    (classic, shell)
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_two_pages_agree_about_what_the_size_control_reaches() {
    let _serial = serial();
    let fx = fixture_audit("audit-p3-size");
    let (classic, shell) = both_surfaces(&fx, 2959);
    let a = cells(&classic.tab, Surface::Classic, "-", "font-size");
    let b = cells(&shell.tab, Surface::AppShell, "-", "font-size");
    report_rendering_differences(&a, &b, "size");
    compare_surfaces(&a, &b, "the code-size control (-)", "font-size");
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_two_pages_agree_about_what_the_wrap_control_reaches() {
    let _serial = serial();
    let fx = fixture_audit("audit-p3-wrap");
    let (classic, shell) = both_surfaces(&fx, 2961);
    let a = cells(&classic.tab, Surface::Classic, "w", "text-wrap-mode");
    let b = cells(&shell.tab, Surface::AppShell, "w", "text-wrap-mode");
    report_rendering_differences(&a, &b, "wrap");
    compare_surfaces(&a, &b, "the wrap control (w)", "text-wrap-mode");
}

/// ── STAGE D — P4: A CONTROL THAT CHANGES NOTHING MUST NOT MOVE THE READER ───────────────────
///
/// CLAIM. A control press whose effect set is EMPTY leaves the reader's scroll position exactly
/// where it was.
///
/// This is #173's other half, and the audit is where it belongs rather than in a one-off case:
/// `applyReading` used to end in an unconditional `viewport.remeasure()`, so `−` held down at
/// the size floor, `Reset` on an already-default panel, or a second click on the same switch
/// each cleared the height cache and rewrote the reader's position from estimates. The fix was
/// `if (layoutChanged)`, and what makes it an AUDIT rather than a regression test is that the
/// premise — "this press changed nothing" — is MEASURED by the same instrument, not asserted.
///
/// The reader is parked MID-DOCUMENT on purpose. At the tail the page converges to the bottom on
/// its own, so a position that was rewritten and a position that was kept look identical — the
/// lesson #185's fixture had to learn from the other direction.
fn scroll_top(tab: &headless_chrome::Tab, surface: Surface) -> f64 {
    let s = surface.scroller();
    probe(
        tab,
        &format!("(function(){{ var s = {s}; return s ? s.scrollTop : -1; }})()"),
    )
    .as_f64()
    .unwrap_or(-1.0)
}

fn scenario_a_press_that_changes_nothing_does_not_move_the_reader(
    tab: &headless_chrome::Tab,
    surface: Surface,
) {
    jump_to_end(tab, surface);
    settle();
    open_everything(tab, surface);
    settle();
    settle();
    // Walk the code size down to its FLOOR, which is where a press stops changing anything.
    // Bounded, and the premise is CHECKED below rather than assumed: if the floor were never
    // reached, the press would change something and the case would say so.
    for _ in 0..14 {
        press(tab, "-");
    }
    settle();
    // Park mid-document, then let the page settle so nothing is still in flight.
    harness::scroll_by(tab, surface, -2500);
    settle();
    settle();
    arm(tab, surface);
    let before = scroll_top(tab, surface);
    assert!(
        before > 50.0,
        "{surface:?}: the reader is parked away from the top, where a rewritten position is \
         visible at all: scrollTop {before}"
    );
    press(tab, "-");
    let report = report(tab);
    let after = scroll_top(tab, surface);
    // The premise, measured: this press really did change nothing.
    assert_eq!(
        report["changedTotal"].as_i64(),
        Some(0),
        "{surface:?}: the press at the floor changed nothing — if it did, this case is testing a \
         different claim than the one it states: {report}"
    );
    assert_eq!(
        report["gone"].as_i64(),
        Some(0),
        "{surface:?}: …and unmounted nothing: {report}"
    );
    // …and therefore the reader did not move. An exact equality: there is no rounding to be
    // generous about when nothing rendered differently.
    assert_eq!(
        after,
        before,
        "{surface:?}: a press that changed NOTHING moved the reader {} px. #173: `applyReading` \
         ended in an unconditional `viewport.remeasure()`, so the height cache was cleared and \
         the position re-derived from estimates even when the page rendered identically.",
        (after - before).abs()
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_a_press_that_changes_nothing_does_not_move_the_reader() {
    let _serial = serial();
    let fx = fixture_audit("audit-p4-classic");
    let page = open(Surface::Classic, &fx, 2963);
    scenario_a_press_that_changes_nothing_does_not_move_the_reader(&page.tab, Surface::Classic);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_press_that_changes_nothing_does_not_move_the_reader() {
    let _serial = serial();
    let fx = fixture_audit("audit-p4-app");
    let page = open(Surface::AppShell, &fx, 2964);
    scenario_a_press_that_changes_nothing_does_not_move_the_reader(&page.tab, Surface::AppShell);
}
