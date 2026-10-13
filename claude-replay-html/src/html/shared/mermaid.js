// Transcript diagrams belong to the mounted message, on both pages. The public Mermaid
// runtime is pinned into the binary; only offline exports carry its bytes in the page.
let loading = null;
let queue = Promise.resolve();
let nextId = 0;
let themeObserver = null;
const generations = new WeakMap();
const bound = new WeakSet();

function loadMermaid() {
  if (loading) return loading;
  loading = new Promise((resolve, reject) => {
    const script = document.createElement("script");
    const ready = () => {
      const runtime = globalThis.mermaid;
      if (!runtime?.render || !runtime?.initialize) {
        script.remove();
        reject(new Error("Mermaid runtime did not initialize"));
      } else resolve(runtime);
    };
    script.onerror = () => {
      script.remove();
      reject(new Error("Could not load the bundled Mermaid runtime"));
    };
    const inline = document.getElementById("mermaid-runtime");
    if (inline) {
      try {
        script.textContent = JSON.parse(inline.textContent);
        document.head.append(script);
        ready();
      } catch (error) { script.remove(); reject(error); }
    } else {
      script.onload = ready;
      script.src = "/mermaid.js";
      document.head.append(script);
    }
  }).catch(error => { loading = null; throw error; });
  return loading;
}

