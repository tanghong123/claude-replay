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

    // Plain user turn — an always-open card.
    if (b.kind === "user") {
      var card = el("div", "uturn blk");
      card.id = b.id;
      card.dataset.turn = b.turn;
      card.dataset.label = b.label;
      card.appendChild(el("span", "caret", "❯"));
      var ub = el("div", "uturn-body");
      body.forEach(function (p) { renderPart(p, ub); });
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
      if (head.target) h.appendChild(el("span", "tool-target", head.target));
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

  function consume(text) {
    var lines = text.split("\n");
    for (; consumed < lines.length; consumed++) {
      var line = lines[consumed].trim();
      if (!line) continue;
      var obj;
      try { obj = JSON.parse(line); } catch (e) { continue; }
      if (obj.t === "meta") { renderMeta(obj); continue; }
      if (obj.t !== "block") continue;
      stream.appendChild(renderBlock(obj));
      if (obj.turn != null) addTurn(obj);
    }
  }

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
          var before = stream.childElementCount;
          consume(text);
          if (stream.childElementCount !== before) spy();
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
    var sid = e.target.closest("#sid");
    if (sid) { copy(sid.dataset.path, sid, "copied transcript path"); return; }
    var cpy = e.target.closest(".cpy");
    if (cpy) {
      var pre = cpy.closest(".fence").querySelector("pre");
      copy(pre.textContent, cpy, "copied", "copy");
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
    if (e.key === "Escape") { if (document.activeElement) document.activeElement.blur(); return; }
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
  function spy() {
    var turns = all("[data-turn]");
    var cur = null;
    for (var i = 0; i < turns.length; i++) {
      if (turns[i].getBoundingClientRect().top <= 130) cur = turns[i];
    }
    curTurn = cur;
    var bar = $("stickybar");
    var show = !!cur && cur.getBoundingClientRect().bottom < 90;
    bar.classList.toggle("on", show);
    if (cur) $("stickytext").textContent = "Turn " + cur.dataset.turn + " — " + cur.dataset.label;
    all(".side-item").forEach(function (si) {
      si.classList.toggle("active", !!cur && si.dataset.t === cur.id);
    });
  }
  window.addEventListener("scroll", function () {
    if (raf) return;
    raf = requestAnimationFrame(function () { raf = null; spy(); });
  }, { passive: true });
  spy();

  if (location.hash) {
    var target = $(location.hash.slice(1));
    if (target) setTimeout(function () { goTo(target); }, 150);
  }
})();
