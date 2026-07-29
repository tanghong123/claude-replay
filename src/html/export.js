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
  var mIdx = -1;
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
    try {
      return new Date(ts * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
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
  var recHit = [];       // with a filter active: does this record (or a nested one) match?
  var idIndex = {};      // block id (incl. nested items) -> top-level record index
  var loIdx = 0, hiIdx = 0; // materialized window [loIdx, hiIdx)
  var EST_H = 30;
  var MARGIN_PX = 1500;
  var prefix = null;     // prefix[i] = sum of effective heights of records[0..i)
  var topPad = null, botPad = null;
  var searchNeedle = ""; // active search term (lowercase), re-marked on materialize

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
    if (searchNeedle) markHits(e, searchNeedle, searchNeedle.length);
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
        topPad.nextSibling.remove();
        loIdx++;
        while (loIdx < lo && loIdx < hiIdx && isHiddenRec(loIdx)) loIdx++;
      }
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
        botPad.previousSibling.remove();
        hiIdx--;
        while (hiIdx > hi && hiIdx > loIdx && isHiddenRec(hiIdx - 1)) hiIdx--;
      }
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
    var st = streamTop();
    var y0 = window.scrollY - st;
    var lo = idxAt(y0 - MARGIN_PX);
    var hi = idxAt(y0 + window.innerHeight + MARGIN_PX) + 1;
    // Anchor: the first materialized element still on screen (or the window start).
    var anchorEl = null, anchorTop = 0;
    matEls().some(function (e) {
      var r = e.getBoundingClientRect();
      if (r.bottom > 0) { anchorEl = e; anchorTop = r.top; return true; }
      return false;
    });
    setWindow(lo, hi);
    if (anchorEl && anchorEl.isConnected) {
      var d = anchorEl.getBoundingClientRect().top - anchorTop;
      if (Math.abs(d) > 1) window.scrollBy(0, d);
    }
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
    recHit.push(false);
    indexIds(b, records.length - 1);
    if (b.turn != null) addTurn(b);
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
        // Show images inline. A served/exported page carries the bytes as a data: URI; an
        // offline bundle materializes them to assets/<file> and links via `att_href` — both
        // are valid <img> sources, so the bundle shows the image too (not just a download).
        var imgsrc = datauri != null ? datauri
            : (h.att_kind === "image" && href != null ? href : null);
        if (imgsrc != null) {
            var img = el("img", "aimg");
            img.src = imgsrc; img.alt = h.att_name || "image";
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
      var ab = el("div", "ablock blk");
      ab.id = b.id;
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

  function renderMeta(m) {
    if (m.title) {
      document.title = m.title;
      $("title").textContent = m.title;
    }
    var meta = $("meta");
    meta.textContent = "";
    var sid = el("span", null, m.sid || "");
    sid.id = "sid";
    sid.title = "Click to copy transcript path";
    sid.dataset.path = m.path || "";
    meta.appendChild(sid);
    if (m.cwd) meta.appendChild(el("span", null, m.cwd));
    var d = fmtDur(m.duration_secs);
    var bits = [];
    if (m.turns != null) bits.push(m.turns + " turn" + (m.turns === 1 ? "" : "s"));
    if (m.tools != null) bits.push(m.tools + " tool call" + (m.tools === 1 ? "" : "s"));
    if (d) meta.appendChild(el("span", null, d));
    if (bits.length) meta.appendChild(el("span", null, bits.join(" · ")));

    var u = m.usage || {};
    var box = $("usage");
    box.textContent = "";
    box.appendChild(el("div", "side-head", "Usage"));
    function row(k, v, cls) {
      var r = el("div", "urow" + (cls ? " " + cls : ""));
      r.appendChild(el("span", null, k));
      r.appendChild(el("span", null, v));
      box.appendChild(r);
    }
    row("input", (u.input || "0") + " tok");
    row("output", (u.output || "0") + " tok");
    row("cache read", (u.cache_read || "0") + " tok");
    if (u.cost) row("est. cost", u.cost, "total");

    renderCrumbs(m.ancestors);
    // In a multi-agent tree (this session has children, or is itself a sub-agent) the box
    // is always shown — grayed on a childless leaf. A standalone session hides it entirely.
    var inTree = (m.children && m.children.length) || (m.ancestors && m.ancestors.length);
    renderAgentMenu(m.children, inTree);
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
    if (!ancestors || !ancestors.length) { nav.style.display = "none"; return; }
    var parent = ancestors[ancestors.length - 1]; // the immediate (spawning) parent
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
  // and non-interactive, so the control never appears/disappears between views. Each item
  // navigates to that agent's stream (click = this tab; the ⧉ icon = a new tab).
  function renderAgentMenu(children, inTree) {
    var wrap = $("agentnav"), items = $("agentitems"), btn = $("btn-agents");
    if (!wrap || !items || !btn) return;
    wrap.style.display = inTree ? "" : "none";
    if (!inTree) return;
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

  // Append one turn to the sidebar (live sessions grow it).
  function addTurn(b) {
    var item = el("div", "side-item", b.turn + " · " + b.label);
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

  // Delta-only: the multi-file live feed hands us just the NEW bytes each poll, so we parse
  // only the new complete lines — never re-splitting the whole accumulated stream — and keep
  // any trailing partial line in `pending`. A bad line is skipped, not fatal.
  var pending = "";
  function consumeDelta(chunk) {
    pending += chunk;
    var nl = pending.lastIndexOf("\n");
    if (nl < 0) return; // no complete line yet
    var lines = pending.slice(0, nl).split("\n");
    pending = pending.slice(nl + 1);
    for (var i = 0; i < lines.length; i++) {
      var l = lines[i];
      if (!l.trim()) continue;
      var obj;
      try { obj = JSON.parse(l); } catch (e) { continue; }
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
    recHit.length = from;
    // Ids of dropped records (incl. nested) leave the index; a full rebuild is
    // cheap and only runs on tail rewrites.
    idIndex = {};
    records.forEach(function (b, i) { indexIds(b, i); });
    turnlist.textContent = "";
    records.forEach(function (b) { if (b.turn != null) addTurn(b); });
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
    if (r.epoch === pc.epoch && !r.committed.length && !r.provisional.length) return;
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
  }

  // ── type / tool filter ────────────────────────────────────────────────
  // Populate the dropdown with the distinct message types present: each tool by its
  // NAME (Read, Bash, Update, …) AND each non-tool fold kind (Agent, Thinking, Activity,
  // Command). `filter` is the CSS selector the chosen entry maps to. Rebuilt whenever
  // content changes (live sessions grow types).
  var KIND_LABEL = { agent: "Agent", think: "Thinking", act: "Activity", command: "Command" };
  function buildToolMenu() {
    // Counted from the RECORDS (nested items included), not the DOM — the DOM only
    // holds the materialized window (#50).
    var entries = {}; // selector -> {label, count}
    eachFoldRec(function (b) {
      if (b.tool) {
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
    sels.forEach(function (sel) {
      var e = entries[sel];
      var item = el("div", "tool-item" + (sel === filter ? " active" : ""));
      item.dataset.sel = sel;
      item.dataset.label = e.label;
      item.tabIndex = 0;
      item.appendChild(el("span", "dot"));
      item.appendChild(el("span", "tname", e.label));
      item.appendChild(el("span", "tool-count", String(e.count)));
      box.appendChild(item);
    });
    $("btn-tools").disabled = sels.length === 0;
  }

  function toolMenu(open) { $("toolmenu").classList.toggle("on", open); }
  function agentMenu(open) { $("agentmenu").classList.toggle("on", open); }

  // Does record `b` (or a nested item) match the filter selector's meaning? The two
  // selector shapes the menu emits are '.fold[data-tool="X"]' / '.fold[data-kind="k"]'.
  function parseFilterSel(sel) {
    var mt = /\[data-tool="([^"]+)"\]/.exec(sel);
    if (mt) return { tool: mt[1] };
    var mk = /\[data-kind="([^"]+)"\]/.exec(sel);
    if (mk) return { kind: mk[1] };
    return {};
  }
  function recMatch(b, want) {
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
  }
  function openFilterChain(b, want) {
    var direct = (want.tool && b.tool === want.tool) ||
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
    var sel = want.tool
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
  function toBottom(smooth) {
    window.scrollTo({ top: document.body.scrollHeight, behavior: smooth ? "smooth" : "auto" });
  }
  var newCount = 0;
  var badge = $("newbadge");
  function showNew(n) {
    newCount += n;
    badge.textContent = "↓ " + newCount + " new message" + (newCount === 1 ? "" : "s");
    badge.classList.add("on");
  }
  function clearNew() {
    newCount = 0;
    badge.classList.remove("on");
  }
  badge.addEventListener("click", function () { toBottom(true); clearNew(); });

  // Initial render from the inlined snapshot, then drop the inline copy: the
  // rendered DOM is the source of truth now, so keeping ~1× the payload as script
  // text is pure waste (live mode re-reads from the companion, not this element).
  var inline = $("session-data");
  turnlist.textContent = "";
  var pollMs = parseInt(document.body.dataset.poll || "0", 10);
  var multi = document.body.dataset.multi;

  // Render freshly consumed content, flagging/following new tail. Shared by every feed.
  function ingest(text) {
    var wasAtBottom = atBottom();
    var before = records.length;
    consume(text);
    var added = records.length - before;
    if (added > 0) {
      if (wasAtBottom) { toBottom(false); clearNew(); }
      else showNew(added);
      spy();
    }
  }

  if (multi) {
    // Multi-file bundle: this page shows ONE agent, `?session=<id>` (default `data-root`).
    // Navigation between agents is a full page load carrying a new `?session=`.
    if (inline) inline.remove();
    var sess = new URLSearchParams(location.search).get("session") || document.body.dataset.root;
    // Transport: the server sets data-pull when it serves the pull feed by default; `?transport=`
    // overrides it either way (pull|stream) for side-by-side comparison.
    var transport = new URLSearchParams(location.search).get("transport");
    var usePull = transport === "pull" || (document.body.dataset.pull === "1" && transport !== "stream");
    if (pollMs > 0 && usePull) {
      // Pull-client feed: poll `/pull?session=&cursor=` and apply the two-zone reply. The client
      // drives the tail (the server folds on our request), so an idle page costs the server nothing.
      // Committed arrives as a POINTER (`committed_ext: {offset, len}`) into the server's on-disk
      // record log; we range-read it via `/records` (phase two), then apply both zones atomically.
      // The inflight guard spans both fetches and the cursor advances only after both succeed; a
      // failed or 409 (stale-epoch after a reset) range read drops the whole reply — the next tick
      // re-pulls with the old cursor and the protocol resyncs us.
      var inflightP = false;
      var pullTick = function () {
        if (inflightP) return;
        inflightP = true;
        fetch("pull?session=" + encodeURIComponent(sess) + "&cursor=" + cursorStr(), { cache: "no-store" })
          .then(function (r) { return r.json(); })
          .then(function (reply) {
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
            var wasAtBottom = atBottom();
            var before = records.length;
            try {
              consumePull(reply);
            } catch (err) {
              // Self-heal (#54): a torn apply must never leave the page desynced — drop all
              // local state and cursor; the next tick resyncs from the server's canonical
              // state, exactly what a manual reload does.
              console.error("pull apply failed; resyncing", err);
              resetFrom(0);
              pc = { epoch: 0, committed: 0, gen: 0, index: 0 };
              return;
            }
            var added = records.length - before;
            if (added > 0) {
              if (wasAtBottom) { toBottom(false); clearNew(); }
              else showNew(added);
              spy();
            }
          })
          .catch(function () { /* server gone / mid-write / stale range — retry next tick */ })
          .finally(function () { inflightP = false; });
      };
      pullTick();
      setInterval(pullTick, pollMs);
    } else if (pollMs > 0) {
      // Served live: poll `/stream?session=&from=<byte cursor>` — the server returns ONLY
      // the bytes past the cursor (the new delta), never the whole transcript. We keep the
      // accumulated text and hand it to `consume`, which dedups records + applies resets.
      // The cursor is the ABSOLUTE byte offset we've processed up to. Each `/stream`
      // response carries `X-Offset` (where its bytes begin); we discard any prefix we
      // already have and snap the cursor to `start + len`, so the client is idempotent
      // even under overlap / a past-EOF request. The in-flight guard prevents overlap in
      // the first place — the initial `from=0` fetch can transfer many MB and outlast the
      // poll interval; without the guard the next tick would fire a second `from=0` fetch,
      // double-rendering every block and overshooting the cursor past EOF (the freeze).
      var cursor = 0, inflight = false;
      var pull = function () {
        if (inflight) return;
        inflight = true;
        fetch("stream?session=" + encodeURIComponent(sess) + "&from=" + cursor, { cache: "no-store" })
          .then(function (r) {
            var off = parseInt(r.headers.get("X-Offset") || "0", 10);
            return r.arrayBuffer().then(function (b) { return { off: off, bytes: new Uint8Array(b) }; });
          })
          .then(function (d) {
            var end = d.off + d.bytes.length;
            if (d.off > cursor || end <= cursor) return; // a gap (retry) or already-seen
            var skip = cursor - d.off; // bytes we already have (server may overlap)
            var wasAtBottom = atBottom();
            var before = records.length;
            consumeDelta(new TextDecoder().decode(d.bytes.subarray(skip)));
            cursor = end;
            var added = records.length - before;
            if (added > 0) {
              if (wasAtBottom) { toBottom(false); clearNew(); }
              else showNew(added);
              spy();
            }
          })
          .catch(function () { /* server gone / mid-write — retry next tick */ })
          .finally(function () { inflight = false; });
      };
      pull();
      setInterval(pull, pollMs);
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
  // §8.8/§8.6 apply persisted width mode and size the fixed bar to the window.
  setWide(wide);
  fitBar();

  // ── single-file live companion (`--dump-html -f`) ─────────────────────
  // No `/stream` endpoint here (the page is served flat, or `file://`), so re-fetch the
  // whole companion each cycle; `consume` skips already-rendered records.
  var src = document.body.dataset.src;
  if (!multi && src && pollMs > 0) {
    var failedC = false;
    setInterval(function () {
      if (failedC) return;
      fetch(src, { cache: "no-store" })
        .then(function (r) { return r.text(); })
        .then(ingest)
        .catch(function () { failedC = true; });
    }, pollMs);
  }

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
      b.textContent = on ? "⇔ Narrow" : "⇔ Wide";
      b.style.color = on ? "var(--tool)" : "";
      b.style.borderColor = on ? "var(--tool)" : "";
      b.title = on ? "Back to reading width" : "Wide mode — drop the reading-width cap for diff-heavy sessions";
    }
  }
  // §8.6 Progressive shedding so the fixed bar never clips its trailing control.
  function fitBar() {
    var w = window.innerWidth;
    ["btn-exp", "btn-col"].forEach(function (id) {
      var b = $(id);
      if (!b) return;
      var full = b.dataset.full;
      b.textContent = w < 1000 ? (id === "btn-exp" ? "⌄" : "⌃") : full;
      b.title = full;
      b.style.minWidth = w < 1000 ? "30px" : "";
    });
    var bs = document.querySelector("#topbar .brand-sub");
    if (bs) bs.style.display = w < 820 ? "none" : "";
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
    spy();
  }

  // §8.5 One clipboard helper for all call sites. Exports normally open from
  // file://, where navigator.clipboard is refused — so fall back to a hidden-textarea
  // execCommand and resolve success only when one path actually works. Never a false ✓.
  function copyText(text) {
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
    if (e.target.closest(".crumb-up")) {
      if (window.opener && !window.opener.closed) {
        e.preventDefault();
        try { window.opener.focus(); } catch (_) {}
        window.close();
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
    // Any other click closes an open dropdown.
    if (!e.target.closest("#toolmenu")) toolMenu(false);
    if (!e.target.closest("#agentmenu")) agentMenu(false);

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
      var hidden = $(more.dataset.more);
      if (hidden) hidden.classList.add("shown");
      more.remove();
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
    if (e.target.closest("#stickybar") && curTurn) { goToId(curTurn.id); return; }
    var si = e.target.closest(".side-item");
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
    var touched = [];
    all("#stream mark.hl").forEach(function (m) {
      var p = m.parentNode;
      p.replaceChild(document.createTextNode(m.textContent), m);
      if (touched.indexOf(p) === -1) touched.push(p);
    });
    touched.forEach(function (p) { p.normalize(); }); // merge the split text nodes back
  }
  // Search scans the RECORDS' text (#50 — the DOM only holds the window). Text per
  // record is extracted once, lazily, in tree order (headers then body, nested items
  // recursively) and cached lowercase.
  var stripDiv = null;
  function stripHtml(h) {
    if (!stripDiv) stripDiv = el("div");
    stripDiv.innerHTML = h;
    var t = stripDiv.textContent;
    stripDiv.textContent = "";
    return t;
  }
  function textOfRec(i) {
    if (recText[i] != null) return recText[i];
    var parts = [];
    (function walk(b) {
      var h = b.head || {};
      ["summary", "badge", "preview", "name", "target", "att_name"].forEach(function (k) {
        if (h[k]) parts.push(String(h[k]));
      });
      (b.body || []).forEach(function (p) {
        if (p.p === "md" || p.p === "think") parts.push(stripHtml(p.h));
        else if (p.p === "pre" || p.p === "note") parts.push(String(p.x));
        else if (p.p === "num") p.rows.forEach(function (r) { parts.push(stripHtml(String(r[1]))); });
        else if (p.p === "diff") p.rows.forEach(function (r) { parts.push(String(r[2])); });
        else if (p.p === "blocks") p.items.forEach(walk);
      });
    })(records[i]);
    recText[i] = parts.join("\n").toLowerCase();
    return recText[i];
  }
  function countOcc(t, lc) {
    var n = 0, i = 0;
    while ((i = t.indexOf(lc, i)) !== -1) { n++; i += lc.length; }
    return n;
  }
  function markHits(blk, lc, len) {
    // Collect matching text nodes first (the walk is read-only), then rewrite each so we
    // never mutate the tree we're walking. Matches within a single text node only — good
    // enough for a viewer, and it never splits across the pre-rendered highlight spans.
    var walker = document.createTreeWalker(blk, NodeFilter.SHOW_TEXT, null);
    var nodes = [], n;
    while ((n = walker.nextNode())) {
      if (n.nodeValue.toLowerCase().indexOf(lc) !== -1) nodes.push(n);
    }
    nodes.forEach(function (tn) {
      var text = tn.nodeValue, lower = text.toLowerCase();
      var frag = document.createDocumentFragment(), i = 0, idx;
      while ((idx = lower.indexOf(lc, i)) !== -1) {
        if (idx > i) frag.appendChild(document.createTextNode(text.slice(i, idx)));
        var mk = el("mark", "hl");
        mk.textContent = text.slice(idx, idx + len);
        frag.appendChild(mk);
        i = idx + len;
      }
      if (i < text.length) frag.appendChild(document.createTextNode(text.slice(i)));
      tn.parentNode.replaceChild(frag, tn);
    });
  }
  function search(v) {
    var qc = $("qcount");
    clearHl();
    mIdx = -1;
    hitRecs = [];
    totalHits = 0;
    var needle = v.trim();
    if (needle.length < 2) { qc.textContent = ""; return; }
    var lc = needle.toLowerCase();
    searchNeedle = lc;
    for (var i = 0; i < records.length; i++) {
      var n = countOcc(textOfRec(i), lc);
      if (n) { hitRecs.push({ rec: i, count: n, start: totalHits }); totalHits += n; }
    }
    matEls().forEach(function (e) { markHits(e, lc, lc.length); });
    qc.textContent = totalHits + " hit" + (totalHits === 1 ? "" : "s");
  }
  function gotoHit(gi) {
    all("#stream mark.hl.cur").forEach(function (m) { m.classList.remove("cur"); });
    var hr = null;
    for (var i = 0; i < hitRecs.length; i++) {
      if (gi >= hitRecs[i].start && gi < hitRecs[i].start + hitRecs[i].count) { hr = hitRecs[i]; break; }
    }
    if (!hr) return;
    // Materialize the hit's region, then land on its k-th mark (the record text is
    // extracted in tree order, so occurrence order ≈ DOM mark order).
    var ti = hr.rec;
    var y = P()[ti];
    setWindow(idxAt(y - MARGIN_PX), idxAt(y + window.innerHeight + MARGIN_PX) + 1);
    var target = null;
    matEls().some(function (e) {
      if (+e.dataset.idx === ti) { target = e; return true; }
      return false;
    });
    $("qcount").textContent = (gi + 1) + "/" + totalHits;
    if (!target) {
      window.scrollTo({ top: streamTop() + y - GOTO_Y });
      return;
    }
    var marks = target.querySelectorAll("mark.hl");
    var m = marks[gi - hr.start] || marks[0];
    if (m) { m.classList.add("cur"); goTo(m, true); }
    else goTo(target, true);
    updateView();
    spy();
  }
  q.addEventListener("input", function () { search(q.value); });
  q.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && totalHits) {
      mIdx = (mIdx + (e.shiftKey ? totalHits - 1 : 1)) % totalHits;
      gotoHit(mIdx);
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
    if (raf) return;
    raf = requestAnimationFrame(function () {
      raf = null;
      updateView(); // #50: materialize the window the scroll landed on
      spy();
      if (newCount && atBottom()) clearNew(); // caught up by scrolling down
    });
  }, { passive: true });
  window.addEventListener("resize", function () { updateView(); }, { passive: true });
  spy();

  // On load, deep-link wins; otherwise jump to the end so the newest messages
  // show first (and live updates then follow the bottom).
  if (location.hash) {
    var hid = location.hash.slice(1);
    setTimeout(function () { goToId(hid); }, 150);
  } else {
    toBottom(false);
    updateView();
  }
})();
