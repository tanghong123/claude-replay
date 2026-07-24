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
  var root = document.documentElement;
  var stream = document.getElementById("stream");
  var turnlist = document.getElementById("turnlist");
  var matches = [];
  var mIdx = -1;
  var ki = -1;
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
  function capped(container, rows, cap, after) {
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
    var btn = el("button", "morebtn", "⋯ " + (rows.length - cap) + " more lines");
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
      capped(box2, p.p === "num" ? numberedRows(p.rows) : diffRows(p.rows), p.cap, holder);
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
    return a;
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
    // lines with a "more" expander (measured after layout in clampLongTurns).
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
  function consume(text) {
    var recs = text.split("\n").filter(function (l) { return l.trim(); });
    while (consumed < recs.length) {
      var obj;
      try { obj = JSON.parse(recs[consumed]); } catch (e) { break; }
      consumed++;
      if (obj.t === "meta") { renderMeta(obj); continue; }
      if (obj.t !== "block") continue;
      stream.appendChild(renderBlock(obj));
      if (obj.turn != null) addTurn(obj);
    }
    clampLongTurns();
    buildToolMenu();
    if (filter) applyFilter(filter); // fold/expand any newly-arrived matches
  }

  // ── tool-use filter ───────────────────────────────────────────────────
  // Populate the dropdown from the distinct data-tool values present, newest
  // counts first. Rebuilt whenever content changes (live sessions grow tools).
  function buildToolMenu() {
    var counts = {};
    all(".fold[data-tool]").forEach(function (f) {
      var t = f.dataset.tool;
      counts[t] = (counts[t] || 0) + 1;
    });
    var names = Object.keys(counts).sort(function (a, b) {
      return a.localeCompare(b); // alphabetical
    });
    var box = $("toolitems");
    box.textContent = "";
    names.forEach(function (t) {
      var item = el("div", "tool-item" + (t === filter ? " active" : ""));
      item.dataset.tool = t;
      item.tabIndex = 0;
      item.appendChild(el("span", "dot"));
      item.appendChild(el("span", "tname", t));
      item.appendChild(el("span", "tool-count", String(counts[t])));
      box.appendChild(item);
    });
    // Nothing to filter → disable the button.
    $("btn-tools").disabled = names.length === 0;
  }

  function toolMenu(open) { $("toolmenu").classList.toggle("on", open); }

  // Apply the current `filter` value to the DOM: matching tool folds stay,
  // expanded, with an accent; user turns stay dimmed as landmarks; the rest hide.
  function applyFilter(tool) {
    var sel = '.fold[data-tool="' + (window.CSS && CSS.escape ? CSS.escape(tool) : tool) + '"]';
    var matchesSel = all(sel);
    all(".fold-h").forEach(function (h) { h.classList.remove("filter-hit"); });
    all(".blk").forEach(function (b) {
      if (b.classList.contains("uturn")) {
        b.classList.remove("filter-hidden");
        b.classList.add("filter-dim");
        if (b.classList.contains("fold")) setFold(b, false); // collapse command turns
        return;
      }
      b.classList.remove("filter-dim");
      var hit = b.matches(sel) || b.querySelector(sel);
      b.classList.toggle("filter-hidden", !hit);
    });
    matchesSel.forEach(function (m) {
      for (var p = m.parentElement; p && p.id !== "stream"; p = p.parentElement) {
        if (p.classList && p.classList.contains("fold")) setFold(p, true); // expand ancestors
      }
      setFold(m, true);
      var h = m.querySelector(":scope > .fold-h");
      if (h) h.classList.add("filter-hit");
    });
  }

  // Enter/leave/toggle the filter. Re-selecting the active tool clears it.
  function setFilter(tool) {
    if (tool === filter) tool = null;
    if (tool && !filter) {
      // Snapshot every fold's open state so Clear restores it exactly.
      savedFolds = {};
      all(".fold[id]").forEach(function (f) { savedFolds[f.id] = f.dataset.open; });
    }
    filter = tool;
    if (!tool) {
      all(".blk").forEach(function (b) {
        b.classList.remove("filter-dim", "filter-hidden");
      });
      all(".fold-h").forEach(function (h) { h.classList.remove("filter-hit"); });
      if (savedFolds) {
        all(".fold[id]").forEach(function (f) {
          if (savedFolds[f.id] !== undefined) setFold(f, savedFolds[f.id] === "1");
        });
      }
    } else {
      applyFilter(tool);
    }
    all(".tool-item").forEach(function (ti) {
      ti.classList.toggle("active", ti.dataset.tool === filter);
    });
    // The button becomes "<tool> ✕": the label opens the menu, the ✕ clears.
    $("btn-tools").classList.toggle("active", !!filter);
    document.querySelector("#btn-tools .tf-label").textContent = filter || "Tools ▾";
    spy();
  }

  // A long user message shows only its first CLAMP_LINES lines with a "⋯ N more
  // lines" expander — measured after layout so it works for wrapped single
  // paragraphs too (not just newline-broken text). Run once per turn body.
  var CLAMP_LINES = 12;
  function clampLongTurns() {
    all(".uturn-md").forEach(function (md) {
      if (md.dataset.clampChecked) return;
      md.dataset.clampChecked = "1";
      var lh = parseFloat(getComputedStyle(md).lineHeight) || 25;
      var cap = lh * CLAMP_LINES;
      if (md.scrollHeight <= cap + lh) return; // fits within N (+1 slack) lines
      var hidden = Math.round((md.scrollHeight - cap) / lh);
      md.style.maxHeight = cap + "px";
      md.classList.add("clamped");
      var btn = el("button", "morebtn clampbtn", "⋯ " + hidden + " more lines");
      btn.dataset.cap = cap;
      btn.dataset.more = "⋯ " + hidden + " more lines";
      md.after(btn);
    });
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

  // Initial render from the inlined snapshot.
  var inline = $("session-data");
  turnlist.textContent = "";
  if (inline) consume(inline.textContent);

  // ── live tail ────────────────────────────────────────────────────────
  var src = document.body.dataset.src;
  var pollMs = parseInt(document.body.dataset.poll || "0", 10);
  if (src && pollMs > 0) {
    var failed = false;
    setInterval(function () {
      if (failed) return;
      fetch(src, { cache: "no-store" })
        .then(function (r) { return r.text(); })
        .then(function (text) {
          var wasAtBottom = atBottom();
          var before = stream.childElementCount;
          consume(text);
          var added = stream.childElementCount - before;
          if (added > 0) {
            // Already at the end → keep following it (and stay caught up);
            // otherwise flag the new content with the badge.
            if (wasAtBottom) { toBottom(false); clearNew(); }
            else showNew(added);
            spy();
          }
        })
        .catch(function () {
          // file:// blocks same-directory fetch in most browsers; the inlined
          // snapshot still rendered, so degrade quietly instead of looping.
          failed = true;
        });
    }, pollMs);
  }

  // ── folds ────────────────────────────────────────────────────────────
  function setFold(f, open) {
    if (!f) return;
    f.dataset.open = open ? "1" : "0";
    var h = f.querySelector(":scope > .fold-h");
    if (h) h.setAttribute("aria-expanded", open ? "true" : "false");
  }
  function allFolds(open) { all(".fold").forEach(function (f) { setFold(f, open); }); }

  function goTo(target) {
    if (!target) return;
    for (var p = target; p; p = p.parentElement) {
      if (p.classList && p.classList.contains("fold")) setFold(p, true);
    }
    window.scrollTo({ top: target.getBoundingClientRect().top + window.scrollY - 120, behavior: "smooth" });
    target.classList.add("flash");
    setTimeout(function () { target.classList.remove("flash"); }, 1000);
  }

  function copy(text, node, done, revert) {
    var orig = revert || node.textContent;
    try {
      navigator.clipboard.writeText(text).then(function () {
        node.textContent = done;
        setTimeout(function () { node.textContent = orig; }, 1200);
      });
    } catch (e) { /* clipboard unavailable */ }
  }

  document.addEventListener("click", function (e) {
    // ── tool-use filter controls ──
    var ti = e.target.closest(".tool-item");
    if (ti) { setFilter(ti.dataset.tool); toolMenu(false); return; }
    if (e.target.closest(".tf-x")) { setFilter(null); toolMenu(false); return; } // ✕ clears
    if (e.target.closest("#btn-tools")) { toolMenu(!$("toolmenu").classList.contains("on")); return; } // label opens menu
    // Any other click closes an open dropdown.
    if (!e.target.closest("#toolmenu")) toolMenu(false);

    var sid = e.target.closest("#sid");
    if (sid) { copy(sid.dataset.path, sid, "copied transcript path"); return; }
    var cpy = e.target.closest(".cpy");
    if (cpy) {
      var pre = cpy.closest(".fence").querySelector("pre");
      copy(pre.textContent, cpy, "copied", "copy");
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
    var al = e.target.closest(".alink");
    if (al) {
      e.preventDefault();
      var href = al.getAttribute("href");
      history.replaceState(null, "", href);
      goTo($(href.slice(1)));
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
    var h = e.target.closest(".fold-h");
    if (h) { var f = h.closest(".fold"); setFold(f, f.dataset.open !== "1"); return; }
    if (e.target.closest("#stickybar") && curTurn) { goTo(curTurn); return; }
    var si = e.target.closest(".side-item");
    if (si) goTo($(si.dataset.t));
  });

  var themeBtn = $("btn-theme");
  if (themeBtn) themeBtn.addEventListener("click", function () {
    var next = root.getAttribute("data-theme") === "light" ? "dark" : "light";
    try { localStorage.setItem(THEME_KEY, next); } catch (e) { /* ignore */ }
    applyTheme(next);
  });
  $("btn-exp").addEventListener("click", function () { allFolds(true); });
  $("btn-col").addEventListener("click", function () { allFolds(false); });

  // ── search ───────────────────────────────────────────────────────────
  var q = $("q");
  function search(v) {
    var qc = $("qcount");
    var needle = v.trim().toLowerCase();
    mIdx = -1;
    if (needle.length < 2) { matches = []; qc.textContent = ""; return; }
    matches = all(".blk").filter(function (n) {
      return n.textContent.toLowerCase().indexOf(needle) !== -1;
    });
    qc.textContent = matches.length + " hit" + (matches.length === 1 ? "" : "s");
  }
  q.addEventListener("input", function () { search(q.value); });
  q.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && matches.length) {
      mIdx = (mIdx + 1) % matches.length;
      $("qcount").textContent = mIdx + 1 + "/" + matches.length;
      goTo(matches[mIdx]);
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
    if (e.key === "j" || e.key === "k") {
      e.preventDefault();
      var hs = all(".fold-h");
      if (!hs.length) return;
      ki = Math.max(0, Math.min(hs.length - 1, ki + (e.key === "j" ? 1 : -1)));
      var h = hs[ki];
      h.focus({ preventScroll: true });
      var r = h.getBoundingClientRect();
      if (r.top < 100 || r.bottom > window.innerHeight - 60) {
        window.scrollTo({ top: r.top + window.scrollY - 160, behavior: "smooth" });
      }
      return;
    }
    var active = document.activeElement;
    if ((e.key === " " || e.key === "Enter") && active && active.classList.contains("fold-h")) {
      e.preventDefault();
      var f = active.closest(".fold");
      setFold(f, f.dataset.open !== "1");
      return;
    }
    if (e.key === "[" || e.key === "]") {
      e.preventDefault();
      var turns = all("[data-turn]");
      if (!turns.length) return;
      var ci = turns.indexOf(curTurn);
      if (ci < 0) ci = 0;
      goTo(turns[Math.max(0, Math.min(turns.length - 1, ci + (e.key === "]" ? 1 : -1)))]);
    }
  });

  // ── scroll spy ───────────────────────────────────────────────────────
  // `cur` is the last turn whose header has scrolled above the sticky line —
  // i.e. the turn you're currently reading. The bar shows it continuously and
  // hands off to the next turn the moment that turn's header crosses the line.
  // (The old `bottom < 90` test only revealed the bar once a card had scrolled
  // fully past, so a turn closely followed by the next never got a sticky head.)
  var STICKY_Y = 72; // just under the 48px topbar
  function spy() {
    var turns = all("[data-turn]");
    var cur = null;
    for (var i = 0; i < turns.length; i++) {
      if (turns[i].getBoundingClientRect().top <= STICKY_Y) cur = turns[i];
    }
    curTurn = cur;
    var bar = $("stickybar");
    bar.classList.toggle("on", !!cur);
    if (cur) $("stickytext").textContent = "Turn " + cur.dataset.turn + " — " + cur.dataset.label;
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
      spy();
      if (newCount && atBottom()) clearNew(); // caught up by scrolling down
    });
  }, { passive: true });
  spy();

  // On load, deep-link wins; otherwise jump to the end so the newest messages
  // show first (and live updates then follow the bottom).
  if (location.hash) {
    var target = $(location.hash.slice(1));
    if (target) setTimeout(function () { goTo(target); }, 150);
  } else {
    toBottom(false);
  }
})();
