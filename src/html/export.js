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
  var matches = [];
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
    stream.appendChild(renderBlock(obj));
    if (obj.turn != null) addTurn(obj);
  }

  // The O(DOM) passes to run after a batch of new records: clamp long turns, rebuild the
  // filter menu, give new code/diff panes their control strip, re-apply size + wrap.
  function postRender() {
    clampLongTurns();
    buildToolMenu();
    buildStrips();
    setMono(ms);
    setWrap(wrap);
    if (filter) applyFilter(filter);
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

  // Drop rendered blocks from stream index `from` onward (a rewritten tail), plus
  // their sidebar turn entries, so the re-emitted records rebuild them cleanly.
  function resetFrom(from) {
    while (stream.children.length > from) {
      var last = stream.lastElementChild;
      var si = turnlist.querySelector('.side-item[data-t="' + last.id + '"]');
      if (si) si.remove();
      last.remove();
    }
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
    stream.appendChild(renderBlock(b));
    if (b.turn != null) addTurn(b);
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
    var entries = {}; // selector -> {label, count}
    all(".fold[data-tool]").forEach(function (f) {
      var sel = '.fold[data-tool="' + f.dataset.tool + '"]';
      (entries[sel] = entries[sel] || { label: f.dataset.tool, count: 0 }).count++;
    });
    all(".fold[data-kind]:not([data-tool])").forEach(function (f) {
      var k = f.dataset.kind;
      var sel = '.fold[data-kind="' + k + '"]';
      (entries[sel] = entries[sel] || { label: KIND_LABEL[k] || k, count: 0 }).count++;
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

  // Apply the current `filter` selector to the DOM: matching folds stay, expanded, with
  // an accent; user turns stay dimmed as landmarks; the rest hide.
  function applyFilter(sel) {
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

  // Enter/leave/toggle the filter. `sel` is a selector; re-selecting the active one clears.
  function setFilter(sel, label) {
    if (sel === filter) sel = null;
    if (sel && !filter) {
      // Snapshot every fold's open state so Clear restores it exactly.
      savedFolds = {};
      all(".fold[id]").forEach(function (f) { savedFolds[f.id] = f.dataset.open; });
    }
    filter = sel;
    if (!sel) {
      // Re-anchor to CONTENT, not to the absolute scroll offset: while filtered most blocks
      // are hidden, so the document is much shorter and scrollY means something different.
      // Whatever the user navigated/scrolled to while filtered — the topmost block visible in
      // the viewport — must stay put when the hidden blocks reflow back in; a raw offset would
      // land back at the pre-filter position instead.
      var anchor = null, anchorTop = 0;
      var blocks = all(".blk");
      for (var i = 0; i < blocks.length; i++) {
        if (blocks[i].offsetParent === null) continue; // hidden by the filter
        var r = blocks[i].getBoundingClientRect();
        if (r.bottom > 0) { anchor = blocks[i]; anchorTop = r.top; break; }
      }
      all(".blk").forEach(function (b) {
        b.classList.remove("filter-dim", "filter-hidden");
      });
      all(".fold-h").forEach(function (h) { h.classList.remove("filter-hit"); });
      if (savedFolds) {
        all(".fold[id]").forEach(function (f) {
          if (savedFolds[f.id] !== undefined) setFold(f, savedFolds[f.id] === "1");
        });
      }
      if (anchor) {
        window.scrollTo({ top: anchor.getBoundingClientRect().top + window.scrollY - anchorTop });
      }
    } else {
      applyFilter(sel);
    }
    all(".tool-item").forEach(function (ti) {
      ti.classList.toggle("active", ti.dataset.sel === filter);
    });
    // The button becomes "<label> ✕": the label opens the menu, the ✕ clears.
    $("btn-tools").classList.toggle("active", !!filter);
    document.querySelector("#btn-tools .tf-label").textContent = filter ? label : "Filter ▾";
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
    var before = stream.childElementCount;
    consume(text);
    var added = stream.childElementCount - before;
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
            var before = stream.childElementCount;
            consumePull(reply);
            var added = stream.childElementCount - before;
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
            var before = stream.childElementCount;
            consumeDelta(new TextDecoder().decode(d.bytes.subarray(skip)));
            cursor = end;
            var added = stream.childElementCount - before;
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
  function allFolds(open) { all(".fold").forEach(function (f) { setFold(f, open); }); }

  // ── §8.3 per-pane code controls / §8.8 wide mode ─────────────────────────
  // Wrap each code/diff pane in `.codewrap` + a `.codefoot` row shared with the
  // "⋯ N more lines" expander: expander left, controls (A− size A+ wrap copy) right.
  // Static button styling lives in the stylesheet; only state goes on classes.
  function buildStrips() {
    all(".numbered, .diff").forEach(function (c) {
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
  function goTo(target) {
    if (!target) return;
    for (var p = target; p; p = p.parentElement) {
      if (p.classList && p.classList.contains("fold")) setFold(p, true);
    }
    window.scrollTo({ top: target.getBoundingClientRect().top + window.scrollY - GOTO_Y, behavior: "smooth" });
    target.classList.add("flash");
    setTimeout(function () { target.classList.remove("flash"); }, 1000);
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
  var wideBtn = $("btn-wide");
  if (wideBtn) wideBtn.addEventListener("click", function () { setWide(!wide); });
  window.addEventListener("resize", function () { fitBar(); }, { passive: true });

  // ── search ───────────────────────────────────────────────────────────
  // `matches` holds the highlight <mark> elements (the hits), in document order. Typing
  // wraps every occurrence in a <mark class="hl">; Enter cycles them, marking the current
  // one `.cur` and scrolling to it (Shift+Enter goes back).
  var q = $("q");
  function clearHl() {
    var touched = [];
    all("#stream mark.hl").forEach(function (m) {
      var p = m.parentNode;
      p.replaceChild(document.createTextNode(m.textContent), m);
      if (touched.indexOf(p) === -1) touched.push(p);
    });
    touched.forEach(function (p) { p.normalize(); }); // merge the split text nodes back
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
    var needle = v.trim();
    if (needle.length < 2) { matches = []; qc.textContent = ""; return; }
    var lc = needle.toLowerCase();
    all(".blk").forEach(function (b) {
      if (b.textContent.toLowerCase().indexOf(lc) !== -1) markHits(b, lc, needle.length);
    });
    matches = all("#stream mark.hl");
    qc.textContent = matches.length + " hit" + (matches.length === 1 ? "" : "s");
  }
  function gotoHit(i) {
    matches.forEach(function (m) { m.classList.remove("cur"); });
    var m = matches[i];
    if (!m) return;
    m.classList.add("cur");
    $("qcount").textContent = (i + 1) + "/" + matches.length;
    goTo(m);
  }
  q.addEventListener("input", function () { search(q.value); });
  q.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && matches.length) {
      mIdx = (mIdx + (e.shiftKey ? matches.length - 1 : 1)) % matches.length;
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
      // Position-based (not index-of-curTurn, which lags a scroll): `]` goes to the
      // first turn below the goTo landing line, `[` to the last turn above it. The
      // ±dead-zone around GOTO_Y stops a just-navigated turn from re-selecting itself.
      var turns = all("[data-turn]");
      if (!turns.length) return;
      var dest = null;
      if (e.key === "]") {
        dest = turns.find(function (t) { return t.getBoundingClientRect().top > GOTO_Y + 8; })
          || turns[turns.length - 1];
      } else {
        turns.forEach(function (t) { if (t.getBoundingClientRect().top < GOTO_Y - 8) dest = t; });
        dest = dest || turns[0];
      }
      goTo(dest);
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
