//! **#174 — the rendering-control audit, by MEASURED EFFECT (P2, P3, P4).**
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
//! What each stage adds, all on the one instrument:
//!
//! * **P2** — a control's REACH, as a predicate quantified over the DOM: `−` changes `font-size`
//!   on everything inside a `[data-code]` container and nothing outside one; `w` changes the wrap
//!   longhand on every `<pre>` and everything inside `[data-code]`; a fold changes its own record
//!   and what that record CONTAINS.
//! * **P3** — EQUIVALENCE as a set difference, over cells keyed `<record id>|<group>` so the two
//!   pages can be compared despite legitimately different trees and class names.
//! * **P4** — a press whose effect set is EMPTY must not move the reader, at two widths and two
//!   themes: cascade bugs hide at a narrow window and colour rules hide in the other theme.
//!
//! Every claim here has been verified by MUTATION in both directions — break the rule and the
//! case names the bug; over-reach and it names that too. A green audit that has never gone red
//! is worth nothing, and that is the whole history of this task.
//!
//! Run: `cargo build --release -p claude-monitor -p claude-monitor-v2 && cargo test -p
//! claude-replay-browser-tests --test rendering_audit -- --ignored`.

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

/// **WHAT THE INSTRUMENT CANNOT ADDRESS, asserted so it cannot widen in silence.**
///
/// A record root is where a page stamps a record id, and the two pages used to disagree about
/// which records get one. The classic page has stamped every mounted block since `matBlock`
/// (`e.id = b.id`), prose turns included; the app shell stamped `data-record-id` only on a
/// `.renderer` — a TOOL rendering — while a user turn, an assistant turn and a slash-command card
/// were addressable by block index and by nothing else. Stage A had recorded the same asymmetry
/// from the other side ("the two kinds with no renderer key are the prose views") without anyone
/// connecting it to the capture.
///
/// It surfaced when a control whose domain is exactly those turns — the raw-text preference —
/// reported that no `user` record existed at all, which meant every app-shell claim here had been
/// quantified over the tool renderings and none of the prose. The fix was in the SHELL, not in
/// the audit: a record the DOM cannot name is a record no audit can quantify over, and the
/// reference page had the property already. `unit.view.id` was right there feeding the deep-link
/// button; it now stamps the turn wrapper too, so a prose turn is `t18` on both pages and P3's
/// cells line up by construction rather than by a lookup.
///
/// Both lists are empty today. The equality is asserted in BOTH directions, so a kind that stops
/// being addressable fails here rather than quietly leaving the quantifier.
const NOT_CAPTURED: [(&str, &[&str]); 2] = [("Classic", &[]), ("AppShell", &[])];

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
        Surface::AppShell => (format!("?ui=app&session={sid}{}", if std::env::var("AUDIT_TRACE").is_ok() { "&trace=viewport" } else { "" }), "document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3 && document.querySelector('.transcript').scrollHeight > document.querySelector('.transcript').clientHeight * 3", "document.querySelector('.virtual-window') ? document.querySelector('.virtual-window').children.length : 'no window'"),
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
    var owner = {}, kind = {};
    [].slice.call(document.querySelectorAll(sel)).forEach(function (root) {
      var id = idAttr === "id" ? root.id : root.getAttribute(idAttr);
      if (!id) return;
      // Which record CONTAINS this one. A record nested inside another (a Thinking that absorbed
      // its tool calls, a sub-agent's children) is inside its parent's body, so a control that
      // collapses the parent legitimately reaches it — that is containment, not leakage, and
      // only the tree can tell the two apart.
      var up = root.parentElement ? root.parentElement.closest(sel) : null;
      owner[id] = up ? (idAttr === "id" ? up.id : up.getAttribute(idAttr)) : null;
      // The record's KIND, from whichever stamp the page carries — the classic page writes
      // `data-kind` in `matBlock`, the app shell `data-record-kind` on the turn wrapper. A third
      // partition, over the same snapshot: some controls govern a kind of RECORD rather than a
      // kind of content.
      var owning = root.closest("[data-kind], [data-record-kind]");
      kind[id] = owning ? (owning.dataset.recordKind || owning.dataset.kind || "") : "";
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
        // Whether the reader can SEE this element right now. A windowing page unmounts what has
        // scrolled out of range and that is its job (#50) — turning wrap on makes the document
        // taller and the classic page drops a block twenty records above the reader. What a
        // control may never do is take away something on screen.
        var box = el.getBoundingClientRect();
        snap.set(id + "/" + path, {
          view: box.bottom > 0 && box.top < (window.innerHeight || 0) && box.width > 0,
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
    snap.kind = kind;
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
    // The effect set partitioned by RECORD KIND. A control can govern a kind of RECORD rather
    // than a kind of content — the raw-text preference is about USER turns and about nothing
    // else — and the kind comes from each page's own stamp, so no vocabulary of mine enters it.
    reportKinds: function () {
      var before = this.snap, after = capture(this.sel, this.idAttr);
      var kinds = {};
      before.forEach(function (a, key) {
        var id = key.slice(0, key.indexOf("/"));
        var k = before.kind[id] || "?";
        var g = kinds[k] || (kinds[k] = { n: 0, changed: 0, records: {}, props: {}, sample: [] });
        g.n++;
        var b = after.get(key);
        if (!b) return;
        var moved = [];
        for (var p in a.props) if (a.props[p] !== b.props[p]) moved.push(p);
        if (!moved.length) return;
        g.changed++;
        g.records[id] = true;
        moved.forEach(function (pr) { g.props[pr] = (g.props[pr] || 0) + 1; });
        if (g.sample.length < 6) g.sample.push(key + " <" + b.tag.toLowerCase() + "." + (b.cls || "") + "> [" + moved.slice(0, 5).join(",") + "]");
      });
      for (var k in kinds) kinds[k].records = Object.keys(kinds[k].records);
      return kinds;
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
      return { present: present, changed: changed, kind: before.kind };
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
      var gone = [], goneInView = [], added = [], changedTotal = 0;
      after.forEach(function (_, key) { if (!before.has(key)) added.push(key); });
      before.forEach(function (a, key) {
        var b = after.get(key);
        if (!b) { gone.push(key); if (a.view) goneInView.push(key); return; }
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
               gone: gone.length, goneInView: goneInView.length, added: added.length,
               goneSample: gone.slice(0, 6), goneInViewSample: goneInView.slice(0, 6),
               addedSample: added.slice(0, 6),
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
    // The opener's own residue, cleared. Opening a classic fold adds `.anim` for a 0.16s entry
    // animation; the class then sits there until the block is rebuilt, at which point the
    // animation longhands move — under whatever control happened to cause the rebuild. That is
    // the OPENER's effect, not the control's, and leaving it in would either be misread as reach
    // (it was, once) or have to be blessed as a benign property, which is worse.
    eval(
        tab,
        "(function(){ document.querySelectorAll('.anim').forEach(function (e) { e.classList.remove('anim'); }); return 'ok'; })()",
    );
    settle();
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
            && idle["goneInView"].as_i64() == Some(0)
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

    // Which kinds the INSTRUMENT can actually see, as against which the page mounted. The gap is
    // the blind spot named at `NOT_CAPTURED`, asserted as an equality in both directions.
    let (sel, id_attr) = roots(surface);
    eval(tab, AUDIT_JS);
    probe(tab, &format!("window.__audit.arm('{sel}', '{id_attr}')"));
    let captured: std::collections::BTreeSet<String> = probe(
        tab,
        "(function(){ var k = window.__audit.snap.kind, out = {}; for (var id in k) out[k[id]] = 1; return Object.keys(out).sort(); })()",
    )
    .as_array()
    .into_iter()
    .flatten()
    .filter_map(|k| k.as_str().filter(|s| !s.is_empty()).map(str::to_string))
    .collect();
    let blind: Vec<&String> = got.difference(&captured).collect();
    let expected_blind = NOT_CAPTURED
        .iter()
        .find(|(name, _)| *name == format!("{surface:?}"))
        .map(|(_, kinds)| *kinds)
        .unwrap_or(&[]);
    assert_eq!(
        blind.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        expected_blind.to_vec(),
        "{surface:?}: the instrument sees every kind the page mounted, except the ones named at \
         NOT_CAPTURED. A kind the page draws and the instrument cannot address is outside every \
         predicate in this file, and a predicate that passes over an unreachable domain is \
         indistinguishable from a clean audit. Mounted {got:?}, captured {captured:?}"
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
    // Used values resolved by layout, not set by the cascade. The MARGINS are here because
    // `margin: auto` is resolved from the free space in the box, precisely like `width` — and
    // that is not a guess: CI caught it and this machine did not. `.codebox .codebar` is
    // right-aligned with `margin-left:auto`, so when wrap changes a code pane's width the bar's
    // computed margin moves with it; the two machines differ in window size, so the pane
    // overflowed on one and not the other. A property that can be `auto` is a property layout
    // resolves, and belongs here rather than being discovered one CI run at a time.
    matches!(prop, "block-size" | "inline-size" | "height" | "width" | "perspective-origin" | "transform-origin"
        | "margin-top" | "margin-right" | "margin-bottom" | "margin-left"
        | "margin-block-start" | "margin-block-end" | "margin-inline-start" | "margin-inline-end")
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
        report["goneInView"].as_i64(),
        Some(0),
        "{surface:?}: {control} took away something ON SCREEN. Unmounting what has scrolled out \
         of range is a windowing page's job (#50) — turning wrap on makes the document taller \
         and the classic page drops a block far above the reader — but a control may never \
         remove a rendering the reader can see: {report}"
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
    kind: std::collections::BTreeMap<String, String>,
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
        kind: got["kind"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect(),
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
    // …and the comparison covers every KIND of record, on both pages. The obvious scope
    // guarantee — "the shell's cells are a subset of the classic page's" — was true when I first
    // measured it and true only by luck: the two pages window their DOM independently, so they
    // mount different RANGES of the padding above the corpus, and the day the shell mounted four
    // records the classic page had not, the assertion fired on a windowing difference and called
    // it a rendering one. What actually matters is that no KIND fell out of the comparison, and
    // that does not depend on which records each page happened to keep mounted.
    let covered: std::collections::BTreeSet<&str> = classic
        .present
        .keys()
        .filter(|cell| shell.present.contains_key(*cell))
        .filter_map(|cell| classic.kind.get(cell.split('|').next().unwrap_or("")))
        .map(String::as_str)
        .filter(|k| !k.is_empty())
        .collect();
    let declared: std::collections::BTreeSet<String> = audit_kinds(&audit_records(&audit_corpus()))
        .into_iter()
        .collect();
    let uncompared: Vec<&String> = declared
        .iter()
        .filter(|k| !covered.contains(k.as_str()))
        .collect();
    assert!(
        uncompared.is_empty(),
        "{control}: {} record kind(s) fell out of the comparison entirely — a kind only one page \
         had mounted is a kind this audit did not compare, and silence there is exactly what the \
         task was written to remove: {uncompared:?}",
        uncompared.len()
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
fn scenario_a_press_that_changes_nothing_does_not_move_the_reader(
    tab: &headless_chrome::Tab,
    surface: Surface,
) {
    jump_to_end(tab, surface);
    settle();
    open_everything(tab, surface);
    settle();
    settle();
    // BOTH ENDS of the size range, and — where the page offers one — a Reset that resets nothing.
    // #173 named three no-op presses and fixing one of them is not the claim: "`-` held down at
    // the floor, `Reset` on an already-default panel, a second click on the same switch".
    for (key, edge) in [("-", "floor"), ("+", "ceiling")] {
        // Walk to the edge, which is where a press stops changing anything. Bounded, and the
        // premise is CHECKED below rather than assumed: short of the edge, the press would change
        // something and the case would say so.
        for _ in 0..16 {
            press(tab, key);
        }
        settle();
        no_op_press(
            tab,
            surface,
            &format!("`{key}` at the size {edge}"),
            &|tab| press(tab, key),
        );
    }
    // The app shell's Reset, pressed on a panel that is already at its defaults — the second of
    // #173's three. The classic page has no such control on purpose ("a step that lands back ON
    // the baseline RELEASES the block rather than pinning it"), so there is nothing to press.
    if surface == Surface::AppShell {
        let reset = "(function(){ var r = document.querySelector('[data-reading-reset]'); if (!r) { var b = document.getElementById('readingBtn'); if (b) b.click(); r = document.querySelector('[data-reading-reset]'); } if (!r) return 'no control'; r.click(); return 'pressed'; })()";
        assert_eq!(
            eval(tab, reset).as_str().unwrap_or(""),
            "pressed",
            "{surface:?}: the shell offers a Reset to press"
        );
        settle();
        settle();
        no_op_press(tab, surface, "Reset on an already-default panel", &|tab| {
            eval(tab, reset);
            settle();
        });
    }
}

/// One no-op press, measured: park the reader mid-document, arm, press, and require BOTH that
/// the effect set is empty and that the reader did not move. Taking the premise from the same
/// instrument is what makes this an audit rather than a regression test — a press that turns out
/// to change something fails saying so instead of quietly testing a different claim.
///
/// The reader is parked MID-DOCUMENT on purpose. At the tail the page converges to the bottom on
/// its own, so a position that was rewritten and a position that was kept look identical — the
/// lesson #185's fixture had to learn from the other direction.
fn no_op_press(
    tab: &headless_chrome::Tab,
    surface: Surface,
    what: &str,
    press_it: &dyn Fn(&headless_chrome::Tab),
) {
    // Re-park from the TAIL each time rather than scrolling further up from wherever the last
    // press left the reader: two of these in a row walked to scrollTop 0, where a rewritten
    // position cannot be told from a kept one because there is nowhere to move to.
    jump_to_end(tab, surface);
    settle();
    harness::scroll_by(tab, surface, -2500);
    settle();
    settle();
    arm(tab, surface);
    let before = harness::scroll_top(tab, surface);
    assert!(
        before > 50.0,
        "{surface:?}: the reader is parked away from the top for {what}, where a rewritten \
         position is visible at all: scrollTop {before}"
    );
    press_it(tab);
    let report = report(tab);
    let after = harness::scroll_top(tab, surface);
    assert_eq!(
        report["changedTotal"].as_i64(),
        Some(0),
        "{surface:?}: {what} changed nothing — if it did, this case is testing a different claim \
         than the one it states: {report}"
    );
    assert_eq!(
        report["goneInView"].as_i64(),
        Some(0),
        "{surface:?}: …and took away nothing on screen: {report}"
    );
    // …and therefore the reader did not move. An exact equality: there is no rounding to be
    // generous about when nothing rendered differently.
    assert_eq!(
        after,
        before,
        "{surface:?}: {what} changed NOTHING and moved the reader {} px. #173: `applyReading` \
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

/// ── STAGE D — THE CONDITIONS: TWO WIDTHS AND TWO THEMES ─────────────────────────────────────
///
/// The task's own reason for them, and both are lessons this repo paid for: cascade bugs hide at
/// a NARROW window — a control that measures a perfectly good rect while sitting behind its own
/// header (#155, and "rect is not visibility" from #98) — and colour and contrast rules hide in
/// the OTHER THEME, which nothing in a light-theme run ever evaluates.
///
/// One case per surface rather than four, because the claims are the same claims: what changes
/// is the DOM they are quantified over, and the condition travels in the failure message. Each
/// press is undone before the next, so the size never walks down to its floor and leaves a later
/// iteration measuring a control that can no longer move.
fn set_theme(tab: &headless_chrome::Tab, dark: bool) {
    eval(
        tab,
        &format!(
            "(function(){{ var want = {}; var root = document.documentElement; \
             var now = root.getAttribute('data-theme') === 'dark'; \
             if (now !== want) {{ var b = document.getElementById('themeBtn') || document.getElementById('btn-theme'); \
             if (b) b.click(); else root.setAttribute('data-theme', want ? 'dark' : ''); }} \
             return root.getAttribute('data-theme') || 'light'; }})()",
            if dark { "true" } else { "false" }
        ),
    );
}

fn scenario_the_claims_hold_at_every_width_and_theme(tab: &headless_chrome::Tab, surface: Surface) {
    for (width, dark) in [
        (1400.0, false),
        (1400.0, true),
        (900.0, false),
        (900.0, true),
    ] {
        let condition = format!("{}px {}", width as i64, if dark { "dark" } else { "light" });
        harness::resize(tab, width, 950.0);
        // `set_bounds` is asynchronous and CI's headless Linux has been seen to ignore a width
        // outright, so the case measures what it actually GOT and says so rather than assuming.
        harness::until(
            tab,
            &format!("Math.abs(innerWidth - {}) < 120", width as i64),
            "the window to reach the requested width",
            Duration::from_secs(10),
            "innerWidth",
        );
        set_theme(tab, dark);
        settle();
        jump_to_end(tab, surface);
        settle();
        // A resize re-renders and re-windows, so what was open may not be.
        open_everything(tab, surface);
        settle();

        arm(tab, surface);
        if std::env::var("AUDIT_TRACE").is_ok() {
            println!("TRACE-BEFORE {condition} {}", eval(tab, "(function(){ var t = window.__viewportTrace || []; return JSON.stringify({ n: t.length, last: t[t.length-1] }); })()"));
        }
        press(tab, "-");
        if std::env::var("AUDIT_TRACE").is_ok() {
            println!("TRACE-AFTER {condition} {}", eval(tab, "(function(){ var t = window.__viewportTrace || []; return JSON.stringify(t.slice(-40)); })()"));
        }
        let size = report(tab);
        assert_governs(
            &size,
            surface,
            &format!("the code-size control (−) at {condition}"),
            "font-size",
            &["code-mono", "code-other"],
            &["mono", "pre", "other"],
        );
        press(tab, "+"); // …and put it back, so the next iteration is not measuring the floor.
        settle();

        arm(tab, surface);
        press(tab, "w");
        let wrap = report(tab);
        let prop = wrap_prop(&wrap);
        assert_governs(
            &wrap,
            surface,
            &format!("the wrap control (w) at {condition}"),
            prop,
            &["code-mono", "code-other", "pre"],
            &["mono", "other"],
        );
        press(tab, "w");
        settle();
    }
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_the_claims_hold_at_every_width_and_theme() {
    let _serial = serial();
    let fx = fixture_audit("audit-conditions-classic");
    let page = open(Surface::Classic, &fx, 2965);
    scenario_the_claims_hold_at_every_width_and_theme(&page.tab, Surface::Classic);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_claims_hold_at_every_width_and_theme() {
    let _serial = serial();
    let fx = fixture_audit("audit-conditions-app");
    let page = open(Surface::AppShell, &fx, 2966);
    scenario_the_claims_hold_at_every_width_and_theme(&page.tab, Surface::AppShell);
}

/// ── STAGE B, the fourth control: a preference that governs a KIND of record ─────────────────
///
/// CLAIM. The raw-text preference (#109) changes the rendering of `user` records and of no record
/// of any other kind.
///
/// A third partition over the same snapshot, and the reason for adding it: the first three claims
/// were about a kind of CONTENT (is it code, is it verbatim text) or about a place in the tree
/// (this record and what it contains). This one is about a kind of RECORD, and the kind comes from
/// each page's own stamp — `data-kind` on the classic page, `data-record-kind` on the app shell —
/// so no vocabulary of mine enters it.
///
/// This case could not be written at all until two things were fixed, both of which it found:
/// the app shell did not stamp a record id on its prose turns, so its `user` records were outside
/// every quantifier here; and the classic page lost a fold's head step on any rebuild (#189), so
/// a preference about user turns moved `.tool-target` on ten other kinds.
///
/// The driver is per-surface because the control is: both pages carry the same PREFERENCE
/// (`READING_KEY`, folded into one in #109) but each offers it in its own chrome. What must not
/// differ is the claim.
fn scenario_raw_text_governs_exactly_the_user_turns(tab: &headless_chrome::Tab, surface: Surface) {
    jump_to_end(tab, surface);
    settle();
    open_everything(tab, surface);
    settle();
    arm(tab, surface);
    let pressed = eval(
        tab,
        match surface {
            Surface::Classic => "(function(){ var b = document.getElementById('btn-raw'); if (!b) return 'no control'; b.click(); return 'pressed'; })()",
            Surface::AppShell => "(function(){ var t = document.querySelector('[data-reading-toggle=\"rawUser\"]'); if (!t) { var r = document.getElementById('readingBtn'); if (r) r.click(); t = document.querySelector('[data-reading-toggle=\"rawUser\"]'); } if (!t) return 'no control'; t.click(); return 'pressed'; })()",
        },
    );
    assert_eq!(
        pressed.as_str().unwrap_or(""),
        "pressed",
        "{surface:?}: the page offers the raw-text preference: {pressed}"
    );
    settle();
    settle();
    let kinds = probe(tab, "window.__audit.reportKinds()");
    let by_kind = kinds.as_object().cloned().unwrap_or_default();
    assert!(
        by_kind.len() > 5,
        "{surface:?}: several kinds are under measurement, or the claim is vacuous: {kinds}"
    );
    assert!(
        by_kind
            .get("user")
            .and_then(|k| k["changed"].as_i64())
            .unwrap_or(0)
            > 0,
        "{surface:?}: the preference reached the USER turns at all — the half a control wired to a \
         selector list fails (#161, #173): {kinds}"
    );
    // Records of other kinds may move ONLY in the affordance/geometry category — the same two
    // reasons named at `geometry_or_affordance`, for the same reason. Measured on the app shell:
    // a slash-command card carries its own per-turn raw toggle (`components.js` emits one for
    // every user and assistant turn), and the page-wide preference LIGHTS IT UP. That is a
    // control showing its own state, exactly like the classic page's `.ms-wrap` button under
    // `w`, and its record kind is `command` because the DOM's kind vocabulary and the rendering
    // are not the same thing: a slash-command card is a USER turn that renders as a command.
    let strayed: Vec<String> = by_kind
        .iter()
        .filter(|(kind, _)| kind.as_str() != "user")
        .filter_map(|(kind, k)| {
            let odd: Vec<&String> = k["props"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(prop, _)| prop)
                .filter(|prop| !geometry_or_affordance(prop))
                .collect();
            (!odd.is_empty()).then(|| {
                format!(
                    "{kind} moved {odd:?} on {} of {} elements, e.g. {:?}",
                    k["changed"], k["n"], k["sample"]
                )
            })
        })
        .collect();
    assert!(
        strayed.is_empty(),
        "{surface:?}: the raw-text preference is about USER turns, and it changed the RENDERING \
         of records of other kinds: {strayed:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_raw_text_governs_exactly_the_user_turns() {
    let _serial = serial();
    let fx = fixture_audit("audit-raw-classic");
    let page = open(Surface::Classic, &fx, 2967);
    scenario_raw_text_governs_exactly_the_user_turns(&page.tab, Surface::Classic);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_raw_text_governs_exactly_the_user_turns() {
    let _serial = serial();
    let fx = fixture_audit("audit-raw-app");
    let page = open(Surface::AppShell, &fx, 2968);
    scenario_raw_text_governs_exactly_the_user_turns(&page.tab, Surface::AppShell);
}
