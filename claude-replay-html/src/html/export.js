// claude-replay HTML export — renderer + behavior. No dependencies, no network
// beyond an optional same-directory companion file.
//
// The page is a fixed shell; all content arrives as an append-only JSONL stream
// (one `meta` line, then one line per block). A one-off export inlines the whole
// stream in #session-data. A live export additionally sets `data-src` on <body>,
// and we poll that companion for lines appended since the last read — so growing
// a session is literally "append a line".
//
// Rust pre-renders markdown / syntax highlighting / diffs into safe fragments;
// every other value is inserted with textContent, so raw text can never inject.
(function () {
  "use strict";

  var THEME_KEY = "claude-replay-export-theme";
  // §8.3/§8.8 code-density + width prefs — global and persisted.
  var MS_KEY = "claude-replay-export-ms";
  var WRAP_KEY = "claude-replay-export-wrap";
  var WIDE_KEY = "claude-replay-export-wide";
  function lsGet(k) { try { return localStorage.getItem(k); } catch (e) { return null; } }
  function lsSet(k, v) { try { localStorage.setItem(k, v); } catch (e) { /* private mode */ } }
  var ms = parseFloat(lsGet(MS_KEY)) || 12.5;
  var wrap = lsGet(WRAP_KEY) !== "0"; // wrap by default
  var wide = lsGet(WIDE_KEY) === "1";
  var root = document.documentElement;
  var stream = document.getElementById("stream");
  var turnlist = document.getElementById("turnlist");
  var curTurn = null;
  var raf = null;
  var moreSeq = 0;
  var consumed = 0; // JSONL lines already rendered
  var filter = null; // active tool-use filter (tool display name), or null
  var savedFolds = null; // fold open/closed snapshot to restore when filtering ends

  function $(id) { return document.getElementById(id); }
  function all(sel) { return Array.prototype.slice.call(document.querySelectorAll(sel)); }
  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }
  function fmtTime(ts) {
    // hh:mm alone for TODAY's turns; older turns carry their date (and year when it
    // differs) — a bare clock time on a week-old turn identifies nothing. Client-side
    // "now" on purpose: the page renders live in a browser, so a dump's bytes carry this
    // code, never a baked render date.
    try {
      var d = new Date(ts * 1000), now = new Date();
      var t = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
      if (d.toDateString() === now.toDateString()) return t;
      var opts = { month: "short", day: "numeric" };
      if (d.getFullYear() !== now.getFullYear()) opts.year = "numeric";
      return d.toLocaleDateString([], opts) + " " + t;
    } catch (e) { return ""; }
  }
  function fmtDur(s) {
    if (!s || s < 0) return "";
    var h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60);
    return h ? h + "h " + m + "m" : m + "m";
  }

  // ── theme ────────────────────────────────────────────────────────────
  function applyTheme(name) {
    root.setAttribute("data-theme", name);
    var b = $("btn-theme");
    if (b) b.textContent = name === "light" ? "◐ Dark" : "◑ Light";
  }
  var stored = null;
  try { stored = localStorage.getItem(THEME_KEY); } catch (e) { /* private mode */ }
  applyTheme(stored === "dark" || stored === "light" ? stored : "light");

  // ── rendering ────────────────────────────────────────────────────────

  // A capped list: first `cap` children stay visible, the rest go into a hidden
  // div revealed by a "⋯ N more lines" button. All content is always present.
  function capped(container, rows, cap, after, toLine) {
    if (!cap || rows.length <= cap) {
      rows.forEach(function (r) { container.appendChild(r); });
      return;
    }
    rows.slice(0, cap).forEach(function (r) { container.appendChild(r); });
    var id = "more" + ++moreSeq;
    var hidden = el("div", "more");
    hidden.id = id;
    rows.slice(cap).forEach(function (r) { hidden.appendChild(r); });
    container.appendChild(hidden);
    // §8.8 name the range, not just a count: "⋯ 126 more lines · to line 132".
    var label = "⋯ " + (rows.length - cap) + " more lines" + (toLine != null ? " · to line " + toLine : "");
    var btn = el("button", "morebtn", label);
    btn.dataset.more = id;
    after.appendChild(btn);
  }

  function numberedRows(rows) {
    return rows.map(function (r) {
      var row = el("div", "nrow");
      row.appendChild(el("span", "gut", String(r[0])));
      var code = el("span", "code");
      code.innerHTML = r[1]; // Rust-escaped + syntect spans
      row.appendChild(code);
      return row;
    });
  }

  function diffRows(rows) {
    return rows.map(function (r) {
      var kind = r[0];
      var row = el("div", "nrow" + (kind === "ctx" ? "" : " " + kind));
      row.appendChild(el("span", "gut", r[1] == null ? "" : String(r[1])));
      row.appendChild(el("span", "mark", kind === "add" ? "+" : kind === "del" ? "−" : " "));
      row.appendChild(el("span", "code", r[2]));
      return row;
    });
  }

  function renderPart(p, into) {
    if (p.p === "md" || p.p === "think") {
      var d = el("div", p.p === "think" ? "think-body" : "");
      d.innerHTML = p.h; // pre-rendered + escaped by Rust
      into.appendChild(d);
      return;
    }
    if (p.p === "note") {
      var note = el("div", "note");
      note.appendChild(el("span", null, "⎿"));
      note.appendChild(el("span", null, p.x));
      into.appendChild(note);
      return;
    }
    if (p.p === "pre") {
      var wrap = el("div", "result");
      wrap.appendChild(el("span", "lead", "⎿"));
      var lines = String(p.x).split("\n");
      var box = el("div");
      box.style.flex = "1";
      box.style.minWidth = "0";
      if (p.cap && lines.length > p.cap) {
        box.appendChild(el("pre", null, lines.slice(0, p.cap).join("\n")));
        var id = "more" + ++moreSeq;
        var hidden = el("div", "more");
        hidden.id = id;
        hidden.appendChild(el("pre", null, lines.slice(p.cap).join("\n")));
        box.appendChild(hidden);
        var btn = el("button", "morebtn", "⋯ " + (lines.length - p.cap) + " more lines");
        btn.dataset.more = id;
        box.appendChild(btn);
      } else {
        box.appendChild(el("pre", null, p.x));
      }
      wrap.appendChild(box);
      into.appendChild(wrap);
      return;
    }
    if (p.p === "num" || p.p === "diff") {
      var box2 = el("div", p.p === "num" ? "numbered" : "diff");
      var holder = el("div");
      // The final rendered row's line number, for the "· to line M" expander label.
      var toLine = null;
      for (var i = p.rows.length - 1; i >= 0; i--) {
        var ln = p.p === "num" ? p.rows[i][0] : p.rows[i][1];
        if (ln != null) { toLine = ln; break; }
      }
      capped(box2, p.p === "num" ? numberedRows(p.rows) : diffRows(p.rows), p.cap, holder, toLine);
      into.appendChild(box2);
      while (holder.firstChild) into.appendChild(holder.firstChild);
      return;
    }
    if (p.p === "blocks") {
      p.items.forEach(function (b) { into.appendChild(renderBlock(b)); });
    }
  }

  function anchor(id) {
    var a = el("a", "alink", "#");
    a.href = "#" + id;
    a.title = "Copy a link to this spot";
    return a;
  }

  // ── #50 virtual DOM window ────────────────────────────────────────────
  // The DOM used to hold EVERY block (O(session) nodes — ~673k for the dev session,
  // hundreds of MB of renderer memory and multi-second layout stalls). Now the parsed
  // RECORDS are the source of truth and only the blocks within viewport ± MARGIN_PX
  // are materialized as DOM, between two spacer divs whose heights stand in for the
  // rest — the browser twin of the TUI's C-5 heights+prefix windowing. Heights are
  // estimated (EST_H) until a block is first laid out, then measured as the delta to
  // its next sibling (which absorbs margin collapse exactly); the prefix-sum array
  // over effective heights (0 when filter-hidden) drives scroll↔index mapping.
  // Fold/filter/search state lives on the records so it survives dematerialization.
  var records = [];      // block records, stream order — the source of truth
  var recHeights = [];   // effective px height per record (EST_H until measured)
  var recText = [];      // lazy lowercase text per record, for search (null = unbuilt)
  var recSearchParts = []; // lazy {start,end,mask} ownership spans into recText
  var recHit = [];       // with a filter active: does this record (or a nested one) match?
  var idIndex = {};      // block id (incl. nested items) -> top-level record index
  var loIdx = 0, hiIdx = 0; // materialized window [loIdx, hiIdx)
  var EST_H = 30;
  var MARGIN_PX = 1500;
  var prefix = null;     // prefix[i] = sum of effective heights of records[0..i)
  var topPad = null, botPad = null;
  var searchNeedle = ""; // active search term (lowercase), re-marked on materialize
  var searchScope = null; // `uatobrew:` prefix parse; w modifies matching, null = unscoped

  function isTurnKind(b) { return b.kind === "user" || b.kind === "command"; }
  function isHiddenRec(i) { return !!filter && !isTurnKind(records[i]) && !recHit[i]; }
  function effH(i) { return isHiddenRec(i) ? 0 : recHeights[i]; }
  function P() {
    if (!prefix) {
      prefix = new Float64Array(records.length + 1);
      for (var i = 0; i < records.length; i++) prefix[i + 1] = prefix[i] + effH(i);
    }
    return prefix;
  }
  // First index whose bottom edge lies below y (binary search over the prefix sums).
  function idxAt(y) {
    var p = P(), lo = 0, hi = records.length;
    while (lo < hi) {
      var mid = (lo + hi) >> 1;
      if (p[mid + 1] > y) hi = mid; else lo = mid + 1;
    }
    return lo;
  }
  function streamTop() {
    return stream.getBoundingClientRect().top + window.scrollY;
  }
  function ensurePads() {
    if (topPad) return;
    topPad = el("div", "vpad");
    botPad = el("div", "vpad");
    stream.appendChild(topPad);
    stream.appendChild(botPad);
  }
  function matEls() {
    var out = [];
    if (!topPad) return out;
    for (var n = topPad.nextSibling; n && n !== botPad; n = n.nextSibling) out.push(n);
    return out;
  }
  function matBlock(i) {
    var b = records[i];
    var e = renderBlock(b);
    e.dataset.idx = i;
    e.dataset.kind = b.kind;
    if (filter) {
      if (isTurnKind(b)) e.classList.add("filter-dim");
      else if (recHit[i]) {
        markFilterHit(e);
        if (b.id === filterCurId) {
          var fh = e.querySelector(":scope > .fold-h");
          if (fh) fh.classList.add("filter-cur");
        }
      }
    }
    return e;
  }
  // Post-materialization passes. Split into a WRITE phase (per element: strips,
  // wrap styles, search marks — no layout reads) and a single BATCHED clamp pass
  // (all layout reads together, then all writes), so materializing N blocks costs
  // O(1) forced layouts, not O(N) — the difference between a 15ms and a 650ms
  // window rebuild on this page.
  function postMat(e) {
    buildStripsIn(e);
    applyWrapIn(e);
    reapplySmallMore(e);
    if (searchNeedle && searchInScope(+e.dataset.idx)) {
      markHits(e, searchNeedle, searchNeedle.length, !!(searchScope && searchScope.w));
      // The current hit survives rematerialization (#66) — same id-keyed idea as
      // the filter's `.filter-cur`.
      if (curHit && +e.dataset.idx === curHit.rec) {
        var ms = e.querySelectorAll("mark.hl");
        if (ms[curHit.mark]) ms[curHit.mark].classList.add("cur");
      }
    }
  }
  var CLAMP_LINES = 12; // long user turns clamp to this many lines + expander
  function clampBatch(els) {
    var jobs = [];
    els.forEach(function (e) {
      Array.prototype.forEach.call(e.querySelectorAll(".uturn-md"), function (md) {
        if (md.dataset.clampChecked) return;
        md.dataset.clampChecked = "1";
        jobs.push(md);
      });
    });
    if (!jobs.length) return;
    // Phase A: all reads (one layout pass)…
    var reads = jobs.map(function (md) {
      return { md: md, lh: parseFloat(getComputedStyle(md).lineHeight) || 25, sh: md.scrollHeight };
    });
    // …phase B: all writes.
    reads.forEach(function (r) {
      var cap = r.lh * CLAMP_LINES;
      if (r.sh <= cap + r.lh) return; // fits within N (+1 slack) lines
      var hidden = Math.round((r.sh - cap) / r.lh);
      r.md.style.maxHeight = cap + "px";
      r.md.classList.add("clamped");
      var btn = el("button", "morebtn clampbtn", "⋯ " + hidden + " more lines");
      btn.dataset.cap = cap;
      btn.dataset.more = "⋯ " + hidden + " more lines";
      r.md.after(btn);
    });
  }
  function updatePads() {
    var p = P();
    ensurePads();
    topPad.style.height = p[loIdx] + "px";
    botPad.style.height = Math.max(0, p[records.length] - p[hiIdx]) + "px";
  }
  // Measure the materialized run: each block's effective height is the offsetTop
  // delta to its next sibling (the last one measures against the bottom pad).
  function measureWindow() {
    var els = matEls();
    if (!els.length) return false;
    var changed = false;
    for (var k = 0; k < els.length; k++) {
      var i = +els[k].dataset.idx;
      var next = k + 1 < els.length ? els[k + 1] : botPad;
      var h = next.offsetTop - els[k].offsetTop;
      if (h > 0 && Math.abs(h - recHeights[i]) > 0.5) {
        recHeights[i] = h;
        changed = true;
      }
    }
    if (changed) prefix = null;
    return changed;
  }
  // Materialize exactly [lo, hi): incremental trim/extend at both ends; a disjoint
  // jump rebuilds. Skips filter-hidden records (they contribute 0 height).
  function setWindow(lo, hi) {
    lo = Math.max(0, Math.min(lo, records.length));
    hi = Math.max(lo, Math.min(hi, records.length));
    ensurePads();
    var fresh = [];
    if (lo >= hiIdx || hi <= loIdx || hiIdx === loIdx) {
      matEls().forEach(function (e) { e.remove(); });
      var frag = document.createDocumentFragment();
      for (var i = lo; i < hi; i++) {
        if (isHiddenRec(i)) continue;
        var e = matBlock(i);
        frag.appendChild(e);
        fresh.push(e);
      }
      stream.insertBefore(frag, botPad);
      loIdx = lo; hiIdx = hi;
    } else {
      while (loIdx < lo && topPad.nextSibling !== botPad) {
        // Trim by the element's REAL index (#94): with filter-hidden records the
        // first DOM child can sit far above loIdx — blindly removing one node per
        // index step deletes visible elements that belong INSIDE the new window.
        if (+topPad.nextSibling.dataset.idx >= lo) break; // [loIdx..lo) is all hidden
        topPad.nextSibling.remove();
        loIdx++;
        while (loIdx < lo && loIdx < hiIdx && isHiddenRec(loIdx)) loIdx++;
      }
      if (loIdx < lo && loIdx < hiIdx) loIdx = lo;
      if (lo < loIdx) {
        var ftop = document.createDocumentFragment();
        for (var a = lo; a < loIdx; a++) {
          if (isHiddenRec(a)) continue;
          var ea = matBlock(a);
          ftop.appendChild(ea);
          fresh.push(ea);
        }
        stream.insertBefore(ftop, topPad.nextSibling);
        loIdx = lo;
      }
      while (hiIdx > hi && botPad.previousSibling !== topPad) {
        // Same real-index guard as the top trim (#94): the last DOM child can sit
        // far below hiIdx when the tail range is filter-hidden.
        if (+botPad.previousSibling.dataset.idx < hi) break; // [hi..hiIdx) is all hidden
        botPad.previousSibling.remove();
        hiIdx--;
        while (hiIdx > hi && hiIdx > loIdx && isHiddenRec(hiIdx - 1)) hiIdx--;
      }
      if (hiIdx > hi) hiIdx = hi;
      if (hi > hiIdx) {
        var fbot = document.createDocumentFragment();
        for (var c = hiIdx; c < hi; c++) {
          if (isHiddenRec(c)) continue;
          var ec = matBlock(c);
          fbot.appendChild(ec);
          fresh.push(ec);
        }
        stream.insertBefore(fbot, botPad);
        hiIdx = hi;
      }
    }
    fresh.forEach(postMat);   // writes only — no layout reads
    clampBatch(fresh);        // one batched read pass + writes
    measureWindow();          // one layout read pass
    updatePads();
  }
  // Recompute the window for the current scroll position, anchoring the content
  // under the viewport so height-measurement drift never visibly jumps the page.
  function updateView() {
    if (!records.length) return;
    // Fully-rendered filter mode (#94): the whole (small) visible set stays
    // materialized — no windowing, no estimate churn.
    if (filter && filterFull) {
      if (loIdx !== 0 || hiIdx !== records.length) setWindow(0, records.length);
      return;
    }
    // Prefer INDEX-anchored windowing (#66): when a materialized element is visible,
    // extend the window around ITS record index by walking effective heights — immune
    // to prefix-estimate drift, which otherwise makes a post-jump updateView compute
    // a window that EXCLUDES the very block just navigated to (scrollY maps through
    // stale estimates to different indices than the real rects on screen).
    var anchorEl = null, anchorTop = 0;
    matEls().some(function (e) {
      var r = e.getBoundingClientRect();
      if (r.bottom > 0 && r.top < window.innerHeight) { anchorEl = e; anchorTop = r.top; return true; }
      return false;
    });
    var lo, hi;
    if (anchorEl) {
      var ai = +anchorEl.dataset.idx;
      lo = ai;
      var px = anchorTop + MARGIN_PX; // content to keep above the viewport top
      while (lo > 0 && px > 0) { lo--; px -= effH(lo); }
      hi = ai;
      px = window.innerHeight - anchorTop + MARGIN_PX;
      while (hi < records.length && px > 0) { px -= effH(hi); hi++; }
    } else {
      var y0 = window.scrollY - streamTop();
      lo = idxAt(y0 - MARGIN_PX);
      hi = idxAt(y0 + window.innerHeight + MARGIN_PX) + 1;
    }
    setWindow(lo, hi);
    if (anchorEl && anchorEl.isConnected) {
      var d = anchorEl.getBoundingClientRect().top - anchorTop;
      if (Math.abs(d) > 1) window.scrollBy(0, d);
    }
    // The page's height is only true once a window has been measured, and `atBottom()` is
    // a question about the height — so the pill has to be reconsidered wherever the height
    // can move, not only on scroll (#170/#171). Guarded to a no-op unless it changed.
    paintBadge();
  }
  // Re-render the materialized window in place (fold/filter state changed).
  function refreshWindow() {
    var lo = loIdx, hi = hiIdx;
    loIdx = hiIdx = 0;
    matEls().forEach(function (e) { e.remove(); });
    setWindow(lo, hi);
  }
  // Register a record's ids (its own + nested items') for deep links and search nav.
  function indexIds(b, top) {
    if (b.id) idIndex[b.id] = top;
    (b.body || []).forEach(function (p) {
      if (p.p === "blocks") p.items.forEach(function (c) { indexIds(c, top); });
    });
  }
  // Fold overrides from explicit user gestures (#61): a live update re-emits the
  // open turn's records with the server's AUTHORED open state, which used to snap a
  // block the user expanded back shut (visible exactly when following the tail).
  // Every record entering the stream re-applies the user's overrides, keyed by
  // block id (stable across re-emission; stale ids simply never match again).
  var userFolds = {};
  // Small "⋯ N more lines" expansions survive rematerialization (#67): a block just
  // over the display cap keeps its expansion (recorded by record-id + the button's
  // ordinal within the block — stable, since re-renders are deterministic from the
  // records); LARGE expansions stay ephemeral by design (a reset is welcome there).
  var MAX_BUFFER_LINES = 200;
  var smallMore = {}; // "recId:ordinal" -> true
  function hiddenLineCount(hidden) {
    var rows = hidden.querySelectorAll(".nrow").length;
    if (rows) return rows;
    var t = hidden.textContent || "";
    return t.split("\n").length;
  }
  function expandMore(btn) {
    var hidden = $(btn.dataset.more);
    if (hidden) hidden.classList.add("shown");
    btn.remove();
  }
  // Stamp stable ordinals on a fresh element's expand buttons (indices computed at
  // click time would shift as earlier buttons get removed), then re-apply recorded
  // small expansions.
  function reapplySmallMore(e) {
    var recId = e.id;
    if (!recId) return;
    var btns = Array.prototype.slice.call(e.querySelectorAll(".morebtn:not(.clampbtn)"));
    for (var k = btns.length - 1; k >= 0; k--) {
      btns[k].dataset.ord = k;
      if (smallMore[recId + ":" + k]) expandMore(btns[k]);
    }
  }
  function applyUserFolds(b) {
    if (b.id && userFolds[b.id] !== undefined && isFoldRec(b)) b.open = userFolds[b.id];
    (b.body || []).forEach(function (p) {
      if (p.p === "blocks") p.items.forEach(applyUserFolds);
    });
  }
  function pushRecord(b) {
    applyUserFolds(b);
    records.push(b);
    recHeights.push(EST_H);
    recText.push(null);
    recSearchParts.push(null);
    recHit.push(false);
    indexIds(b, records.length - 1);
    if (b.turn != null) addTurn(b);
    else if (b.epoch) addEpoch(b);
  }
  // Walk a top-level record's tree to the node with `id`, applying `fn` to every
  // container on the path (for open-chains) and to the node itself.
  function withChain(b, id, fn) {
    if (b.id === id) { fn(b); return true; }
    var parts = b.body || [];
    for (var i = 0; i < parts.length; i++) {
      if (parts[i].p !== "blocks") continue;
      var items = parts[i].items;
      for (var j = 0; j < items.length; j++) {
        if (withChain(items[j], id, fn)) { fn(b); return true; }
      }
    }
    return false;
  }
  function isFoldRec(b) {
    return !(b.kind === "user" || b.kind === "attachment" || b.kind === "queue" || b.kind === "assistant");
  }
  function setRecordOpen(id, open) {
    var ti = idIndex[id];
    if (ti == null) return;
    withChain(records[ti], id, function (n) { if (isFoldRec(n)) n.open = open ? 1 : 0; });
  }
  function eachFoldRec(fn) {
    function walk(b) {
      if (isFoldRec(b)) fn(b);
      (b.body || []).forEach(function (p) {
        if (p.p === "blocks") p.items.forEach(walk);
      });
    }
    records.forEach(walk);
  }

  function chips(head, into) {
    (head.chips || []).forEach(function (c) {
      into.appendChild(el("span", "chip" + (c.c ? " " + c.c : ""), c.x));
    });
  }

  function renderBlock(b) {
    var head = b.head || {};
    var body = b.body || [];

    // Plain user turn — an always-open card. Long messages are clamped to a few
    // lines with a "more" expander (measured after layout in clampBatch).
    if (b.kind === "user") {
      var card = el("div", "uturn blk");
      card.id = b.id;
      card.dataset.turn = b.turn;
      card.dataset.label = b.label;
      card.appendChild(el("span", "caret", "❯"));
      var ub = el("div", "uturn-body");
      var md = el("div", "uturn-md");
      body.forEach(function (p) { renderPart(p, md); });
      ub.appendChild(md);
      card.appendChild(ub);
      if (b.ts) card.appendChild(el("span", "ts", fmtTime(b.ts)));
      card.appendChild(anchor(b.id));
      return card;
    }

    // A surfaced attachment — an always-open card with a clickable name that either
    // downloads the embedded content (Blob for text, data: URI for image) or reveals
    // the path in the file manager (served pages only). Exported pages show name only.
    if (b.kind === "attachment") {
        var h = b.head || {};
        var ac = el("div", "amark blk");
        ac.id = b.id;
        ac.appendChild(el("span", "acaret", "▤"));
        ac.appendChild(el("span", "akind", (h.att_kind || "file") + " "));
        var an = el("span", "aname", h.att_name || "attachment");
        var text = h.att_text, datauri = h.att_datauri, path = h.att_path, href = h.att_href;
        if (href != null) {
            // Offline bundle: the bytes live at assets/<file>; link straight to them.
            an = el("a", "aname adl", h.att_name || "attachment");
            an.href = href;
            an.download = h.att_name || "attachment";
            an.title = "download";
        } else if (text != null || datauri != null) {
            an.classList.add("adl");
            an.title = "download";
            an.onclick = function () {
                var url = datauri != null ? datauri
                    : URL.createObjectURL(new Blob([text], { type: "text/plain" }));
                var link = el("a");
                link.href = url; link.download = h.att_name || "attachment";
                document.body.appendChild(link); link.click(); link.remove();
                if (datauri == null) setTimeout(function () { URL.revokeObjectURL(url); }, 1000);
            };
        } else if (path != null) {
            an.classList.add("adl");
            an.title = "reveal in file manager";
            an.onclick = function () { fetch("__reveal?path=" + encodeURIComponent(path)); };
        }
        ac.appendChild(an);
        // #16: a PLAN's body renders inline, expandable — served pages embed the text
        // (att_text); an offline bundle fetches its materialized assets/ file on first
        // toggle. A portable single-file export stays name-only by design.
        if (h.att_kind === "plan" && (text != null || href != null)) {
            var pv = el("button", "morebtn planview", "\u25b8 view plan");
            var pb = el("pre", "plan-body");
            if (text != null) pb.textContent = text;
            var fetched = text != null;
            pv.onclick = function (ev) {
                ev.stopPropagation();
                if (!fetched) {
                    fetched = true;
                    fetch(href).then(function (r) { return r.text(); })
                        .then(function (t) { pb.textContent = t; })
                        .catch(function () { pb.textContent = "(could not load " + href + ")"; });
                }
                var openNow = pb.classList.toggle("shown");
                pv.textContent = openNow ? "\u25be hide plan" : "\u25b8 view plan";
            };
            ac.appendChild(pv);
            ac.appendChild(pb);
        }
        // Show images inline. A served/exported page carries the bytes as a data: URI; an
        // offline bundle materializes them to assets/<file> and links via `att_href` — both
        // are valid <img> sources, so the bundle shows the image too (not just a download).
        var imgsrc = datauri != null ? datauri
            : (h.att_kind === "image" && href != null ? href : null);
        if (imgsrc != null) {
            var img = el("img", "aimg");
            img.src = imgsrc; img.alt = h.att_name || "image";
            // #139: inline images are capped at 520px, which for a screenshot means
            // unreadable. Click opens it at full size — the bytes are already here, so
            // this replaces "download it, then open Preview".
            img.title = "Click to view full size";
            ac.appendChild(img);
        }
        return ac;
    }

    // A queued (in-flight) mid-turn prompt not yet picked up by the agent — a dim,
    // always-open "⧗ queued:" marker. Not a turn (no sidebar entry, no clamp).
    if (b.kind === "queue") {
      var qc = el("div", "qmarker blk");
      qc.id = b.id;
      qc.appendChild(el("span", "qcaret", "⧗ queued:"));
      var qmd = el("div", "qmd");
      body.forEach(function (p) { renderPart(p, qmd); });
      qc.appendChild(qmd);
      return qc;
    }

    // Assistant prose — always open, no fold chrome.
    if (b.kind === "assistant") {
      var phase = b.phase ? " phase-" + b.phase : "";
      var ab = el("div", "ablock blk" + phase);
      ab.id = b.id;
      if (b.phase) ab.title = "assistant " + b.phase;
      ab.appendChild(el("span", "adot"));
      var prose = el("div", "prose");
      body.forEach(function (p) { renderPart(p, prose); });
      ab.appendChild(prose);
      return ab;
    }

    // Everything else is a fold.
    var isCmd = b.kind === "command";
    var f = el("div", "fold blk" + (isCmd ? " uturn" : ""));
    f.id = b.id;
    f.dataset.kind = b.kind;
    if (b.tool) f.dataset.tool = b.tool; // drives the tool-use filter
    f.dataset.open = b.open ? "1" : "0";
    if (b.turn != null) {
      f.dataset.turn = b.turn;
      f.dataset.label = b.label;
    }

    var h = el("div", "fold-h");
    h.tabIndex = 0;
    h.setAttribute("role", "button");
    h.setAttribute("aria-expanded", b.open ? "true" : "false");
    h.appendChild(el("span", "chev", "▸"));

    if (isCmd) {
      h.appendChild(el("span", "caret", "❯"));
      h.appendChild(el("span", "cmd-badge", head.badge));
      h.appendChild(el("span", "cmd-preview", head.preview || ""));
      chips(head, h);
      if (b.ts) h.appendChild(el("span", "ts", fmtTime(b.ts)));
    } else if (head.summary) {
      h.appendChild(el("span", "summary", head.summary));
      chips(head, h);
    } else if (head.badge) {
      // A sub-agent spawn/completion: an agent-hued dot + the "Agent" badge + a
      // "type: description" preview + the status chip (launched / completed / …).
      h.appendChild(el("span", "tool-dot"));
      h.appendChild(el("span", "tool-name", head.badge));
      if (head.preview) h.appendChild(el("span", "tool-target", head.preview));
      chips(head, h);
      // Cross-agent navigation (multi-file bundles): the id is a link to the agent's
      // own stream. A full page load carries the new `?session=`.
      if (head.child) {
        var alink = el("a", "agent-open", "↵ " + (head.child_id || "open"));
        alink.href = head.child;
        alink.title = "Open this agent's transcript (this tab)";
        h.appendChild(alink);
        var ant = el("span", "agent-newtab", "⧉");
        ant.title = "Open in a new tab";
        ant.dataset.href = head.child;
        h.appendChild(ant);
      }
    } else {
      if (head.dot) h.appendChild(el("span", "tool-dot"));
      if (head.name) h.appendChild(el("span", "tool-name", head.name));
      if (head.target) {
        if (head.path) {
          // A file-acting tool: clicking the path reveals the file. On a served
          // (live) page the click hits the local /__reveal endpoint (browsers
          // block http→file:// navigation); a standalone file:// page follows the
          // native file:// link. Clicking elsewhere on the header still folds.
          var a = el("a", "tool-path", head.target);
          a.href = "file://" + head.path.split("/").map(encodeURIComponent).join("/");
          a.target = "_blank";
          a.rel = "noopener";
          a.dataset.path = head.path;
          a.title = "Reveal " + head.path;
          h.appendChild(a);
        } else {
          h.appendChild(el("span", "tool-target", head.target));
        }
      }
      chips(head, h);
    }
    h.appendChild(anchor(b.id));
    f.appendChild(h);

    var fb = el("div", "fold-b");
    body.forEach(function (p) { renderPart(p, fb); });
    f.appendChild(fb);
    // §8.2 an authored-open fold emits its header target in the expanded (pre-wrap)
    // form immediately; setFold keeps it in sync on every later toggle.
    if (b.open) {
      var tgt = h.querySelector(":scope > .tool-target, :scope > .tool-path");
      if (tgt) {
        tgt.style.whiteSpace = "pre-wrap";
        tgt.style.overflow = "visible";
        tgt.style.textOverflow = "clip";
        tgt.style.overflowWrap = "anywhere";
      }
    }
    return f;
  }

  // ── #53 synthesized Back for ⧉ new-tab children ──────────────────────
  // A child opened in a NEW tab starts with empty history: Back is dead, and the
  // way home is scrolling to the breadcrumb. When this page is a fresh-history tab
  // in a multi-agent bundle AND knows its parent (the meta's ancestors), rewrite
  // the lone history entry to the parent's URL and push the child back on — so
  // Back IS the breadcrumb. Real same-tab history is left untouched (no doubled
  // Back stop); a deep-linked child with no ancestry keeps its dead Back.
  var renderedSession = null; // the ?session this document rendered (multi only)
  var historySynth = false;
  function synthesizeBack(ancestors) {
    if (historySynth || !multi || history.length > 1) return;
    if (!ancestors || !ancestors.length) return;
    var parent = ancestors[ancestors.length - 1];
    try {
      var childUrl = location.href;
      history.replaceState(null, "", "?session=" + encodeURIComponent(parent.id));
      history.pushState(null, "", childUrl);
      historySynth = true;
    } catch (e) { /* sandboxed / file:// — leave Back as-is */ }
  }
  window.addEventListener("popstate", function () {
    // Back/Forward across the synthesized entries: agent switches are full page
    // loads in this viewer, so reload whenever the URL's session differs from the
    // one this document rendered.
    var cur = new URLSearchParams(location.search).get("session") || document.body.dataset.root;
    if (renderedSession != null && cur !== renderedSession) location.reload();
  });

  // A session id is a uuid; its first group identifies it among a machine's sessions
  // without eating the bar. Codex wraps that uuid in `rollout-<datetime>-<uuid>`, so
  // find a trailing uuid before shortening instead of rendering every Codex id as
  // the identical `rollout-`. The full value stays in the title and on the clipboard.
  function snipId(s) {
    var uuid = s.match(/(?:^|-)([0-9a-f]{8})-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i);
    return uuid ? uuid[1] : (s.length > 12 ? s.slice(0, 8) : s);
  }

  // #139: show an image at full size over the page. Built on demand and torn down on
  // close, so a session with hundreds of screenshots carries no standing DOM for them.
  function lightbox(src, alt) {
    var box = el("div", "lightbox");
    var img = el("img");
    img.src = src;
    img.alt = alt || "";
    box.appendChild(img);
    var cap = el("div", "lb-cap", alt || "");
    box.appendChild(cap);
    function close() {
      box.remove();
      document.removeEventListener("keydown", onKey, true);
    }
    function onKey(ev) {
      if (ev.key === "Escape") { ev.stopPropagation(); close(); }
    }
    // Anywhere outside the image closes; the image itself is a safe place to click.
    box.addEventListener("click", function (ev) { if (ev.target !== img) close(); });
    document.addEventListener("keydown", onKey, true);
    document.body.appendChild(box);
  }

  // Total messages — what a reader means by the word: the top-level user and assistant
  // records. Tool calls, thinking and activity are already counted separately ("calls" in
  // the session bits). Counted CLIENT-side from the records the page already holds, so the
  // number is live, identical across dump/live/monitor, and costs no wire or stream change
  // (a SessionMeta counter would have to survive a durable resume, which means a fold-version
  // bump and a machine-wide cache rebuild — not worth it for a display row).
  function messageCount() {
    var n = 0;
    for (var i = 0; i < records.length; i++) {
      var k = records[i].kind;
      if (k === "user" || k === "assistant") n++;
    }
    return n;
  }
  // The value must track CONTENT, not meta refreshes: in the pull path renderMeta runs
  // before this reply's blocks are applied, so the row it draws is one tick stale — and an
  // idle tick carries no meta at all while a static page renders meta exactly once. This
  // runs after every apply (from postRender), updating the row in place — or CREATING it,
  // because on a no-usage session the first renderMeta saw zero records, drew an empty box,
  // and no later meta refresh ever comes on a quiet session.
  function refreshMessageRow() {
    var n = messageCount();
    var v = $("umsg-v");
    if (v) {
      v.textContent = String(n);
      return;
    }
    if (!n) return;
    var box = $("usage");
    if (!box) return;
    if (!box.children.length) box.appendChild(el("div", "side-head", "Usage"));
    var r = el("div", "urow");
    r.appendChild(el("span", null, "messages"));
    var val = el("span", null, String(n));
    val.id = "umsg-v";
    r.appendChild(val);
    box.insertBefore(r, box.children[1] || null); // first row, right under the head
  }

  function renderMeta(m) {
    if (m.title) {
      document.title = m.title;
      $("title").textContent = m.title;
    }
    // #127: which session, where, and how big now live in the fixed top bar, so they stay
    // readable at any scroll depth. The masthead below keeps the title and the duration —
    // showing the same four facts twice, both visible at once, is worse than either.
    var bar = $("sessionbits");
    bar.textContent = "";
    var sid = el("span", null, snipId(m.sid || ""));
    sid.id = "sid";
    sid.title = (m.sid || "") + " — click to copy transcript path";
    sid.dataset.path = m.path || "";
    bar.appendChild(sid);
    if (m.cwd) {
      var cwd = el("span", "sb-cwd", m.cwd);
      cwd.title = m.cwd;
      bar.appendChild(cwd);
    }
    var bits = [];
    if (m.turns != null) bits.push(m.turns + " turn" + (m.turns === 1 ? "" : "s"));
    if (m.tools != null) bits.push(m.tools + " call" + (m.tools === 1 ? "" : "s"));
    if (bits.length) bar.appendChild(el("span", "sb-counts", bits.join(" · ")));

    var meta = $("meta");
    meta.textContent = "";
    var d = fmtDur(m.duration_secs);
    if (d) meta.appendChild(el("span", null, d));

    var u = m.usage || {};
    var box = $("usage");
    box.textContent = "";
    // Some agents (QoderWork) emit a usage object that is ALL ZEROS with no model — no real
    // token accounting. Showing "0 tok / 0 tok" and no cost is noise, so omit the whole Usage
    // block when there is nothing to report. (`compacted` is a real signal even at 0 tokens —
    // it keeps the block, but not the token rows; see #152 below.)
    var hasTokens = (u.input && u.input !== "0") || (u.output && u.output !== "0") ||
                    (u.cache_read && u.cache_read !== "0") || u.cost;
    // #152's guard, SCOPED to the usage box it is about. It used to `return` out of the whole
    // meta renderer, which silently dropped everything rendered below it — the task panel, the
    // ancestor crumbs, the Back synthesis, and the Agents menu — for exactly the sessions the
    // guard targets: a QoderWork session that never compacted lost its 12 live todos and its
    // 7-child agent menu to a check about token rows.
    //
    // The message count keeps the box alive on its own: it is real information for every
    // session, including the no-usage agents the token guard exists for.
    var msgs = messageCount();
    if (hasTokens || u.compacted || msgs > 0) {
      box.appendChild(el("div", "side-head", "Usage"));
      var row = function (k, v, cls, vid) {
        var r = el("div", "urow" + (cls ? " " + cls : ""));
        r.appendChild(el("span", null, k));
        var val = el("span", null, v);
        if (vid) val.id = vid;
        r.appendChild(val);
        box.appendChild(r);
      };
      // Above the token rows: it exists for every agent where the tokens may not, and the
      // value cell is addressable so `refreshMessageRow` can track live growth in place.
      row("messages", String(msgs), null, "umsg-v");
      // #152: the token rows are drawn only when there ARE tokens. `compacted` keeps the
      // block alive (it is real information — "24× compacted, 4.0M dropped"), but it must not
      // drag three "0 tok" rows in with it: QoderWork never reports usage, so for its
      // compacted sessions — 17 of them here — the panel was three zeros and one fact.
      if (hasTokens) {
        row("input", (u.input || "0") + " tok");
        row("output", (u.output || "0") + " tok");
        row("cache read", (u.cache_read || "0") + " tok");
      }
      // #108: only sessions that actually compacted get a row, so the panel keeps its shape.
      // Without it the token totals look inexplicable beside a short-looking replay.
      if (u.compacted) row("compacted", u.compacted);
      if (u.cost) row("est. cost", u.cost, "total");
      // Credits-billed agents (Qoder): zero tokens, no USD — credits are the cost figure.
      if (u.credits) row("credits", u.credits, "total");
    }

    // Latest persisted runtime snapshot. These facts come from transcript records (not the
    // viewer's current process), so a historical export can retain the context/settings/limits
    // that Codex showed while the session was running.
    var rt = u.runtime || null;
    var rbox = $("runtime");
    rbox.textContent = "";
    if (rt) {
      rbox.appendChild(el("div", "side-head", "Runtime"));
      var rrow = function (k, v) {
        if (v == null || v === "") return;
        var r = el("div", "urow");
        r.appendChild(el("span", null, k));
        r.appendChild(el("span", null, String(v)));
        rbox.appendChild(r);
      };
      if (rt.context_left != null) {
        rrow("context", rt.context_left + "% left");
      } else if (rt.context_used_tokens && rt.context_window_tokens) {
        rrow("context", rt.context_used_tokens + " / " + rt.context_window_tokens);
      }
      rrow("effort", rt.effort);
      rrow("mode", rt.mode);
      rrow("sandbox", rt.sandbox);
      rrow("approvals", rt.approvals);
      rrow("permission", rt.permission);
      rrow("service tier", rt.tier);
      rrow("plan", rt.plan);
      var limitLabel = function (w) {
        if (!w) return null;
        var mins = w.window_minutes || 0;
        var span = mins % 10080 === 0 ? (mins / 10080) + "w" :
                   mins % 1440 === 0 ? (mins / 1440) + "d" :
                   mins % 60 === 0 ? (mins / 60) + "h" : mins + "m";
        var value = Math.round(w.used_percent) + "% used";
        if (w.resets_at) value += " · resets " + new Date(w.resets_at * 1000).toLocaleString();
        return [span + " limit", value];
      };
      [rt.primary, rt.secondary].forEach(function (w) {
        var label = limitLabel(w);
        if (label) rrow(label[0], label[1]);
      });
      if (rt.reached) rrow("limit reached", rt.reached);
    }

    renderTasks(m.tasks);
    renderCrumbs(m.ancestors);
    synthesizeBack(m.ancestors);
    // In a multi-agent tree (this session has children, or is itself a sub-agent) the box
    // is always shown — grayed on a childless leaf. A standalone session hides it entirely.
    var inTree = (m.children && m.children.length) || (m.ancestors && m.ancestors.length);
    renderAgentMenu(m.children, inTree);
  }

  // The session's task/todo panel (#15, a topbar dropdown since #70 — the sidebar
  // slot was too small to read): fed by the meta record (op-log state merged with
  // the live task files server-side). Each row: status glyph + #id + subject
  // (activeForm for the in-progress one); click toggles the description +
  // dependency details inline. The whole control hides when the session has no
  // tasks; the panel's open state and expanded rows survive live meta refreshes
  // (only the box's children are rebuilt).
  // #83 the floating task panel: finished → running → pending, IDs ascending in each
  // section. While auto-focus is engaged (default; ⌖ re-engages after a manual
  // scroll), every refresh re-centers the viewport on the MIDDLE of the running
  // section — so as the agent finishes tasks and picks up new ones, done items
  // visibly scroll up through the panel.
  var taskAutoFocus = true;
  function renderTasks(tasks) {
    var wrap = $("tasknav"), box = $("taskbox"), btn = $("btn-tasks");
    if (!wrap || !box || !btn) return;
    if (!tasks || !tasks.length) {
      wrap.style.display = "none";
      taskPanel(false);
      box.textContent = "";
      return;
    }
    // Preserve which details are expanded across live meta refreshes.
    var openIds = {};
    all("#taskbox .task-item.open").forEach(function (t) { openIds[t.dataset.tid] = 1; });
    wrap.style.display = "";
    box.textContent = "";
    var open = tasks.filter(function (t) { return t.status !== "Completed"; }).length;
    var label = btn.querySelector(".tf-label");
    if (label) label.textContent = "Tasks (" + open + " open) ▾";
    var title = $("tp-title");
    if (title) title.textContent = "Session tasks — " + open + " open";
    var byId = function (a, b) {
      var na = parseInt(a.id, 10), nb = parseInt(b.id, 10);
      if (!isNaN(na) && !isNaN(nb)) return na - nb;
      return String(a.id).localeCompare(String(b.id));
    };
    var done = tasks.filter(function (t) { return t.status === "Completed"; }).sort(byId);
    var run = tasks.filter(function (t) { return t.status === "InProgress"; }).sort(byId);
    var pend = tasks.filter(function (t) {
      return t.status !== "Completed" && t.status !== "InProgress";
    }).sort(byId);
    function item(t) {
      var it = el("div", "task-item" + (openIds[t.id] ? " open" : ""));
      it.dataset.tid = t.id;
      var glyph = t.status === "Completed" ? "●" : t.status === "InProgress" ? "◐" : "○";
      var row = el("div", "task-row" + (t.status === "Completed" ? " done" : t.status === "InProgress" ? " active" : ""));
      row.appendChild(el("span", "task-glyph", glyph));
      row.appendChild(el("span", "task-id", "#" + t.id));
      // #125: a transcript that only ever UPDATED a task never saw its subject, and the
      // title is not recoverable — the tool result says "Updated task #5 status" and
      // nothing more. "(untitled)" read as a broken task; this says what is actually
      // true, beside the #id that already identifies the row.
      var subj = t.subject || "(no title recorded in this session)";
      if (t.status === "InProgress" && t.active_form) subj += " · " + t.active_form;
      row.appendChild(el("span", "task-subj", subj));
      it.appendChild(row);
      var det = el("div", "task-det");
      var deps = [];
      if (t.blocked_by && t.blocked_by.length) deps.push("blocked by " + t.blocked_by.join(", "));
      if (t.blocks && t.blocks.length) deps.push("blocks " + t.blocks.join(", "));
      if (deps.length) det.appendChild(el("div", "task-deps", deps.join(" · ")));
      det.appendChild(el("div", "task-desc", t.description || "(no recorded description)"));
      it.appendChild(det);
      return it;
    }
    function section(name, list, cls) {
      if (!list.length) return;
      box.appendChild(el("div", "task-sec", name));
      list.forEach(function (t) { box.appendChild(item(t)); });
    }
    section("Finished", done);
    section("Running", run);
    section("Pending", pend);
    if (taskAutoFocus) centerTasks();
  }
  // Scroll the panel so the middle of the running section sits mid-viewport; with no
  // running tasks, center the FINISHED/PENDING BOUNDARY — where work will resume
  // (#100). The "Pending" section header sits exactly there, since sections render
  // finished → running → pending; with nothing pending, the end of the list is the
  // boundary. Rect-based positions, not offsetTop: the items' offsetParent is the
  // FIXED PANEL (the nearest positioned ancestor), so offsetTop carries the head
  // bar's height as a constant error in #taskbox scroll coordinates — invisible when
  // the panel was tall, off-center at the #100 compact height.
  function centerTasks() {
    var box = $("taskbox"), panel = $("taskpanel");
    if (!box || !panel || !panel.classList.contains("on")) return; // hidden ⇒ rects are 0; reopening recenters
    var boxTop = box.getBoundingClientRect().top;
    // Layout position within the scrolled content — scroll-invariant.
    function at(elm) { return elm.getBoundingClientRect().top - boxTop + box.scrollTop; }
    var rows = all("#taskbox .task-row.active").map(function (r) { return r.parentElement; });
    var mid;
    if (rows.length) {
      var last = rows[rows.length - 1];
      mid = (at(rows[0]) + at(last) + last.getBoundingClientRect().height) / 2;
    } else {
      var secs = all("#taskbox .task-sec");
      var pendSec = secs.filter(function (s2) { return s2.textContent === "Pending"; })[0];
      mid = pendSec ? at(pendSec) + pendSec.getBoundingClientRect().height / 2 : box.scrollHeight;
    }
    box.scrollTop = Math.max(0, mid - box.clientHeight / 2);
  }

  // The breadcrumb bar: `↑ <parent> › <current>`. Navigation steps up ONE level — to the
  // session that spawned this one — so the affordance is an up-arrow to the immediate
  // parent, not a home icon (which would imply a jump straight to the main session that the
  // server can't always make: a directly-opened deep link only knows its immediate parent).
  // To climb several levels, step up one at a time. If this view was opened in a *new tab*
  // (via ⧉), clicking ↑ closes this tab and returns to the parent's already-open tab
  // (handled in the click router); otherwise the link just navigates this tab.
  function renderCrumbs(ancestors) {
    var nav = $("crumbs");
    if (!nav) return;
    nav.textContent = "";
    // #140: the crumb lives in the masthead, which scrolls away — and inside the monitor
    // the view is an iframe, so the browser's Back button is not a way up either. The
    // toolbar carries a persistent twin of the same step-up.
    var btn = $("btn-up");
    if (!ancestors || !ancestors.length) {
      nav.style.display = "none";
      if (btn) btn.style.display = "none";
      return;
    }
    var parent = ancestors[ancestors.length - 1]; // the immediate (spawning) parent
    if (btn) {
      btn.style.display = "";
      btn.dataset.parent = parent.id;
      btn.title = "Back to the parent session — " + (parent.title || parent.id);
    }
    var up = el("a", "crumb crumb-up", "↑ " + (parent.title || parent.id));
    up.href = "?session=" + encodeURIComponent(parent.id);
    up.dataset.parent = parent.id;
    up.title = "Back to the parent session — " + (parent.title || parent.id);
    nav.appendChild(up);
    nav.appendChild(el("span", "crumb-sep", "›"));
    nav.appendChild(el("span", "crumb crumb-cur", document.title || "current"));
    nav.style.display = "";
  }

  // The "Agents ▾" menu: this session's sub-agents, **active first then done**, each in
  // launch order (the server ships `children` in spawn order with a `running` flag). The
  // box is **always present** — a leaf sub-agent (no children of its own) shows it grayed
  // and non-interactive, so the control never appears/disappears between views. In a LIVE
  // session it shows even before any agent exists (#70): a spawn can arrive at any moment,
  // and a control materializing out of nowhere is more surprising than a grayed one.
  // Each item navigates to that agent's stream (click = this tab; the ⧉ icon = a new tab).
  function renderAgentMenu(children, inTree) {
    var wrap = $("agentnav"), items = $("agentitems"), btn = $("btn-agents");
    if (!wrap || !items || !btn) return;
    var show = inTree || !!document.body.dataset.poll;
    wrap.style.display = show ? "" : "none";
    if (!show) return;
    var n = (children && children.length) || 0;
    btn.classList.toggle("disabled", n === 0);
    var label = btn.querySelector(".tf-label");
    if (label) label.textContent = n ? "Agents (" + n + ") ▾" : "Agents ▾";
    if (!n) { agentMenu(false); items.textContent = ""; return; }
    items.textContent = "";
    var active = children.filter(function (c) { return c.running; });
    var done = children.filter(function (c) { return !c.running; });
    function section(label, list) {
      if (!list.length) return;
      items.appendChild(el("div", "agent-sec", label + " (" + list.length + ")"));
      list.forEach(function (c) {
        var href = "?session=" + encodeURIComponent(c.id);
        var a = el("a", "agent-item" + (c.running ? " running" : ""));
        a.href = href;
        a.appendChild(el("span", "agent-dot"));
        a.appendChild(el("span", "agent-name", c.title || c.id));
        if (c.type) a.appendChild(el("span", "agent-type", c.type));
        var nt = el("span", "agent-newtab", "⧉");
        nt.title = "Open in a new tab";
        nt.dataset.href = href;
        a.appendChild(nt);
        items.appendChild(a);
      });
    }
    section("Active", active);
    section("Completed", done);
  }

  // Append one turn to the sidebar (live sessions grow it). Keyed by the turn's
  // stable id: a record re-emitted through any feed (a provisional resend, a
  // resync) UPDATES its entry in place, so the sidebar can never show a turn
  // twice (#88).
  function addTurn(b) {
    var label = b.turn + " \u00b7 " + b.label;
    var exist = turnlist.querySelector('[data-t="' + b.id + '"]');
    if (exist) { exist.textContent = label; return; }
    var item = el("div", "side-item", label);
    item.dataset.t = b.id;
    item.tabIndex = 0;
    turnlist.appendChild(item);
  }

  // Append a compaction EPOCH tick to the sidebar (#108). Not a turn — it carries no
  // number and is styled as a seam — but it lets a session that compacted fifteen times
  // read as fifteen chapters instead of one flat list. Keyed by the block id like a turn,
  // so a re-emitted record updates in place rather than duplicating.
  function addEpoch(b) {
    var label = (b.head && b.head.summary) || "context compacted";
    var exist = turnlist.querySelector('[data-t="' + b.id + '"]');
    if (exist) { exist.textContent = label; return; }
    var item = el("div", "side-epoch", label);
    item.dataset.t = b.id;
    item.tabIndex = 0;
    turnlist.appendChild(item);
  }

  // Render every JSONL record we haven't yet. `consumed` counts *records*
  // (non-empty lines), not array indices — the inline snapshot and the polled
  // companion frame their newlines differently, so an index would misalign and
  // new lines would be silently skipped. Stop at the first line that won't parse
  // (a partial tail caught mid-append); the next poll retries it.
  // One record → DOM. Shared by the whole-text and delta consumers.
  //   meta  → repaint metadata;
  //   reset → drop rendered blocks from index `from` (a rewritten tail: a thinking block
  //           closing, a tool result landing, an activity coalescing — the following
  //           records re-render them);
  //   block → render + append.
  function applyRecord(obj) {
    if (obj.t === "meta") { renderMeta(obj); return; }
    if (obj.t === "reset") { resetFrom(obj.from); return; }
    if (obj.t !== "block") return;
    pushRecord(obj);
  }

  // After a batch of new records: rebuild the filter menu (from the records), refresh
  // the filter's hit map, and re-window (new tail records materialize if in range).
  function postRender() {
    refreshMessageRow();
    buildToolMenu();
    if (filter) computeFilterHits();
    prefix = null;
    updatePads();
    updateView();
  }

  // Whole-text, record-counter based: the inline snapshot and the single-file `-f`
  // companion (which re-fetches the whole file each poll) both replay the full text and
  // dedup by the `consumed` counter.
  function consume(text) {
    var recs = text.split("\n").filter(function (l) { return l.trim(); });
    while (consumed < recs.length) {
      var obj;
      try { obj = JSON.parse(recs[consumed]); } catch (e) { break; }
      consumed++;
      applyRecord(obj);
    }
    postRender();
  }

  // Drop records from stream index `from` onward (a rewritten tail), plus their
  // sidebar turn entries, so the re-emitted records rebuild them cleanly.
  function resetFrom(from) {
    if (records.length <= from) return;
    records.length = from;
    recHeights.length = from;
    recText.length = from;
    recSearchParts.length = from;
    recHit.length = from;
    // Ids of dropped records (incl. nested) leave the index; a full rebuild is
    // cheap and only runs on tail rewrites.
    idIndex = {};
    records.forEach(function (b, i) { indexIds(b, i); });
    turnlist.textContent = "";
    records.forEach(function (b) { if (b.turn != null) addTurn(b); else if (b.epoch) addEpoch(b); });
    prefix = null;
    if (loIdx > from) loIdx = from;
    if (hiIdx > from) hiIdx = from;
    matEls().forEach(function (e) { if (+e.dataset.idx >= from) e.remove(); });
    updatePads();
  }

  // ── pull-client transport (`/pull?session=&cursor=`) ───────────────────
  // The pull version of the live feed (vs the `/stream` byte-diff). The server returns a
  // self-describing PullReply with TWO zones — `committed` (permanent, append-only) and
  // `provisional` (the open turn: truncate-from + append) — keyed by a 4-number cursor
  // {epoch, committed_id, provisional_gen, provisional_index}. We are a CONTENT-BLIND client:
  // we apply committed appends and the provisional truncate/extend by position, never inspecting
  // a block. `pc` is our cursor; epoch 0 ⇒ the first pull resyncs. See `cache::stream` / serve.rs.
  var pc = { epoch: 0, committed: 0, gen: 0, index: 0 };
  function cursorStr() {
    return pc.epoch + "." + pc.committed + "." + pc.gen + "." + pc.index;
  }
  function putBlock(b) {
    pushRecord(b);
  }
  function consumePull(r) {
    // Idle tick (same epoch, both zones empty): nothing to do.
    if (r.epoch === pc.epoch && !r.committed.length && !r.provisional.length) return false;
    if (r.epoch !== pc.epoch) { resetFrom(0); pc.committed = 0; } // resync
    if (r.meta) renderMeta(r.meta);
    // A commit (or resync): committed grew — drop everything at/after committed_from, then append
    // the new permanent blocks. `committed_from <= pc.committed` always.
    if (r.committed.length) {
      resetFrom(r.committed_from);
      pc.committed = r.committed_from;
      for (var i = 0; i < r.committed.length; i++) { putBlock(r.committed[i]); pc.committed++; }
    }
    // Provisional: truncate to the committed prefix + provisional_from, then append the suffix
    // (a same-gen append keeps the prefix; a gen bump/commit sends provisional_from = 0 ⇒ replace).
    resetFrom(pc.committed + r.provisional_from);
    for (var j = 0; j < r.provisional.length; j++) putBlock(r.provisional[j]);
    pc.epoch = r.epoch;
    pc.gen = r.provisional_gen;
    pc.index = r.provisional_from + r.provisional.length;
    postRender();
    return true;
  }

  // ── type / tool filter ────────────────────────────────────────────────
  // Populate the dropdown with the distinct message types present: each tool by its
  // NAME (Read, Bash, Update, …) AND each non-tool fold kind (Agent, Thinking, Activity,
  // Command). `filter` is the CSS selector the chosen entry maps to. Rebuilt whenever
  // content changes (live sessions grow types).
  var KIND_LABEL = { agent: "Agent", think: "Thinking", act: "Activity", command: "Command", compaction: "Compaction" };
  // Expanded tree nodes in the dropdown (#94) — survives menu rebuilds on live growth.
  var mcpOpen = {};
  function buildToolMenu() {
    // Counted from the RECORDS (nested items included), not the DOM — the DOM only
    // holds the materialized window (#50).
    var entries = {}; // selector -> {label, count}
    // MCP calls group into ONE expandable tree (#94): MCP -> server -> tool, parsed
    // from mcp__<server>__<tool>; single-child nodes compress into their child.
    var mcp = { total: 0, servers: {} };
    eachFoldRec(function (b) {
      if (b.tool) {
        var m = /^mcp__(.+?)__(.+)$/.exec(b.tool);
        if (m) {
          mcp.total++;
          var srv = (mcp.servers[m[1]] = mcp.servers[m[1]] || { count: 0, tools: {} });
          srv.count++;
          srv.tools[m[2]] = (srv.tools[m[2]] || 0) + 1;
          return;
        }
        var sel = '.fold[data-tool="' + b.tool + '"]';
        (entries[sel] = entries[sel] || { label: b.tool, count: 0 }).count++;
      } else if (b.kind) {
        var ks = '.fold[data-kind="' + b.kind + '"]';
        (entries[ks] = entries[ks] || { label: KIND_LABEL[b.kind] || b.kind, count: 0 }).count++;
      }
    });
    var sels = Object.keys(entries).sort(function (a, b) {
      return entries[a].label.localeCompare(entries[b].label);
    });
    var box = $("toolitems");
    box.textContent = "";
    function row(sel, label, count, depth, twKey, tint) {
      var item = el("div", "tool-item" + (sel === filter ? " active" : "") + (depth ? " tool-sub" + depth : ""));
      item.dataset.sel = sel;
      item.dataset.label = label;
      item.tabIndex = 0;
      if (twKey) {
        var tw = el("span", "tool-tw", mcpOpen[twKey] ? "▾" : "▸");
        tw.dataset.tw = twKey;
        item.appendChild(tw);
      } else {
        item.appendChild(el("span", "dot" + (tint ? " dot-" + tint : "")));
      }
      item.appendChild(el("span", "tname", label));
      item.appendChild(el("span", "tool-count", String(count)));
      box.appendChild(item);
    }
    var servers = Object.keys(mcp.servers).sort();
    var toolSel = function (srv, tool) { return '.fold[data-tool="mcp__' + srv + '__' + tool + '"]'; };
    var srvSel = function (srv) { return '.fold[data-tool^="mcp__' + srv + '__"]'; };
    // The MCP root sorts into the flat list by label (#94 follow-ups): one "MCP"
    // entry, never compressed with its children; expanding walks server rows
    // (srv-tinted bullets) then tool rows.
    var renderMcp = function () {
      row('.fold[data-tool^="mcp__"]', "MCP", mcp.total, 0, "mcp");
      if (!mcpOpen["mcp"]) return;
      servers.forEach(function (srv) {
        var tools = Object.keys(mcp.servers[srv].tools).sort();
        if (tools.length === 1) {
          // A server with one tool compresses into a combined child row.
          row(toolSel(srv, tools[0]), srv + "/" + tools[0], mcp.servers[srv].count, 1, null, "srv");
        } else {
          row(srvSel(srv), srv, mcp.servers[srv].count, 1, "mcp/" + srv, "srv");
          if (mcpOpen["mcp/" + srv]) {
            tools.forEach(function (t) {
              row(toolSel(srv, t), t, mcp.servers[srv].tools[t], 2, null, "leaf");
            });
          }
        }
      });
    };
    var flatRows = sels.map(function (sel) {
      return { label: entries[sel].label, render: function () { row(sel, entries[sel].label, entries[sel].count, 0, null); } };
    });
    if (mcp.total > 0) flatRows.push({ label: "MCP", render: renderMcp });
    flatRows.sort(function (a, b) { return a.label.localeCompare(b.label); });
    flatRows.forEach(function (r) { r.render(); });
    $("btn-tools").disabled = sels.length === 0 && mcp.total === 0;
  }

  function toolMenu(open) { $("toolmenu").classList.toggle("on", open); }
  function agentMenu(open) { $("agentmenu").classList.toggle("on", open); }
  function taskPanel(open) {
    var m = $("taskpanel");
    if (!m) return;
    m.classList.toggle("on", open);
    if (open) {
      // Sit just below the top bar, whatever its current height.
      var bar = $("topbar");
      if (bar) m.style.top = Math.round(bar.getBoundingClientRect().bottom + 8) + "px";
      if (taskAutoFocus) centerTasks();
    }
  }

  // Does record `b` (or a nested item) match the filter selector's meaning? The two
  // selector shapes the menu emits are '.fold[data-tool="X"]' / '.fold[data-kind="k"]'.
  function parseFilterSel(sel) {
    var mp = /\[data-tool\^="([^"]+)"\]/.exec(sel);
    if (mp) return { toolPre: mp[1] }; // MCP tree nodes filter by name prefix (#94)
    var mt = /\[data-tool="([^"]+)"\]/.exec(sel);
    if (mt) return { tool: mt[1] };
    var mk = /\[data-kind="([^"]+)"\]/.exec(sel);
    if (mk) return { kind: mk[1] };
    return {};
  }
  function recMatch(b, want) {
    if (want.toolPre && b.tool && b.tool.indexOf(want.toolPre) === 0) return true;
    if (want.tool && b.tool === want.tool) return true;
    if (want.kind && b.kind === want.kind && !b.tool && isFoldRec(b)) return true;
    var parts = b.body || [];
    for (var i = 0; i < parts.length; i++) {
      if (parts[i].p !== "blocks") continue;
      var items = parts[i].items;
      for (var j = 0; j < items.length; j++) {
        if (recMatch(items[j], want)) return true;
      }
    }
    return false;
  }
  // Rebuild the per-record hit map for the active filter, and open every hit's fold
  // chain (record-level, so it holds for blocks materialized later too).
  function computeFilterHits() {
    var want = parseFilterSel(filter);
    for (var i = 0; i < records.length; i++) {
      var b = records[i];
      recHit[i] = !isTurnKind(b) && recMatch(b, want);
      if (recHit[i]) openFilterChain(b, want);
      if (isTurnKind(b) && isFoldRec(b)) b.open = 0; // collapse command turns
    }
    // A SPARSE-hit filter renders its visible set FULLY (#94): with every height
    // real, P() is exact and a precision jump (the one-hit case that used to land in
    // blank pads) cannot drift when estimates correct. Dense filters keep normal
    // windowing — hundreds of force-opened folds would freeze the renderer, and with
    // hits everywhere the estimate drift is not observable. updateView honors the
    // flag so scroll-driven windowing can't trim the full render back away.
    var visible = 0, nhits = 0;
    for (var fi = 0; fi < records.length; fi++) {
        if (!isHiddenRec(fi)) visible++;
        if (recHit[fi]) nhits++;
    }
    filterFull = nhits <= 50 && visible <= 400;
    updateFilterNav();
  }
  var filterFull = false;
  // ‹ › grey out when there is nothing to step between (#94).
  function updateFilterNav() {
    var n = filterHitIdxs().length;
    all(".tf-prev, .tf-next").forEach(function (e) {
      e.classList.toggle("disabled", n <= 1);
    });
  }
  function openFilterChain(b, want) {
    var direct = (want.tool && b.tool === want.tool) ||
      (want.toolPre && b.tool && b.tool.indexOf(want.toolPre) === 0) ||
      (want.kind && b.kind === want.kind && !b.tool && isFoldRec(b));
    var containsHit = false;
    (b.body || []).forEach(function (p) {
      if (p.p !== "blocks") return;
      p.items.forEach(function (c) { if (openFilterChain(c, want)) containsHit = true; });
    });
    if ((direct || containsHit) && isFoldRec(b)) b.open = 1;
    return direct || containsHit;
  }
  // Accent the hit headers of a materialized element: its own header if the record
  // matches directly, plus any nested matching folds.
  function markFilterHit(e) {
    var want = parseFilterSel(filter);
    var sel = want.toolPre
      ? '.fold[data-tool^="' + want.toolPre + '"]'
      : want.tool
        ? '.fold[data-tool="' + want.tool + '"]'
        : '.fold[data-kind="' + want.kind + '"]:not([data-tool])';
    var targets = [];
    if (e.matches(sel)) targets.push(e);
    Array.prototype.forEach.call(e.querySelectorAll(sel), function (m) { targets.push(m); });
    targets.forEach(function (m) {
      var h = m.querySelector(":scope > .fold-h");
      if (h) h.classList.add("filter-hit");
    });
  }

  // #49 prev/next through the filtered hits. `filterCurId` is the current hit's
  // record id — id-keyed so the emphasis survives rematerialization (matBlock
  // re-applies it); navigation wraps like search's.
  var filterCurId = null;
  function filterHitIdxs() {
    var out = [];
    for (var i = 0; i < records.length; i++) if (recHit[i]) out.push(i);
    return out;
  }
  function setFilterCur(id) {
    filterCurId = id;
    all(".fold-h.filter-cur").forEach(function (h) { h.classList.remove("filter-cur"); });
    if (!id) return;
    var e = document.getElementById(id);
    if (e) {
      var h = e.querySelector(":scope > .fold-h");
      if (h) h.classList.add("filter-cur");
    }
  }
  // The hit nearest the current viewport top (record math — hits may be
  // unmaterialized), used for the jump-on-apply and as the nav starting point.
  function nearestHitIdx() {
    var hits = filterHitIdxs();
    if (!hits.length) return -1;
    var p = P(), y = window.scrollY - streamTop();
    var best = 0, bestD = Infinity;
    for (var k = 0; k < hits.length; k++) {
      var d = Math.abs(p[hits[k]] - y);
      if (d < bestD) { bestD = d; best = k; }
    }
    return best;
  }
  function filterNav(dir) {
    var hits = filterHitIdxs();
    if (!hits.length) return;
    if (dir !== 0 && hits.length < 2) return; // nothing to step between (#94)
    var pos;
    if (filterCurId != null && idIndex[filterCurId] != null) {
      var cur = idIndex[filterCurId];
      pos = 0;
      for (var k = 0; k < hits.length; k++) if (hits[k] === cur) { pos = k; break; }
      pos = (pos + dir + hits.length) % hits.length; // wraps
    } else {
      pos = nearestHitIdx();
    }
    var id = records[hits[pos]].id;
    goToId(id);
    setFilterCur(id);
  }

  // Enter/leave/toggle the filter. `sel` is a selector; re-selecting the active one clears.
  function setFilter(sel, label) {
    if (sel === filter) sel = null;
    if (sel && !filter) {
      // Snapshot every fold's open state (records, nested included) so Clear restores it.
      savedFolds = {};
      eachFoldRec(function (b) { if (b.id) savedFolds[b.id] = b.open ? "1" : "0"; });
    }
    filter = sel;
    if (!sel) {
      setFilterCur(null);
      // Re-anchor to CONTENT, not the absolute offset: while filtered most records are
      // hidden, so the document is much shorter and scrollY means something different.
      var anchorId = null, anchorTop = 0;
      matEls().some(function (e) {
        var r = e.getBoundingClientRect();
        if (r.bottom > 0) { anchorId = e.id; anchorTop = r.top; return true; }
        return false;
      });
      for (var i = 0; i < recHit.length; i++) recHit[i] = false;
      if (savedFolds) {
        eachFoldRec(function (b) {
          if (b.id && savedFolds[b.id] !== undefined) b.open = savedFolds[b.id] === "1" ? 1 : 0;
        });
      }
      prefix = null;
      refreshWindow();
      if (anchorId != null && idIndex[anchorId] != null) {
        var ti = idIndex[anchorId];
        window.scrollTo({ top: streamTop() + P()[ti] - anchorTop });
        updateView();
      }
    } else {
      computeFilterHits();
      prefix = null;
      refreshWindow();
      updateView();
      // #49: land on the nearest hit so the filter visibly did something (the
      // viewport could otherwise sit amid dimmed/hidden content with no hit).
      filterCurId = null;
      filterNav(0);
    }
    all(".tool-item").forEach(function (ti2) {
      ti2.classList.toggle("active", ti2.dataset.sel === filter);
    });
    // The button becomes "<label> ✕": the label opens the menu, the ✕ clears.
    $("btn-tools").classList.toggle("active", !!filter);
    document.querySelector("#btn-tools .tf-label").textContent = filter ? label : "Filter ▾";
    spy();
  }

  // ── follow-the-bottom (live tail UX) ──────────────────────────────────
  // Are we scrolled to (near) the end of the page?
  var BOTTOM_SLACK = 80;
  function atBottom() {
    return window.innerHeight + window.scrollY >= document.body.scrollHeight - BOTTOM_SLACK;
  }
  // #103: pin ACQUISITION is stricter than pin HOLDING. Reading the last message
  // naturally sits within BOTTOM_SLACK, and a scroll ending there used to acquire
  // the pin silently — then any provisional reshape (a duration tick, a result
  // back-patch: no visible new message) yanked the page to the exact bottom with
  // the badge suppressed. Acquire only at the true end — browsers clamp a
  // scrolled-to-the-end position to scrollHeight, so wheel, trackpad and scrollbar
  // all genuinely reach it. The generous slack still governs holding and healing,
  // which the virtualizer's pad shifts need (#88/#89).
  var PIN_SLACK = 2;
  function atEnd() {
    return window.innerHeight + window.scrollY >= document.body.scrollHeight - PIN_SLACK;
  }
  // Whether the view is PINNED to the live tail. An explicit mode, not inferred from
  // pixel proximity each tick (#88): under the virtualizer, materializing the tail
  // corrects estimated heights and silently moves the true bottom away from the
  // viewport — proximity-based following then unlatches on its own.
  //
  // Who may flip it (#89): ONLY the user. A scroll event within USER_MS of real
  // input (wheel, keys, pointer — incl. scrollbar drags —, touch) is the user
  // moving: position decides (at the bottom ⇒ pin, away ⇒ unpin). Every other
  // scroll — ours, or the BROWSER's own (scroll-anchoring adjustments and
  // clamp-on-shrink fire the same event with no marker) — carries no intent:
  // while pinned it is displacement to heal with a re-pin, never a state change.
  var following = false;
  // #103: the pin state is VISIBLE — body.following drives the #livechip — so a
  // page that moves by itself is never a mystery.
  function setFollowing(v) {
    following = v;
    document.body.classList.toggle("following", v);
  }
  var USER_MS = 300;
  // Sentinel far in the past: performance.now() is small right after load, so a 0
  // init would classify the load sequence's own scrolls (and the browser's async
  // scroll restoration) as user input and wrongly unpin the fresh page (#89).
  var lastUserInput = -1e9;
  ["pointerdown", "wheel", "keydown", "touchstart", "touchmove"].forEach(function (ev) {
    window.addEventListener(ev, function (e) {
      // Typing in the search box is not scroll intent.
      if (ev === "keydown" && e.target && /^(INPUT|TEXTAREA)$/.test(e.target.tagName)) return;
      lastUserInput = performance.now();
    }, { passive: true, capture: true });
  });
  function toBottom() {
    // Two-step: the jump materializes the tail window (real heights replace
    // estimates, pads shift), so re-read the height and correct. A smooth scroll
    // is not survivable here — any DOM mutation under it cancels the animation.
    window.scrollTo({ top: document.body.scrollHeight });
    updateView();
    if (!atBottom()) window.scrollTo({ top: document.body.scrollHeight });
  }
  // Displacement is a HEIGHT signal, not a scroll signal (#89): late reflows —
  // fonts arriving, images sizing, estimate-vs-real pad shifts — grow the page
  // below the viewport WITHOUT firing any scroll event, silently parking a pinned
  // view above the tail. Observe the body: any size change while pinned that
  // leaves the bottom is healed on the spot. (toBottom moves scroll, not size —
  // no feedback loop.)
  if (window.ResizeObserver) {
    new ResizeObserver(function () {
      if (following && !atBottom()) toBottom();
    }).observe(document.body);
  }
  // The pre-apply viewport anchor (#89): while unpinned, a content apply must not
  // shift what the reader is looking at — capture the first on-screen materialized
  // element, and afterwards put it back at the exact same viewport offset (the tail
  // rewrite dropped measured heights back to estimates below it; without this the
  // resulting pad shifts + the browser's own anchoring walk the page around).
  function captureAnchor() {
    if (following) return null;
    var a = null;
    matEls().some(function (e) {
      var r = e.getBoundingClientRect();
      if (r.bottom > 0 && e.id) { a = { id: e.id, top: r.top }; return true; }
      return false;
    });
    return a;
  }
  function restoreAnchor(a) {
    if (!a) return;
    var e = document.getElementById(a.id);
    if (!e) return; // the anchor was inside the rewritten tail — nothing stable to hold
    var d = e.getBoundingClientRect().top - a.top;
    if (Math.abs(d) > 1) window.scrollBy(0, d);
  }
  // Shared apply epilogue: settle the viewport (pin or anchor), then refresh the
  // spy at the FINAL position — a rewrite that nets zero new records still rebuilt
  // the sidebar, and without this the active-turn highlight silently vanished.
  function settleAfterApply(anchor, added) {
    if (pendingRestore) { // the first apply: land where we left off, not where we are
      applyPendingRestore();
      spy();
      return;
    }
    if (following) {
      toBottom();
      clearNew();
    } else {
      restoreAnchor(anchor);
      if (added > 0) showNew(added);
    }
    spy();
  }
  var newCount = 0;
  var badge = $("newbadge");
  // The pill answers ONE question — "how do I get back down" — and it has that answer whenever
  // the view is scrolled up, not only when something arrived (#171). At the bottom there is
  // nothing to offer, so it hides; scrolled up with arrivals, the count is the more useful
  // wording and already implies the jump.
  var badgeWant = null; // what the pill currently says, or null while hidden
  function paintBadge() {
    if (!badge) return; // called from `updateView`, which can run before this is wired
    var want = atBottom() ? null
      : newCount ? "↓ " + newCount + " new message" + (newCount === 1 ? "" : "s")
      : "↓ Jump to bottom";
    if (want === badgeWant) return; // scroll fires constantly — only touch the DOM on a change
    badgeWant = want;
    if (want === null) {
      badge.classList.remove("on");
      return;
    }
    badge.textContent = want;
    badge.classList.add("on");
  }
  function showNew(n) {
    newCount += n;
    paintBadge();
  }
  function clearNew() {
    newCount = 0;
    paintBadge();
  }
  badge.addEventListener("click", function () {
    setFollowing(true);
    toBottom();
    clearNew();
  });

  // ── the same view you left, across a reload (#170) ──────────────────────────────
  // The monitor reloads this page on every session switch, so "where I was" cannot live
  // in memory. It parks in sessionStorage under the session id: per tab, gone when the
  // tab closes, and never shared between two sessions.
  //
  // Three facts, because a view that LOOKS the same needs all three: where the viewport
  // was, what was folded, and how much had been read (so the badge counts what arrived
  // while you were away, not everything below you).
  var VS_KEY = "cr:view:" +
    (new URLSearchParams(location.search).get("session") || document.body.dataset.root || "");
  var pendingRestore = null;
  function saveView() {
    try {
      var a = captureAnchor(); // null while following — then the tail IS the position
      sessionStorage.setItem(VS_KEY, JSON.stringify({
        v: 1, following: following, anchor: a && a.id, dy: a ? Math.round(a.top) : 0,
        y: Math.round(window.scrollY), // coarse fallback, for when the anchor is gone
        folds: userFolds, seen: records.length
      }));
    } catch (e) { /* private mode or quota: the view just starts fresh */ }
  }
  function loadView() {
    try {
      var st = JSON.parse(sessionStorage.getItem(VS_KEY));
      return st && st.v === 1 ? st : null;
    } catch (e) { return null; }
  }
  // `pagehide` covers the monitor swapping the iframe's src, a reload, and a close;
  // `visibilitychange` covers a tab switch that never unloads the document.
  window.addEventListener("pagehide", saveView);
  document.addEventListener("visibilitychange", function () { if (document.hidden) saveView(); });

  // Seed the fold overrides BEFORE anything is applied: `pushRecord` runs `applyUserFolds`
  // over every record it builds (#61), so restoring the map here IS the fold restore —
  // and it happens without opening anything, which `goToId` would.
  if (!location.hash) { // a deep link is an explicit destination; it wins
    var vs0 = loadView();
    if (vs0) {
      if (vs0.folds) userFolds = vs0.folds;
      pendingRestore = vs0;
    }
  }

  // `goToId`'s landing WITHOUT its chain-open, and to a remembered offset rather than
  // GOTO_Y. Opening a fold to reveal the target is right for navigation and wrong for a
  // restore: it changes what the page looks like, which is the one thing a restore must
  // not do. Same convergence loop — the measure pass replaces estimated heights above the
  // target, so the first landing is only approximate.
  function landOn(id, dy) {
    var ti = idIndex[id];
    if (ti == null) return false;
    lastUserInput = performance.now(); // this is the user's own position, not displacement
    var y = streamTop() + P()[ti];
    setWindow(idxAt(y - streamTop() - MARGIN_PX),
              idxAt(y - streamTop() + window.innerHeight + MARGIN_PX) + 1);
    window.scrollTo({ top: Math.max(0, streamTop() + P()[ti] - dy) });
    updateView();
    for (var i = 0; i < 3; i++) {
      var t = document.getElementById(id);
      if (!t) break;
      var d = t.getBoundingClientRect().top - dy;
      if (Math.abs(d) <= 2) break;
      window.scrollBy(0, d);
      updateView();
    }
    return true;
  }

  // Consumed by the first apply, which is the earliest moment there is anything to land on.
  function applyPendingRestore() {
    var st = pendingRestore;
    pendingRestore = null;
    if (!st.following) {
      // A block id first: heights are estimates the virtualizer corrects on
      // materialization, so the same y maps to a different block on the next load — an id
      // does not drift. The raw offset is the fallback for a block that is no longer there
      // (or was never captured), and it beats the alternative of dumping the reader at the
      // tail, which is the one place they had already chosen not to be.
      var landed = st.anchor ? landOn(st.anchor, st.dy) : false;
      if (!landed && st.y != null) {
        lastUserInput = performance.now();
        window.scrollTo(0, st.y);
        updateView();
        landed = true;
      }
      if (landed) {
        setFollowing(false);
        // Everything that arrived while we were away is new — and nothing else is. This is
        // the fix for a fresh page counting the WHOLE session as unread.
        newCount = Math.max(0, records.length - (st.seen || 0));
        paintBadge();
        // …and once more when the layout has settled. The first apply lands before the
        // measure pass has replaced estimated heights, so `atBottom()` can still read TRUE
        // here and the pill would cache itself hidden until the next scroll.
        setTimeout(paintBadge, 0);
        return;
      }
    }
    setFollowing(true); // following when we left, or nothing left to land on — the tail
    toBottom();
  }

  // Initial render from the inlined snapshot, then drop the inline copy: the
  // rendered DOM is the source of truth now, so keeping ~1× the payload as script
  // text is pure waste (live mode re-reads from the companion, not this element).
  var inline = $("session-data");
  turnlist.textContent = "";
  var pollMs = parseInt(document.body.dataset.poll || "0", 10);
  var multi = document.body.dataset.multi;
  var kickFeed = null; // the active feed's poll-now hook (visibility kick, #89)

  // Render freshly consumed content, following/flagging the new tail. Shared by every
  // feed. Any change settles through the epilogue — pin or anchor, then spy (#89).
  function ingest(text) {
    var anchor = captureAnchor();
    var before = records.length;
    var beforeConsumed = consumed;
    consume(text);
    if (consumed > beforeConsumed) settleAfterApply(anchor, records.length - before);
  }

  if (multi) {
    // Multi-file bundle: this page shows ONE agent, `?session=<id>` (default `data-root`).
    // Navigation between agents is a full page load carrying a new `?session=`.
    if (inline) inline.remove();
    var sess = new URLSearchParams(location.search).get("session") || document.body.dataset.root;
    renderedSession = sess;
    // Transport: ONE pattern for every server-backed page (#85) — the pull protocol. A
    // static page is a pull client that pulls once (pollMs 0); a live page keeps polling.
    // Only the offline bundle (no server) fetches its flat `<id>.jsonl` instead.
    var usePull = document.body.dataset.pull === "1";
    if (usePull) {
      // Pull-client feed: poll `/pull?session=&cursor=` and apply the two-zone reply. The client
      // drives the tail (the server folds on our request), so an idle page costs the server nothing.
      // Committed arrives as a POINTER (`committed_ext: {offset, len}`) into the server's on-disk
      // record log; we range-read it via `/records` (phase two), then apply both zones atomically.
      // The inflight guard spans both fetches and the cursor advances only after both succeed; a
      // failed or 409 (stale-epoch after a reset) range read drops the whole reply — the next tick
      // re-pulls with the old cursor and the protocol resyncs us.
      var inflightP = false;
      var pullTimer = 0;
      // A reply the page cannot act on, said out loud. The pull loop's own `.catch` retries
      // quietly forever, which is right for a mid-write or a stale range and exactly wrong for
      // "this session is not servable" — that used to render as a blank page with no clue.
      var showFatal = function (msg) {
        var box = el("div", "ablock blk");
        box.appendChild(el("div", "blk-h", "This session is not being served here"));
        box.appendChild(el("div", "pre", String(msg || "no reason given")));
        stream.appendChild(box);
      };
      var pullTick = function () {
        if (inflightP) return;
        inflightP = true;
        fetch("pull?session=" + encodeURIComponent(sess) + "&cursor=" + cursorStr(), { cache: "no-store" })
          .then(function (r) { return r.json(); })
          .then(function (reply) {
            // Not a feed: this session lives on another server, or cannot be served at all.
            // A full navigation, never a transparent redirect — our cursor was minted against
            // THIS server's record stream and means nothing to another one.
            if (reply.t === "redirect") { clearInterval(pullTimer); location.replace(reply.url); return null; }
            if (reply.t === "error") { clearInterval(pullTimer); showFatal(reply.message); return null; }
            var ext = reply.committed_ext;
            if (!ext || !ext.len) { reply.committed = []; return reply; }
            return fetch("records?session=" + encodeURIComponent(sess) + "&from=" + ext.offset +
                         "&len=" + ext.len + "&epoch=" + reply.epoch, { cache: "no-store" })
              .then(function (rr) {
                if (!rr.ok) throw new Error("stale records"); // 409 ⇒ drop the reply, re-pull
                return rr.text();
              })
              .then(function (text) {
                reply.committed = text.split("\n").filter(function (l) { return l.trim(); }).map(JSON.parse);
                return reply;
              });
          })
          .then(function (reply) {
            if (!reply) return; // routed away, or nothing left to serve
            var anchor = captureAnchor();
            var before = records.length;
            var changed = false;
            try {
              changed = consumePull(reply);
            } catch (err) {
              // Self-heal (#54): a torn apply must never leave the page desynced — drop all
              // local state and cursor; the next tick resyncs from the server's canonical
              // state, exactly what a manual reload does.
              console.error("pull apply failed; resyncing", err);
              resetFrom(0);
              pc = { epoch: 0, committed: 0, gen: 0, index: 0 };
              return;
            }
            if (changed) settleAfterApply(anchor, records.length - before);
          })
          .catch(function () { /* server gone / mid-write / stale range — retry next tick */ })
          .finally(function () { inflightP = false; });
      };
      pullTick();
      if (pollMs > 0) pullTimer = setInterval(pullTick, pollMs);
      kickFeed = pullTick;
    } else {
      // Static bundle (served by any file server): fetch the whole stream file once.
      fetch(sess + ".jsonl", { cache: "no-store" })
        .then(function (r) { if (!r.ok) throw 0; return r.text(); })
        .then(function (t) { consume(t); spy(); })
        .catch(function () {
          stream.appendChild(el("div", "ablock blk", "No stream for “" + sess + "”."));
        });
    }
  } else if (inline) {
    consume(inline.textContent);
    inline.remove();
  }
  ["wheel", "touchmove", "pointerdown"].forEach(function (ev) {
    $("taskbox") && $("taskbox").addEventListener(ev, function () {
      if (!taskAutoFocus) return;
      taskAutoFocus = false;
      var tc = document.querySelector(".tp-center");
      if (tc) tc.classList.remove("autofocus");
    }, { passive: true });
  });

  // #101: upward drag-selection. The page scrolls on the WINDOW, and native selection
  // auto-scroll engages only at the viewport edge — but the fixed topbar occupies the
  // top of the viewport, so an upward drag reaches the bar (still inside the viewport)
  // and stalls there instead of scrolling. Downward needs nothing: the bottom edge is
  // bare, so the native behavior works and is not driven here. While a drag that
  // started in the content is live: body.selecting makes the chrome unselectable
  // (css), and a pointer inside the topbar band scrolls the window up at a rate
  // proportional to the intrusion (the editor-style ramp), re-extending the selection
  // to the point just below the band as content slides under the pointer.
  (function () {
    var live = false, lastX = 0, speed = 0, raf = 0;
    function bandBottom() {
      var bar = $("topbar");
      return (bar ? bar.getBoundingClientRect().bottom : 48) + 8;
    }
    // Extend the live selection to the caret nearest (x, y) — scrolling alone moves
    // content under a stationary pointer without growing the selection.
    function extendTo(x, y) {
      var sel = window.getSelection();
      if (!sel || !sel.rangeCount || !sel.extend) return;
      var node = null, off = 0;
      if (document.caretPositionFromPoint) {
        var p = document.caretPositionFromPoint(x, y);
        if (p) { node = p.offsetNode; off = p.offset; }
      } else if (document.caretRangeFromPoint) {
        var r = document.caretRangeFromPoint(x, y);
        if (r) { node = r.startContainer; off = r.startOffset; }
      }
      if (node) try { sel.extend(node, off); } catch (e) { /* non-Text hit — skip this frame */ }
    }
    // A 16 ms timer, not requestAnimationFrame: rAF freezes in hidden/occluded tabs,
    // which makes the loop untestable headless (this repo's tests drive the page in a
    // background tab) — and at ~1 frame's cadence the two are visually identical.
    function tick() {
      raf = setTimeout(function () {
        raf = 0;
        if (!live || !speed) return;
        // #103: this scroll IS the user moving — mark it, or the follow classifier
        // reads it as browser displacement and (while pinned) heals it straight
        // back to the bottom, fighting the drag.
        lastUserInput = performance.now();
        window.scrollBy(0, -speed);
        extendTo(lastX, bandBottom() + 2);
        tick(); // keep scrolling while the pointer rests in the band
      }, 16);
    }
    document.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      // Only drags that START in the content engage: a drag inside the task panel,
      // sidebar or a menu keeps its own scroll behavior (the v1.25.1 containment).
      if (e.target.closest("#topbar,#taskpanel,#sidebar,#toolmenu,#agentmenu")) return;
      live = true;
      document.body.classList.add("selecting"); // no preventDefault — clicks unaffected
    });
    document.addEventListener("mousemove", function (e) {
      if (!live) return;
      lastX = e.clientX;
      var into = bandBottom() - e.clientY;
      speed = into > 0 ? Math.min(4 + into * 0.6, 44) : 0;
      if (speed && !raf) tick();
    });
    function stop() {
      live = false; speed = 0;
      document.body.classList.remove("selecting");
    }
    document.addEventListener("mouseup", stop);
    window.addEventListener("blur", stop);
  })();

  // #100: the task panel's user resize. Native `resize: vertical` sets an INLINE height
  // when (and only when) the user drags the handle — content growth never does — so an
  // inline height is the "user sized it" signal: lift the ~5-row default (.user-sized)
  // and persist. Restore runs before the observer attaches, so applying the stored
  // height here doesn't loop.
  (function () {
    var panel = $("taskpanel");
    if (!panel) return;
    var TP_KEY = "cr-taskpanel-h";
    var stored = parseInt(lsGet(TP_KEY) || "", 10);
    if (stored > 0) {
      panel.style.height = stored + "px";
      panel.classList.add("user-sized");
    }
    if (typeof ResizeObserver === "undefined") return;
    var t = 0;
    new ResizeObserver(function () {
      if (!panel.style.height) return; // content reflow, not a user drag
      panel.classList.add("user-sized");
      clearTimeout(t);
      t = setTimeout(function () {
        var h = parseInt(panel.style.height, 10);
        if (h > 0) lsSet(TP_KEY, String(h));
        if (taskAutoFocus) centerTasks(); // keep ⌖ semantics at the new height
      }, 150);
    }).observe(panel);
  })();

  // §8.8/§8.6 apply persisted width mode and size the fixed bar to the window.
  setWide(wide);
  fitBar();

  // ── single-file live companion (`--dump-html -f`) ─────────────────────
  // No `/stream` endpoint here (the page is served flat, or `file://`), so re-fetch the
  // whole companion each cycle; `consume` skips already-rendered records.
  var src = document.body.dataset.src;
  if (!multi && src && pollMs > 0) {
    var failedC = false;
    var pollOnce = function () {
      if (failedC) return;
      fetch(src, { cache: "no-store" })
        .then(function (r) { return r.text(); })
        .then(ingest)
        .catch(function () { failedC = true; });
    };
    setInterval(pollOnce, pollMs);
    kickFeed = pollOnce;
  }
  // Background tabs throttle timers; on return, poll NOW instead of waiting out
  // the stretched interval (#89).
  document.addEventListener("visibilitychange", function () {
    if (!document.hidden && kickFeed) kickFeed();
  });

  // ── folds ────────────────────────────────────────────────────────────
  function setFold(f, open) {
    if (!f) return;
    f.dataset.open = open ? "1" : "0";
    // Persist to the record (#50): the DOM window is disposable, the record isn't.
    if (f.id) setRecordOpen(f.id, open);
    var h = f.querySelector(":scope > .fold-h");
    if (!h) return;
    h.setAttribute("aria-expanded", open ? "true" : "false");
    // §8.2 collapsed header target = one ellipsized line; expanded = pre-wrap. Set
    // inline so it's correct on every state change (init syncs all folds; renderBlock
    // also emits the expanded form for authored-open blocks).
    var t = h.querySelector(":scope > .tool-target, :scope > .tool-path");
    if (t) {
      t.style.whiteSpace = open ? "pre-wrap" : "nowrap";
      t.style.overflow = open ? "visible" : "hidden";
      t.style.textOverflow = open ? "clip" : "ellipsis";
      t.style.overflowWrap = open ? "anywhere" : "normal";
    }
  }
  // §8.8 Toggle with scroll anchoring + the foldin animation. Measures the header's
  // viewport top before/after and scrollBy the delta so the clicked row doesn't move;
  // if it would sit behind the sticky bars (<96px) ease it to 104. Bulk/programmatic
  // paths call setFold directly and skip anchoring.
  function toggleFold(f, open) {
    if (!f) return;
    if (f.id) userFolds[f.id] = open ? 1 : 0; // an explicit user gesture (#61)
    var h = f.querySelector(":scope > .fold-h");
    var y0 = h ? h.getBoundingClientRect().top : 0;
    setFold(f, open);
    var b = f.querySelector(":scope > .fold-b");
    if (open && b) { b.classList.remove("anim"); void b.offsetWidth; b.classList.add("anim"); }
    if (!h) return;
    var y1 = h.getBoundingClientRect().top;
    if (Math.abs(y1 - y0) > 1) window.scrollBy(0, y1 - y0);
    var top = h.getBoundingClientRect().top;
    if (top < 96) window.scrollBy({ top: top - 104, behavior: "smooth" });
  }
  function allFolds(open) {
    // Record-level (#50): applies to every fold in the session, materialized or not,
    // and pins each as a user override so live re-emission can't undo it (#61).
    eachFoldRec(function (b) {
      b.open = open ? 1 : 0;
      if (b.id) userFolds[b.id] = b.open;
    });
    refreshWindow();
  }

  // ── §8.3 per-pane code controls / §8.8 wide mode ─────────────────────────
  // Wrap each code/diff pane in `.codewrap` + a `.codefoot` row shared with the
  // "⋯ N more lines" expander: expander left, controls (A− size A+ wrap copy) right.
  // Static button styling lives in the stylesheet; only state goes on classes.
  function buildStripsIn(root_) {
    Array.prototype.slice.call(root_.querySelectorAll(".numbered, .diff")).forEach(function (c) {
      if (c.parentElement.classList.contains("codewrap")) return;
      var wrapEl = el("div", "codewrap");
      c.parentElement.insertBefore(wrapEl, c);
      wrapEl.appendChild(c);
      function b(cls, label, title) { var x = el("button", cls, label); x.title = title; return x; }
      var bar = el("div", "codebar");
      bar.appendChild(b("ms-dn", "A−", "Smaller code (−) — applies to all code blocks"));
      bar.appendChild(el("span", "ms-val", String(ms)));
      bar.appendChild(b("ms-up", "A+", "Larger code (+) — applies to all code blocks"));
      bar.appendChild(b("ms-wrap", wrap ? "⤶" : "↔", "Long lines: wrap / scroll (w)"));
      bar.appendChild(b("cpy-code", "copy", "Copy this block"));
      var foot = el("div", "codefoot");
      var next = wrapEl.nextElementSibling; // the "⋯ N more lines" expander, if any
      if (next && next.classList.contains("morebtn")) foot.appendChild(next);
      foot.appendChild(bar);
      wrapEl.appendChild(foot);
    });
  }
  function setMono(v) {
    ms = Math.max(8, Math.min(16, Math.round(v * 2) / 2));
    root.style.setProperty("--ms", ms + "px");
    all(".ms-val").forEach(function (n) { n.textContent = ms; });
    lsSet(MS_KEY, ms);
  }
  function setWrap(on) {
    wrap = on;
    lsSet(WRAP_KEY, on ? "1" : "0");
    all(".ms-wrap").forEach(function (b) {
      b.textContent = on ? "⤶" : "↔";
      b.title = on
        ? "Long lines: wrapping — click to scroll instead"
        : "Long lines: scrolling — click to wrap instead";
      b.classList.toggle("on", !on);
    });
    all(".numbered, .diff").forEach(function (c) {
      c.style.overflowX = on ? "hidden" : "auto";
      c.classList.toggle("scrollx", !on);
    });
    all(".numbered .code, .diff .code").forEach(function (c) {
      c.style.whiteSpace = on ? "pre-wrap" : "pre";
      c.style.wordBreak = on ? "break-word" : "normal";
    });
  }
  // The per-element form of setWrap's styling, for freshly materialized blocks (#50).
  function applyWrapIn(root_) {
    Array.prototype.slice.call(root_.querySelectorAll(".numbered, .diff")).forEach(function (c) {
      c.style.overflowX = wrap ? "hidden" : "auto";
      c.classList.toggle("scrollx", !wrap);
    });
    Array.prototype.slice.call(root_.querySelectorAll(".numbered .code, .diff .code")).forEach(function (c) {
      c.style.whiteSpace = wrap ? "pre-wrap" : "pre";
      c.style.wordBreak = wrap ? "break-word" : "normal";
    });
    Array.prototype.slice.call(root_.querySelectorAll(".ms-wrap")).forEach(function (b) {
      b.textContent = wrap ? "⤶" : "↔";
      b.classList.toggle("on", !wrap);
    });
  }
  function setWide(on) {
    wide = on;
    lsSet(WIDE_KEY, on ? "1" : "0");
    var lay = $("layout"), mn = $("main");
    if (lay) lay.style.maxWidth = on ? "none" : "1160px";
    if (mn) mn.style.maxWidth = on ? "none" : "820px";
    var b = $("btn-wide");
    if (b) {
      // #127: icon-only. The tinted state, not a word, says which mode is on.
      b.style.color = on ? "var(--tool)" : "";
      b.style.borderColor = on ? "var(--tool)" : "";
      b.title = on ? "Back to reading width" : "Wide mode — drop the reading-width cap for diff-heavy sessions";
    }
  }
  // §8.6 kept the bar from clipping its trailing control by shedding button labels as the
  // window narrowed. #127 removed the cause: two rows, and those buttons are icons at every
  // width. Only the version chip still earns its keep by stepping aside.
  function fitBar() {
    var bs = document.querySelector("#topbar .brand-sub");
    if (bs) bs.style.display = window.innerWidth < 820 ? "none" : "";
  }

  // Where goTo lands a target's top (px from the viewport top). `[`/`]` reference
  // this so a just-navigated turn isn't re-selected.
  var GOTO_Y = 120;
  // `instant` skips the smooth animation — a long-distance jump in the virtual list
  // would otherwise re-window on every animation frame (and lose the landing
  // element's transient state to churn); short local moves stay smooth.
  function goTo(target, instant) {
    if (!target) return;
    for (var p = target; p; p = p.parentElement) {
      if (p.classList && p.classList.contains("fold")) setFold(p, true);
    }
    var top = target.getBoundingClientRect().top + window.scrollY - GOTO_Y;
    window.scrollTo({ top: top, behavior: instant ? "auto" : "smooth" });
    target.classList.add("flash");
    setTimeout(function () { target.classList.remove("flash"); }, 1000);
  }
  // Navigate to a block id through the virtual layer (#50): open its record's fold
  // chain, materialize its region, then land on the element (nested ids included).
  function goToId(id) {
    var ti = idIndex[id];
    if (ti == null) return;
    // Every goToId is user-initiated navigation (sidebar, search, filter) — mark it
    // as intent so the follow classifier reads the jump's scrolls as the user moving
    // (position decides the pin), never as displacement to heal (#94).
    lastUserInput = performance.now();
    withChain(records[ti], id, function (n) { if (isFoldRec(n)) n.open = 1; });
    var y = streamTop() + P()[ti];
    setWindow(idxAt(y - streamTop() - MARGIN_PX), idxAt(y - streamTop() + window.innerHeight + MARGIN_PX) + 1);
    // The chain-open may have changed an already-materialized element — refresh it.
    var e0 = document.getElementById(records[ti].id);
    if (e0 && e0.dataset.idx != null) {
      var repl = matBlock(ti);
      e0.replaceWith(repl);
      postMat(repl);
      clampBatch([repl]);
      measureWindow();
      updatePads();
    }
    var target = document.getElementById(id);
    if (target) goTo(target, true);
    else window.scrollTo({ top: streamTop() + P()[ti] - GOTO_Y });
    // A landing at (nearly) the same y fires no scroll event — refresh the window
    // and the scrollspy explicitly.
    updateView();
    // Re-land exactly (#94): the post-jump measure pass replaces estimated heights
    // ABOVE the target with real ones — under a filter the shift can be thousands of
    // px, leaving the viewport in a pad. Correct against the target's REAL rect until
    // it converges (the region around it is fully measured after a pass or two).
    for (var gi = 0; gi < 3; gi++) {
      var t2 = document.getElementById(id);
      if (!t2) break;
      var d = t2.getBoundingClientRect().top - GOTO_Y;
      if (Math.abs(d) <= 2) break;
      window.scrollBy(0, d);
      updateView();
    }
    // Navigation SETS the pin state directly (#94): in a background tab the jump's
    // scroll events can deliver long after the intent window, and the classifier
    // would read them as displacement and yank the view back to the tail.
    // #103: acquisition needs the true end here too — landing NEAR the tail must
    // not pin (the same silent-pin trap as a near-bottom scroll).
    setFollowing(following ? atBottom() : atEnd());
    spy();
  }

  // §8.5 One clipboard helper for all call sites. Exports normally open from
  // file://, where navigator.clipboard is refused — so fall back to a hidden-textarea
  // execCommand and resolve success only when one path actually works. Never a false ✓.
  function copyText(text) {
    // Nothing to copy is a FAILURE, not a success: `writeText("")` resolves ok, which
    // turned a missing meta path into a false "copied transcript path" flash (#81).
    if (!text) return Promise.resolve(false);
    function legacy() {
      try {
        var ta = document.createElement("textarea");
        ta.value = text;
        ta.style.cssText = "position:fixed;top:-1000px;opacity:0";
        document.body.appendChild(ta);
        ta.select();
        var ok = document.execCommand("copy");
        ta.remove();
        return ok;
      } catch (e) { return false; }
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text).then(function () { return true; }).catch(legacy);
    }
    return Promise.resolve(legacy());
  }
  // Button feedback: "copied" / "blocked" (never a false success). A codebar button
  // (stylesheet-styled) uses the `.bad` class so :hover still wins; others go inline.
  function copyBtn(btn, text) {
    var was = btn.dataset.label || (btn.dataset.label = btn.textContent);
    var inBar = !!btn.closest(".codebar");
    copyText(text).then(function (ok) {
      clearTimeout(btn._t);
      btn.textContent = ok ? "copied" : "blocked";
      if (inBar) btn.classList.toggle("bad", !ok);
      else btn.style.color = ok ? "" : "var(--delfg)";
      btn._t = setTimeout(function () {
        btn.textContent = was;
        if (inBar) btn.classList.remove("bad");
        else btn.style.color = "";
      }, 1300);
    });
  }

  document.addEventListener("click", function (e) {
    // ── type/tool filter controls ──
    var tw = e.target.closest(".tool-tw");
    if (tw) {
      mcpOpen[tw.dataset.tw] = !mcpOpen[tw.dataset.tw];
      buildToolMenu();
      toolMenu(true);
      return;
    }
    var ti = e.target.closest(".tool-item");
    if (ti) { setFilter(ti.dataset.sel, ti.dataset.label); toolMenu(false); return; }
    if (e.target.closest(".tf-prev")) { filterNav(-1); return; } // ‹ previous hit (#49)
    if (e.target.closest(".tf-next")) { filterNav(1); return; }  // › next hit (#49)
    if (e.target.closest(".tf-x")) { setFilter(null); toolMenu(false); return; } // ✕ clears
    if (e.target.closest("#btn-tools")) { toolMenu(!$("toolmenu").classList.contains("on")); return; } // label opens menu
    // ── breadcrumb "↑ parent" ── if this view was opened in a new tab (so it has an
    // opener still showing the parent), return to parent = close this tab and refocus the
    // opener, rather than navigating a duplicate. Same-tab views (no opener) just follow the
    // link to the parent session.
    // The toolbar's persistent ↑ (#140) is the same step, so it shares this router.
    var upEl = e.target.closest(".crumb-up, #btn-up");
    if (upEl) {
      if (window.opener && !window.opener.closed) {
        e.preventDefault();
        try { window.opener.focus(); } catch (_) {}
        window.close();
        return;
      }
      // The crumb is an <a> and navigates itself; the toolbar button has no href.
      if (upEl.id === "btn-up" && upEl.dataset.parent) {
        location.href = "?session=" + encodeURIComponent(upEl.dataset.parent);
      }
      return;
    }
    // ── agents menu ── an item is an <a href> → let it navigate (this tab); the ⧉ icon
    // opens the same target in a new tab; the button toggles (unless disabled = no children).
    // Open WITHOUT `noopener` so the child keeps `window.opener` — that is what lets its
    // "↑ parent" close-and-return to this tab instead of opening yet another.
    var ant = e.target.closest(".agent-newtab");
    if (ant) { e.preventDefault(); e.stopPropagation(); window.open(ant.dataset.href, "_blank"); return; }
    if (e.target.closest(".agent-item")) return;
    if (e.target.closest("#btn-agents")) {
      if (!$("btn-agents").classList.contains("disabled")) agentMenu(!$("agentmenu").classList.contains("on"));
      return;
    }
    // ── tasks menu (#70) ── the button toggles; clicks inside the panel fall through
    // to the task-row expander below without closing it.
    if (e.target.closest("#btn-tasks")) {
      taskPanel(!$("taskpanel").classList.contains("on"));
      return;
    }
    // The floating task panel (#83): ⌖ re-engages auto-centering, ✕ closes; it does
    // NOT close on outside clicks — it is a persistent panel, not a dropdown.
    if (e.target.closest(".tp-center")) {
      taskAutoFocus = true;
      var tc = document.querySelector(".tp-center");
      if (tc) tc.classList.add("autofocus");
      centerTasks();
      return;
    }
    if (e.target.closest(".tp-x")) {
      taskPanel(false);
      return;
    }
    // Any other click closes an open dropdown.
    if (!e.target.closest("#toolmenu")) toolMenu(false);
    if (!e.target.closest("#agentmenu")) agentMenu(false);
    if (!e.target.closest(".qscopewrap")) scopeMenu(false);

    // #139: an inline image opens full size.
    var aimg = e.target.closest(".aimg");
    if (aimg) { lightbox(aimg.src, aimg.alt); return; }


    var sid = e.target.closest("#sid");
    if (sid) {
      var sorig = sid.dataset.label || (sid.dataset.label = sid.textContent);
      copyText(sid.dataset.path).then(function (ok) {
        clearTimeout(sid._t);
        sid.textContent = ok ? "copied transcript path" : "copy blocked — ⌘C the path";
        sid._t = setTimeout(function () { sid.textContent = sorig; }, 1400);
      });
      return;
    }
    var cpy = e.target.closest(".cpy");
    if (cpy) {
      var pre = cpy.closest(".fence").querySelector("pre");
      copyBtn(cpy, pre.textContent);
      return;
    }
    // §8.3 per-pane code controls (event-delegated).
    if (e.target.closest(".ms-dn")) { setMono(ms - 0.5); return; }
    if (e.target.closest(".ms-up")) { setMono(ms + 0.5); return; }
    if (e.target.closest(".ms-wrap")) { setWrap(!wrap); return; }
    var cc = e.target.closest(".cpy-code");
    if (cc) {
      var blk = cc.closest(".codewrap").querySelector(".numbered, .diff");
      var codeText = Array.prototype.map
        .call(blk.querySelectorAll(".code"), function (n) { return n.textContent; })
        .join("\n");
      copyBtn(cc, codeText);
      return;
    }
    // Clamp toggle on a long user turn: expand to full height, or re-collapse.
    var clamp = e.target.closest(".clampbtn");
    if (clamp) {
      var body = clamp.previousElementSibling;
      if (body.classList.contains("clamped")) {
        body.classList.remove("clamped");
        body.style.maxHeight = "";
        clamp.textContent = "▲ show less";
      } else {
        body.classList.add("clamped");
        body.style.maxHeight = clamp.dataset.cap + "px";
        clamp.textContent = clamp.dataset.more;
      }
      return;
    }
    var more = e.target.closest(".morebtn");
    if (more) {
      // #67: a SMALL expansion (content within MAX_BUFFER_LINES) is recorded by
      // record-id + ordinal so it survives rematerialization; large ones reset.
      var blk67 = more.closest(".blk");
      var hidden67 = $(more.dataset.more);
      if (blk67 && blk67.id && hidden67 && more.dataset.ord != null
          && hiddenLineCount(hidden67) <= MAX_BUFFER_LINES) {
        smallMore[blk67.id + ":" + more.dataset.ord] = true;
      }
      expandMore(more);
      return;
    }
    // §8.4 the `#` anchor COPIES a deep link — no scroll, no hash write, no fold
    // toggle. (Loading a URL that already has a hash still scrolls + expands.)
    var al = e.target.closest(".alink");
    if (al) {
      e.preventDefault();
      e.stopPropagation();
      var href = al.getAttribute("href");
      copyText(location.href.split("#")[0] + href).then(function (ok) {
        clearTimeout(al._t);
        al.textContent = ok ? "✓" : "⚠";
        al.style.opacity = "1";
        al.style.color = ok ? "var(--tool)" : "var(--delfg)";
        al.title = ok ? "Copy a link to this spot" : "Copy blocked — select the address bar and press ⌘C";
        al._t = setTimeout(function () {
          al.textContent = "#";
          al.style.opacity = "";
          al.style.color = "";
          al.title = "Copy a link to this spot";
        }, 1400);
      });
      return;
    }
    // A file path in a tool header reveals the file, and never folds the block.
    var tp = e.target.closest(".tool-path");
    if (tp) {
      if (location.protocol === "file:") return; // native file:// link works standalone
      e.preventDefault(); // served page: http→file:// is blocked, so ask the server
      var orig = tp.textContent;
      fetch("__reveal?path=" + encodeURIComponent(tp.dataset.path))
        .then(function (r) {
          tp.textContent = r.ok ? "revealed ✓" : "not found";
          setTimeout(function () { tp.textContent = orig; }, 1000);
        })
        .catch(function () { /* server gone */ });
      return;
    }
    // The agent-transcript link navigates (full page load to `?session=<id>`); let the
    // <a> do its thing instead of toggling the fold it sits in.
    if (e.target.closest(".agent-open")) return;
    var h = e.target.closest(".fold-h");
    if (h) { var f = h.closest(".fold"); toggleFold(f, f.dataset.open !== "1"); return; }
    var trow = e.target.closest(".task-row");
    if (trow) { trow.parentElement.classList.toggle("open"); return; }
    if (e.target.closest("#stickybar") && curTurn) { goToId(curTurn.id); return; }
    var si = e.target.closest(".side-item, .side-epoch");
    if (si) goToId(si.dataset.t);
  });

  var themeBtn = $("btn-theme");
  if (themeBtn) themeBtn.addEventListener("click", function () {
    var next = root.getAttribute("data-theme") === "light" ? "dark" : "light";
    try { localStorage.setItem(THEME_KEY, next); } catch (e) { /* ignore */ }
    applyTheme(next);
  });
  $("btn-exp").addEventListener("click", function () { allFolds(true); });
  $("btn-col").addEventListener("click", function () { allFolds(false); });
  var wideBtn = $("btn-wide");
  if (wideBtn) wideBtn.addEventListener("click", function () { setWide(!wide); });
  window.addEventListener("resize", function () { fitBar(); }, { passive: true });

  // ── search ───────────────────────────────────────────────────────────
  // Hits live on the RECORDS (`hitRecs`/`totalHits`, #50); the window's occurrences are
  // wrapped in <mark class="hl"> as their blocks materialize. Enter cycles the global
  // hit index, materializing + marking `.cur` on the way (Shift+Enter goes back).
  var q = $("q");
  var hitRecs = [];   // {rec, count, start} per record with hits, in stream order (#50)
  var totalHits = 0;
  function clearHl() {
    searchNeedle = "";
    searchScope = null;
    var touched = [];
    all("#stream mark.hl").forEach(function (m) {
      var p = m.parentNode;
      p.replaceChild(document.createTextNode(m.textContent), m);
      if (touched.indexOf(p) === -1) touched.push(p);
    });
    touched.forEach(function (p) { p.normalize(); }); // merge the split text nodes back
  }
  // Search scans the RECORDS' text (#50 — the DOM only holds the window). Text per
  // record is extracted once, lazily, in tree order. The same walk also builds one
  // projection per search class: an `act` owns only its thinking prose under `t`, while
  // each absorbed child tool owns its command/output under `o`/`b`/`r`/`e`.
  var stripDiv = null;
  function stripHtml(h) {
    if (!stripDiv) stripDiv = el("div");
    stripDiv.innerHTML = h;
    var t = stripDiv.textContent;
    stripDiv.textContent = "";
    return t;
  }
  function ownTextParts(b) {
    var parts = [], h = b.head || {};
    ["summary", "badge", "preview", "name", "target", "att_name"].forEach(function (k) {
      if (h[k]) parts.push(String(h[k]));
    });
    (b.body || []).forEach(function (p) {
      if (p.p === "md" || p.p === "think") parts.push(stripHtml(p.h));
      else if (p.p === "pre" || p.p === "note") parts.push(String(p.x));
      else if (p.p === "num") p.rows.forEach(function (r) { parts.push(stripHtml(String(r[1]))); });
      else if (p.p === "diff") p.rows.forEach(function (r) { parts.push(String(r[2])); });
    });
    return parts;
  }
  var CLASS_BIT = { u: 1, a: 2, t: 4, o: 8, b: 16, r: 32, e: 64 };
  function directMask(k) {
    if (k === "user" || k === "command") return CLASS_BIT.u;
    if (k === "assistant") return CLASS_BIT.a;
    if (k === "think" || k === "act") return CLASS_BIT.t;
    if (!/^(bash|edit|write|read|skill|tool)$/.test(k)) return 0;
    var mask = CLASS_BIT.o;
    if (k === "bash") mask |= CLASS_BIT.b;
    if (k === "read") mask |= CLASS_BIT.r;
    if (k === "edit" || k === "write") mask |= CLASS_BIT.e;
    return mask;
  }
  function scopeMask(set) {
    var mask = 0;
    scopeLetters(set).forEach(function (k) { mask |= CLASS_BIT[k]; });
    return mask;
  }
  function ensureRecText(i) {
    if (recText[i] != null && recSearchParts[i] != null) return;
    var allParts = [], ownership = [], length = 0;
    (function walk(b) {
      var own = ownTextParts(b).join("\n").toLowerCase();
      if (own) {
        if (allParts.length) { allParts.push("\n"); length++; }
        var start = length;
        allParts.push(own);
        length += own.length;
        ownership.push({ start: start, end: length, mask: directMask(b.kind) });
      }
      (b.body || []).forEach(function (p) {
        if (p.p === "blocks") p.items.forEach(walk);
      });
    })(records[i]);
    recText[i] = allParts.join("");
    recSearchParts[i] = ownership;
  }
  function textOfRec(i) {
    ensureRecText(i);
    return recText[i];
  }
  function countRec(i, set, lc, whole) {
    ensureRecText(i);
    var wanted = scopeMask(set);
    if (!wanted) return countOcc(recText[i], lc, whole);
    var n = 0;
    recSearchParts[i].forEach(function (part) {
      if (part.mask & wanted) n += countOcc(recText[i].slice(part.start, part.end), lc, whole);
    });
    return n;
  }
  var WORD_LEFT = /[\p{L}\p{N}\p{M}_]$/u;
  var WORD_RIGHT = /^[\p{L}\p{N}\p{M}_]/u;
  function wholeAt(t, start, len) {
    return !WORD_LEFT.test(t.slice(0, start)) && !WORD_RIGHT.test(t.slice(start + len));
  }
  function countOcc(t, lc, whole) {
    var n = 0, i = 0;
    while ((i = t.indexOf(lc, i)) !== -1) {
      if (!whole || wholeAt(t, i, lc.length)) n++;
      i += lc.length;
    }
    return n;
  }
  function kindInScope(k, set) {
    var wanted = scopeMask(set);
    return !wanted || !!(directMask(k) & wanted);
  }
  function markHits(blk, lc, len, whole) {
    // Collect matching text nodes first (the walk is read-only), then rewrite each so we
    // never mutate the tree we're walking. Matches within a single text node only — good
    // enough for a viewer, and it never splits across the pre-rendered highlight spans.
    var walker = document.createTreeWalker(blk, NodeFilter.SHOW_TEXT, null);
    var nodes = [], n;
    while ((n = walker.nextNode())) {
      var owner = n.parentElement && n.parentElement.closest(".blk");
      if ((!searchScope || (owner && kindInScope(owner.dataset.kind, searchScope)))
          && countOcc(n.nodeValue.toLowerCase(), lc, whole)) nodes.push(n);
    }
    nodes.forEach(function (tn) {
      var text = tn.nodeValue, lower = text.toLowerCase();
      var frag = document.createDocumentFragment(), i = 0, emitted = 0, idx;
      while ((idx = lower.indexOf(lc, i)) !== -1) {
        if (whole && !wholeAt(lower, idx, len)) { i = idx + len; continue; }
        if (idx > emitted) frag.appendChild(document.createTextNode(text.slice(emitted, idx)));
        var mk = el("mark", "hl");
        mk.textContent = text.slice(idx, idx + len);
        frag.appendChild(mk);
        emitted = idx + len;
        i = emitted;
      }
      if (emitted < text.length) frag.appendChild(document.createTextNode(text.slice(emitted)));
      tn.parentNode.replaceChild(frag, tn);
    });
  }
  // The `uatobrew:` scope grammar (same syntax as the TUI's `/` search,
  // case-insensitive): a run of DISTINCT letters — u (your turns: user+command),
  // a (agent replies), t (thinking, both think and act), o (ALL tools), b (bash),
  // r (reads), e (edits+writes), w (whole-word modifier) — then `:`, ORDER-FREE,
  // so `aut:` ≡ `uat:` and `tw:` means whole words in thinking prose (`+` still parses).
  // A LEADING colon escapes:
  // `:rate:limit` searches the literal `rate:limit`. Returns {set, len}; {set:null}
  // for the escape; null when the text has no prefix (repeats, foreign letters —
  // including the dropped `user:` alias — and colons in ordinary text like `http://`).
  function parseScope(needle) {
    if (needle.charAt(0) === ":") return { set: null, len: 1 };
    var m = /^([uatobrew+]{1,15}):/i.exec(needle);
    if (!m) return null;
    var set = { u: false, a: false, t: false, o: false, b: false, r: false, e: false, w: false };
    var run = m[1].toLowerCase();
    for (var i = 0; i < run.length; i++) {
      var p = run.charAt(i);
      if (p === "+") continue; // the v1.73 separator, still accepted
      if (set[p]) return null; // a repeated letter is a word, not a scope
      set[p] = true;
    }
    if (!activeLetters(set).length) return null;
    return { set: set, len: m[0].length };
  }
  function searchInScope(i) {
    return !searchScope || countRec(i, searchScope, searchNeedle, !!searchScope.w) > 0;
  }
  function scopeLetters(set) {
    return ["u", "a", "t", "o", "b", "r", "e"].filter(function (k) { return set && set[k]; });
  }
  function activeLetters(set) {
    return ["u", "a", "t", "o", "b", "r", "e", "w"].filter(function (k) { return set && set[k]; });
  }
  function search(v) {
    var qc = $("qcount");
    showQNav(false);
    clearHl();
    navPos = -1;
    navMark = -1;
    curHit = null;
    hitRecs = [];
    totalHits = 0;
    var needle = v.trim();
    var scoped = parseScope(needle);
    if (scoped) {
      var rest = needle.slice(scoped.len);
      // A PURE scope run ("auto:") has nothing after it to search — it searches
      // ITSELF, literally, no escape needed. (The parser still reports the scope, so
      // the dropdown's armed-but-empty state keeps its icon and checkboxes; only the
      // search falls back to the literal.)
      if (scoped.set && !rest.length) scoped = null;
      else needle = rest;
    }
    if (needle.length < 2) {
      qc.textContent = "";
      classCounts = null;
      updateScopeCounts();
      return;
    }
    searchScope = scoped && scoped.set ? scoped.set : null;
    var lc = needle.toLowerCase();
    searchNeedle = lc;
    classCounts = { u: 0, a: 0, t: 0, o: 0, b: 0, r: 0, e: 0 };
    for (var i = 0; i < records.length; i++) {
      ensureRecText(i);
      var whole = !!(searchScope && searchScope.w);
      recSearchParts[i].forEach(function (part) {
        var partHits = countOcc(recText[i].slice(part.start, part.end), lc, whole);
        if (!partHits) return;
        ["u", "a", "t", "o", "b", "r", "e"].forEach(function (k) {
          if (part.mask & CLASS_BIT[k]) classCounts[k] += partHits;
        });
      });
      var n = countRec(i, searchScope, lc, whole);
      if (n) { hitRecs.push({ rec: i, count: n, start: totalHits }); totalHits += n; }
    }
    updateScopeCounts();
    matEls().forEach(function (e) {
      if (searchInScope(+e.dataset.idx)) markHits(e, lc, lc.length, whole);
    });
    qc.textContent = totalHits + " hit" + (totalHits === 1 ? "" : "s")
      + (scopeLetters(searchScope).length ? " in " + scopeLetters(searchScope).join("") : "")
      + (whole ? " · whole words" : "");
    showQNav(totalHits > 0);
  }
  // Materialize record `ti`'s region and return its element (shared by hit nav).
  function matRecord(ti) {
    var y = P()[ti];
    setWindow(idxAt(y - MARGIN_PX), idxAt(y + window.innerHeight + MARGIN_PX) + 1);
    var target = null;
    matEls().some(function (e) {
      if (+e.dataset.idx === ti) { target = e; return true; }
      return false;
    });
    return target;
  }
  // #102: a hit can sit inside a "⋯ N more lines" cap (a display:none `.more` div)
  // or a clamped user turn (`.clamped`, max-height + overflow:hidden) — goTo opens
  // FOLDS but neither of these, so the jump either derived its target from a zero
  // rect (hidden ⇒ scrolled to ~page top) or landed on a clipped, invisible mark.
  // Expand the whole enclosing chain first, recording small cap expansions exactly
  // like a click would (#67) so they survive rematerialization.
  function revealMark(m) {
    for (var p = m.parentElement; p; p = p.parentElement) {
      if (p.classList.contains("more") && !p.classList.contains("shown")) {
        var btn = document.querySelector('.morebtn[data-more="' + p.id + '"]');
        if (btn) {
          var blk = btn.closest(".blk");
          if (blk && blk.id && btn.dataset.ord != null
              && hiddenLineCount(p) <= MAX_BUFFER_LINES) {
            smallMore[blk.id + ":" + btn.dataset.ord] = true;
          }
          expandMore(btn);
        } else {
          p.classList.add("shown");
        }
      }
      if (p.classList.contains("clamped")) {
        var cb = p.nextElementSibling;
        if (cb && cb.classList.contains("clampbtn")) {
          p.classList.remove("clamped");
          p.style.maxHeight = "";
          cb.textContent = "▲ show less";
        }
      }
    }
  }
  // Record-FIRST stepping: `hitRecs` — the record-TEXT counts, the same source of
  // truth the total comes from — is the authoritative walk, and the DOM's marks are
  // presentation. The old walk (#66) stepped the rendered marks and skipped any hit
  // record whose occurrences the DOM could not mark (a needle spanning styled text
  // nodes — highlight spans, inline markup — is counted in the record text but never
  // matches inside a single node). Skipping was wrong twice over: those hits were
  // unreachable — with mark-poor content the walk visibly cycled among the few
  // markable blocks — and every skip forced a full window rebuild via matRecord,
  // freezing the page for the length of the scan.
  //
  // The rule now: EVERY press MOVES. Per hit record the walk visits each rendered
  // mark once — or the record itself, once, when the DOM could mark nothing — and
  // then crosses to the NEXT hit record. Occurrences beyond what the DOM can mark
  // are not separate stops (they would land on the same pixel repeatedly, a stall
  // that reads as "the button does nothing"); the flat counter says where in the
  // TOTAL the landing sits, so crossing a mark-poor record advances it by that
  // record's whole count. Cost per press is bounded: at most two window
  // materializations (the boundary cross), never a scan.
  var navPos = -1, navMark = -1; // hit-record position + mark index (Infinity = enter at the END)
  var curHit = null; // {rec, mark} — re-applied on rematerialization (postMat)
  function stepHit(dir) {
    if (!hitRecs.length) return;
    // Every step is user-initiated navigation, whatever triggered it — ⏎ in the box
    // (whose keydown the intent listener deliberately ignores: TYPING is not scroll
    // intent), the ▲▼ steppers, anything later. Mark the intent like goToId does, so
    // the follow classifier reads the jump's scroll as the user moving. Without this,
    // a step from a PINNED live view is displacement, healed by an instant snap back
    // to the tail — a search that "finds 14 hits you can never see".
    lastUserInput = performance.now();
    // Continuing the SEQUENCE only makes sense while the reader is still at the current
    // hit. If they moved — scrolled away, clicked a turn, jumped through the sidebar — the
    // stored position is where the search was, not where they are, and "next" means next
    // FROM HERE. So: current anchor off-screen (or unmaterialized, which is the same fact
    // seen by the virtualizer) ⇒ drop the sequence and re-enter through the viewport
    // anchor below. The anchor is the cur mark when one rendered, else the hit record's
    // own element (a mark-less landing is still a position the reader is AT).
    if (navPos >= 0) {
      var anchor = document.querySelector("#stream mark.hl.cur");
      if (!anchor && hitRecs[navPos]) {
        anchor = document.querySelector(
          '#stream [data-idx="' + hitRecs[navPos].rec + '"]'
        );
      }
      var off = true;
      if (anchor) {
        var cr = anchor.getBoundingClientRect();
        off = cr.bottom < 0 || cr.top > window.innerHeight;
      }
      if (off) {
        navPos = -1;
        navMark = -1;
      }
    }
    if (navPos < 0) {
      // Entering navigation: start from the viewport, not the top of the document. `k` is
      // the first hit record that BEGINS at or below the viewport top (stream coords) —
      // the nearest hit that is on screen or reached by scrolling down. Forward starts
      // there (wrapping to the first hit when every hit is behind); backward starts on the
      // hit just above (wrapping to the last). Enter/n keep cycling through the wrap below.
      var vt = window.scrollY - streamTop();
      var k = -1;
      for (var f = 0; f < hitRecs.length; f++) {
        if (P()[hitRecs[f].rec] >= vt) { k = f; break; }
      }
      if (dir > 0) {
        navPos = k >= 0 ? k : 0;
        navMark = 0;
      } else {
        navPos = k > 0 ? k - 1 : hitRecs.length - 1;
        navMark = Infinity; // the record's LAST visit, known after materialization
      }
    } else {
      navMark += dir;
      if (navMark < 0) {
        navPos = (navPos - 1 + hitRecs.length) % hitRecs.length; // wraps
        navMark = Infinity;
      }
    }
    // Resolve the landing. A record's visit count needs its DOM (marks are counted
    // after materialization), so a forward boundary-cross resolves here — at most one
    // extra iteration, because every hit record yields at least one landing.
    var hr, el, marks, visits;
    for (;;) {
      hr = hitRecs[navPos];
      el = matRecord(hr.rec);
      marks = el ? el.querySelectorAll("mark.hl") : [];
      // Visits: each rendered mark once (capped at the counted occurrences when the
      // DOM over-renders — a fold's collapsed and expanded faces can both match), or
      // ONE landing on the record when nothing could be marked.
      visits = marks.length ? Math.min(marks.length, Math.max(hr.count, 1)) : 1;
      if (navMark === Infinity) navMark = visits - 1; // entered stepping backward
      if (navMark < visits) break;
      navPos = (navPos + 1) % hitRecs.length; // forward cross — wraps
      navMark = 0;
    }
    all("#stream mark.hl.cur").forEach(function (m) { m.classList.remove("cur"); });
    var m = marks.length ? marks[navMark] : null;
    curHit = m ? { rec: hr.rec, mark: navMark } : null;
    // The flat position of THIS landing within the total: mark-poor records advance
    // it by their whole count on the boundary cross — honest about occurrences the
    // DOM cannot address individually.
    $("qcount").textContent =
      (hr.start + Math.min(navMark, hr.count - 1) + 1) + "/" + totalHits;
    if (m) {
      m.classList.add("cur");
      revealMark(m); // #102 — expand caps/clamps BEFORE goTo reads the rect
      goTo(m, true);
    } else if (el) {
      goTo(el, true); // mark-less hit record: land on it, never skip it
    }
    // Settle the pin SYNCHRONOUSLY, position deciding — the async classifiers race:
    // the jump's own materialization resizes the body, and the ResizeObserver heal
    // consults only `following`, not intent. If it wins the race against the scroll
    // event, a still-pinned page snaps back to the tail over the landing. By deciding
    // here, the observers find the pin already correct and heal nothing.
    setFollowing(atBottom());
    spy();
  }
  // #102: visible prev/next steppers — Shift+Enter always went backward, but
  // nothing said so; the arrows make both directions discoverable and mousable.
  // mousedown is swallowed so the input keeps focus and ⏎ keeps working after a click.
  ["qprev", "qnext"].forEach(function (id) {
    var b = $(id);
    if (!b) return;
    b.addEventListener("mousedown", function (e) { e.preventDefault(); });
    b.addEventListener("click", function () { stepHit(id === "qprev" ? -1 : 1); });
  });
  function showQNav(on) {
    ["qprev", "qnext"].forEach(function (id) {
      var b = $(id);
      if (b) b.classList.toggle("on", !!on);
    });
  }
  // The scope dropdown is the visible face of the `uatobrew:` prefix, and the BOX is the
  // single source of truth: checking a box rewrites the prefix in the input, typing a
  // prefix by hand checks the boxes — neither can drift from the other. The rebuilt
  // prefix is canonical (`uato` order); a hand-typed permutation is honored as typed.
  var qscope = $("qscope");
  // The per-class hit counts of the CURRENT query, shown beside each dropdown choice
  // (null = no query). Computed in search()'s one pass over the records.
  var classCounts = null;
  function updateScopeCounts() {
    ["u", "a", "t", "o", "b", "r", "e"].forEach(function (k) {
      var el = $("qsn-" + k);
      if (el) el.textContent = classCounts ? String(classCounts[k]) : "";
    });
  }
  function scopeMenu(open) {
    var m = $("qscopemenu");
    if (m) m.classList.toggle("on", !!open);
  }
  function syncQScope() {
    var parsed = parseScope(q.value.trim());
    var set = (parsed && parsed.set)
      || { u: false, a: false, t: false, o: false, b: false, r: false, e: false, w: false };
    ["u", "a", "t", "o", "b", "r", "e", "w"].forEach(function (k) {
      var cb = $("qs-" + k);
      if (cb) cb.checked = !!set[k];
    });
    // The trigger is a plain funnel icon: gray = no scope (everything searched),
    // colored = some scope active. The letters live in the box, not the icon.
    if (qscope) qscope.classList.toggle("on", activeLetters(set).length > 0);
  }
  function applyScopeFromMenu() {
    var letters = ["u", "a", "t", "o", "b", "r", "e", "w"].filter(function (k) {
      var cb = $("qs-" + k);
      return cb && cb.checked;
    });
    var trimmed = q.value.replace(/^\s+/, "");
    var parsed = parseScope(trimmed);
    var rest = parsed ? trimmed.slice(parsed.len) : trimmed;
    q.value = (letters.length ? letters.join("") + ":" : "") + rest;
    search(q.value);
    syncQScope();
  }
  if (qscope) {
    qscope.addEventListener("click", function () {
      scopeMenu(!$("qscopemenu").classList.contains("on"));
    });
    ["u", "a", "t", "o", "b", "r", "e", "w"].forEach(function (k) {
      var cb = $("qs-" + k);
      if (cb) cb.addEventListener("change", applyScopeFromMenu);
    });
  }
  q.addEventListener("input", function () { search(q.value); syncQScope(); });
  q.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && totalHits) {
      stepHit(e.shiftKey ? -1 : 1);
    }
    if (e.key === "Escape") q.blur();
    e.stopPropagation();
  });

  // ── keyboard ─────────────────────────────────────────────────────────
  document.addEventListener("keydown", function (e) {
    if (e.target.tagName === "INPUT" || e.target.tagName === "TEXTAREA") return;
    if (e.key === "/") { e.preventDefault(); q.focus(); return; }
    if (e.key === "Escape") {
      toolMenu(false);
      agentMenu(false);
      scopeMenu(false);
      taskPanel(false);
      if (filter) { setFilter(null); return; }
      if (document.activeElement) document.activeElement.blur();
      return;
    }
    if ((e.key === "n" || e.key === "N") && filter) {
      e.preventDefault();
      filterNav(e.key === "n" ? 1 : -1); // #49 next/prev filtered hit
      return;
    }
    if (e.key === "j" || e.key === "k") {
      e.preventDefault();
      // Only VISIBLE fold headers — a header inside a collapsed group (or hidden by
      // the tool filter) has `offsetParent === null`, so it's skipped. Move relative
      // to whatever's focused now, so every stroke lands on the next visible one.
      var hs = all(".fold-h").filter(function (x) { return x.offsetParent !== null; });
      if (!hs.length) return;
      var cur = hs.indexOf(document.activeElement);
      var next = cur < 0 ? (e.key === "j" ? 0 : hs.length - 1) : cur + (e.key === "j" ? 1 : -1);
      var h = hs[Math.max(0, Math.min(hs.length - 1, next))];
      h.focus({ preventScroll: true });
      var r = h.getBoundingClientRect();
      if (r.top < 100 || r.bottom > window.innerHeight - 60) {
        window.scrollTo({ top: r.top + window.scrollY - 160, behavior: "smooth" });
      }
      return;
    }
    // §8.3 code-density keys (global): size −/+, wrap w.
    if (e.key === "-" || e.key === "_") { setMono(ms - 0.5); return; }
    if (e.key === "+" || e.key === "=") { setMono(ms + 0.5); return; }
    if (e.key === "w") { setWrap(!wrap); return; }
    var active = document.activeElement;
    if ((e.key === " " || e.key === "Enter") && active && active.classList.contains("fold-h")) {
      e.preventDefault();
      var f = active.closest(".fold");
      toggleFold(f, f.dataset.open !== "1");
      return;
    }
    if (e.key === "[" || e.key === "]") {
      e.preventDefault();
      // Position-based over the RECORDS (#50 — turns outside the DOM window count
      // too): `]` goes to the first turn below the goTo landing line, `[` to the
      // last turn above it, with a ±dead-zone so a just-navigated turn doesn't
      // re-select itself.
      var dest = null;
      if (e.key === "]") {
        for (var i = 0; i < records.length; i++) {
          if (records[i].turn == null) continue;
          dest = i; // falls back to the last turn if none lies below the line
          if (turnTop(i) > GOTO_Y + 8) break;
        }
      } else {
        for (var j = 0; j < records.length; j++) {
          if (records[j].turn == null) continue;
          if (turnTop(j) < GOTO_Y - 8) dest = j;
          else break;
        }
        if (dest == null) {
          for (var j0 = 0; j0 < records.length; j0++) {
            if (records[j0].turn != null) { dest = j0; break; }
          }
        }
      }
      if (dest != null) goToId(records[dest].id);
    }
  });

  // ── scroll spy ───────────────────────────────────────────────────────
  // `cur` is the last turn whose header has scrolled above the sticky line —
  // i.e. the turn you're currently reading. The bar shows it continuously and
  // hands off to the next turn the moment that turn's header crosses the line.
  // (The old `bottom < 90` test only revealed the bar once a card had scrolled
  // fully past, so a turn closely followed by the next never got a sticky head.)
  // Must sit just BELOW where goTo lands a target (GOTO_Y = 120): otherwise a turn you
  // click/navigate to lands below this line and spy keeps the PREVIOUS turn selected
  // (and a second click is a no-op because the scroll doesn't move → spy never re-runs).
  var STICKY_Y = 130;
  // A turn record's viewport top: the REAL rect when materialized (estimates can
  // drift by hundreds of px on unmeasured blocks), record math otherwise (#50).
  function turnTop(i) {
    var e = document.getElementById(records[i].id);
    if (e && e.dataset.idx != null) return e.getBoundingClientRect().top;
    return streamTop() + P()[i] - window.scrollY;
  }
  function spy() {
    var cur = null;
    if (records.length) {
      for (var i = 0; i < records.length; i++) {
        if (records[i].turn == null) continue;
        if (turnTop(i) <= STICKY_Y) cur = records[i];
        else break;
      }
      // End rule (#89): at the document bottom no further header can ever cross
      // the sticky line, so the LAST turn could otherwise never become active —
      // exactly where a pinned live tail sits. At the bottom, the last turn wins.
      if (atBottom()) {
        for (var j = records.length - 1; j >= 0; j--) {
          if (records[j].turn != null) { cur = records[j]; break; }
        }
      }
    }
    curTurn = cur;
    var bar = $("stickybar");
    bar.classList.toggle("on", !!cur);
    if (cur) $("stickytext").textContent = "Turn " + cur.turn + " — " + cur.label;
    var changed = cur && cur.id !== lastActiveId;
    lastActiveId = cur ? cur.id : null;
    all(".side-item").forEach(function (si) {
      var active = !!cur && si.dataset.t === cur.id;
      si.classList.toggle("active", active);
      // Keep the active turn visible when the list scrolls independently.
      if (active && changed) si.scrollIntoView({ block: "nearest" });
    });
  }
  var lastActiveId = null;
  window.addEventListener("scroll", function () {
    if (performance.now() - lastUserInput < USER_MS) {
      // #103 hysteresis: while pinned the old slack decides (away unpins), but
      // acquiring the pin needs the true end — near-bottom reading never pins.
      setFollowing(following ? atBottom() : atEnd());
    } else if (following && !atBottom()) {
      toBottom(); // browser displacement (anchoring/clamp) while pinned — heal it
    }
    if (newCount && atBottom()) newCount = 0; // caught up by scrolling down
    // NOT inside the rAF below: a background tab pauses `requestAnimationFrame`, and the pill
    // has to be right the moment the tab is looked at. It is guarded to a no-op unless the
    // state actually changed, so running it per scroll event costs a comparison.
    paintBadge();
    if (raf) return;
    raf = requestAnimationFrame(function () {
      raf = null;
      updateView(); // #50: materialize the window the scroll landed on
      spy();
    });
  }, { passive: true });
  window.addEventListener("resize", function () { updateView(); }, { passive: true });
  spy();

  // On load, deep-link wins; otherwise jump to the end so the newest messages
  // show first (and live updates then follow the bottom).
  if (location.hash) {
    var hid = location.hash.slice(1);
    // A deep link lands mid-history and stays there, so the offer has to be painted for it —
    // no scroll follows to trigger the handler above.
    setTimeout(function () { goToId(hid); paintBadge(); }, 150);
  } else {
    // A live page OWNS its landing position (the tail) — stop the browser's async
    // scroll restoration from yanking the view to a stale offset seconds later (#89).
    if (pollMs > 0 && "scrollRestoration" in history) history.scrollRestoration = "manual";
    // With a restore pending, stay put: the first apply lands us (#170). Jumping to the
    // tail here first would flash the bottom on every session switch.
    if (!pendingRestore) {
      setFollowing(true);
      toBottom();
    }
  }
})();