function diagramTheme() {
  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

function diagramPalette(theme) {
  const dark = theme === "dark";
  const host = getComputedStyle(document.documentElement);
  return {
    darkMode: dark,
    background: dark ? "#1d212b" : "#f8f9fc",
    node: dark ? "#232a38" : "#ffffff",
    nodeBorder: dark ? "#3d4a61" : "#d3dbe9",
    cluster: dark ? "#202733" : "#f1f4f9",
    clusterBorder: dark ? "#344054" : "#e0e6f0",
    edge: dark ? "#9aa8bf" : "#7c89a0",
    ink: host.getPropertyValue("--ink").trim() || (dark ? "#f0f2f8" : "#202536"),
    sub: host.getPropertyValue("--sub").trim() || (dark ? "#adb4c5" : "#646c80"),
    accent: host.getPropertyValue("--primary").trim() || (dark ? "#74a0ff" : "#356fe8"),
    soft: host.getPropertyValue("--primary-soft").trim() || (dark ? "#233252" : "#edf3ff"),
    fontFamily: host.getPropertyValue("--sans").trim()
      || '-apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif',
  };
}

function showMermaidSource(card, showingSource) {
  card.querySelector("pre").hidden = !showingSource;
  card.querySelector(".mermaid-diagram").hidden = showingSource;
  const button = card.querySelector(".mermaid-source");
  button.textContent = showingSource ? "diagram" : "source";
  button.setAttribute("aria-pressed", String(showingSource));
}

/** Search indexes the original Markdown. A hit in diagram syntax must expose its source
 *  before the page measures that hit's position. */
function revealMermaidSource(node) {
  const source = node.closest("[data-mermaid] pre");
  if (source?.hidden) showMermaidSource(source.closest("[data-mermaid]"), true);
}

function bindSource(card, readerReshaped) {
  if (bound.has(card)) return;
  bound.add(card);
  const button = card.querySelector(".mermaid-source");
  button.addEventListener("click", () => {
    // A source toggle changes the reader's chosen view; release tail following before
    // the viewport measures that growth as an incoming message.
    readerReshaped?.();
    showMermaidSource(card, card.querySelector("pre").hidden);
  });
}

async function rejectAutomaticImages(runtime, source) {
  // Strict Mermaid still loads flowchart images and sequence actor icons. Parse without
  // layout first: offline transcripts must not let diagram data initiate image requests.
  const { db } = await runtime.mermaidAPI.getDiagramFromText(source);
  for (const vertex of db.getVertices?.().values() || []) {
    if (vertex.img) throw new Error("Image nodes are not supported in transcript diagrams");
  }
  for (const actor of db.getActors?.().values() || []) {
    const icon = actor.properties?.icon;
    if (icon && !icon.startsWith("@")) {
      throw new Error("External actor icons are not supported in transcript diagrams");
    }
  }
}

async function drawMermaid(card, generation) {
  if (!card.isConnected || generations.get(card) !== generation) return;
  const diagram = card.querySelector(".mermaid-diagram");
  const source = card.querySelector("pre");
  const errorBox = card.querySelector(".mermaid-error");
  const button = card.querySelector(".mermaid-source");
  let scratch = null;
  try {
    if (source.textContent.length > 50000) throw new Error("Diagram source exceeds 50,000 characters");
    const runtime = await loadMermaid();
    if (!card.isConnected || generations.get(card) !== generation) return;
    const palette = diagramPalette(generation.theme);
    // initialize/render share Mermaid's configuration. The page serializes this pair,
    // and a transcript's init directive cannot relax these host-owned limits.
    runtime.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      suppressErrorRendering: true,
      theme: "base",
      look: "classic",
      fontFamily: palette.fontFamily,
      themeVariables: {
        darkMode: palette.darkMode,
        fontFamily: palette.fontFamily,
        fontSize: "14px",
        background: palette.background,
        primaryColor: palette.node,
        primaryTextColor: palette.ink,
        primaryBorderColor: palette.nodeBorder,
        secondaryColor: palette.soft,
        secondaryTextColor: palette.ink,
        secondaryBorderColor: palette.accent,
        tertiaryColor: palette.cluster,
        tertiaryTextColor: palette.sub,
        tertiaryBorderColor: palette.clusterBorder,
        mainBkg: palette.node,
        textColor: palette.ink,
        nodeTextColor: palette.ink,
        lineColor: palette.edge,
        defaultLinkColor: palette.edge,
        clusterBkg: palette.cluster,
        clusterBorder: palette.clusterBorder,
        titleColor: palette.sub,
        edgeLabelBackground: palette.background,
        actorBkg: palette.node,
        actorBorder: palette.nodeBorder,
        actorTextColor: palette.ink,
        actorLineColor: palette.edge,
        signalColor: palette.edge,
        signalTextColor: palette.ink,
        noteBkgColor: palette.cluster,
        noteBorderColor: palette.clusterBorder,
        noteTextColor: palette.ink,
        activationBkgColor: palette.soft,
        activationBorderColor: palette.accent,
      },
      themeCSS: ".node rect {rx:7;ry:7;stroke-width:1px;} .cluster rect {rx:9;ry:9;stroke-width:1px;} .cluster-label {font-weight:500;}",
      flowchart: { useMaxWidth: false, curve: "basis", nodeSpacing: 24, rankSpacing: 40, padding: 14 },
      htmlLabels: false,
      maxTextSize: 50000,
      maxEdges: 500,
      secure: ["secure", "securityLevel", "startOnLoad", "suppressErrorRendering",
        "maxTextSize", "maxEdges", "htmlLabels", "theme", "themeVariables", "themeCSS", "look", "fontFamily"],
    });
    await rejectAutomaticImages(runtime, source.textContent);
    if (!card.isConnected || generations.get(card) !== generation) return;
    // Mermaid measures text while drawing. A separate, hidden host keeps its temporary
    // SVG and parser failures out of the transcript's scroll geometry.
    scratch = document.createElement("div");
    scratch.style.cssText = "position:absolute;left:-100000px;top:0;visibility:hidden";
    document.body.append(scratch);
    const { svg } = await runtime.render(`transcript-mermaid-${++nextId}`, source.textContent, scratch);
    if (!card.isConnected || generations.get(card) !== generation) return;
    diagram.innerHTML = svg;
    const rendered = diagram.querySelector("svg");
    const width = rendered?.viewBox.baseVal.width;
    if (Number.isFinite(width) && width > 0) {
      // Native diagram pixels preserve label size. Wide graphs scroll inside the fence
      // instead of silently shrinking every label to fit the reading column.
      rendered.style.width = `${width}px`;
      rendered.style.maxWidth = "none";
    }
    diagram.style.background = palette.background;
    if (!errorBox.hidden) showMermaidSource(card, false);
    errorBox.hidden = true;
    card.dataset.mermaidState = "ready";
    button.disabled = false;
  } catch (error) {
    if (!card.isConnected || generations.get(card) !== generation) return;
    errorBox.textContent = `Could not render Mermaid diagram: ${String(error?.message || error).slice(0, 240)}`;
    errorBox.hidden = false;
    source.hidden = false;
    diagram.hidden = true;
    button.textContent = "source";
    button.disabled = true;
    button.setAttribute("aria-pressed", "true");
    card.dataset.mermaidState = "error";
  } finally {
    scratch?.remove();
    if (generations.get(card) === generation) diagram.setAttribute("aria-busy", "false");
  }
}

/** Render Mermaid fences inside a newly mounted transcript root. Repeated calls are
 *  idempotent for the current theme. Detached messages never receive an async result;
 *  remounted messages render from their own source with unique SVG identifiers.
 *  readerReshaped is the host viewport's callback for user-controlled height changes. */
function renderMermaid(root, readerReshaped) {
  const cards = [...root.querySelectorAll("[data-mermaid]")];
  if (root.matches?.("[data-mermaid]")) cards.unshift(root);
  if (!cards.length) return;
  if (!themeObserver) {
    themeObserver = new MutationObserver(() => renderMermaid(document));
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  }
  const theme = diagramTheme();
  for (const card of cards) {
    if (generations.get(card)?.theme === theme) continue;
    bindSource(card, readerReshaped);
    const generation = { theme };
    generations.set(card, generation);
    card.dataset.mermaidState = "rendering";
    card.querySelector(".mermaid-diagram").setAttribute("aria-busy", "true");
    queue = queue.then(() => drawMermaid(card, generation));
  }
}

export { renderMermaid, revealMermaidSource };
