// The two-stamp file rule — what a clicked attachment or path may DO — is the shared module's
// (html/shared/capabilities.js, #46), read here and by the classic page alike.
import { attachmentCapability, referenceAction, revealQuery } from "./shared/capabilities.js";
import { svg } from "./icons.js";
import { escapeText, partsHtml } from "./view-model.js";
import { rememberCap } from "./shared/parts.js";

const element = html => {
  const template = document.createElement("template");
  template.innerHTML = html.trim();
  const root = template.content.firstElementChild;
  for (const table of root.querySelectorAll(".markdown table")) {
    const scroll = document.createElement("div");
    scroll.className = "markdown-table-scroll";
    table.before(scroll);
    scroll.append(table);
  }
  return root;
};

/** The image attachments a reader expanded to a thumbnail (#80), by record id — page-session
 *  state, so a re-render keeps them open. */
const openImages = new Set();

/** A tool view's body with the reader's cap state applied (#108): the raw parts render at
 *  render time, so an expander opened before comes back open. */
function bodyHtml(view, state) {
  return view.parts ? partsHtml(view.parts, view.id, state) : view.html;
}

function rendererBody(view, state) {
  if (view.renderer === "fallback") {
    return `<div class="renderer-fallback"><div class="renderer-fallback-row"><span>record</span><code class="fallback-raw">${escapeText(JSON.stringify(view.raw, null, 2))}</code></div></div>`;
  }
  if (view.renderer === "queue") {
    // The queued prompt's own words, as the classic page's "⧗ queued: …" marker shows them
    // (#65): the record's body is the text the user typed while the agent was busy, rendered as
    // markdown by the same pipeline as a turn; a bare "Queued input" label told the reader
    // nothing about WHAT waits. The label stays as the mark; the text follows it.
    const text = view.html ? `<div class="renderer-queue-text">${view.html}</div>` : `<small>no text recorded</small>`;
    return `<div class="renderer-queue"><span class="renderer-queue-mark" aria-hidden="true"></span><div class="renderer-queue-copy"><strong>${escapeText(view.summary || "Queued input")}</strong>${text}</div></div>`;
  }
  if (view.renderer === "agent") {
    return `<div class="renderer-agent"><div class="renderer-agent-section renderer-agent-result"><span class="renderer-agent-label">Agent event</span><div class="renderer-agent-copy">${view.html || "No additional details recorded."}</div></div>${view.childId ? `<button class="renderer-agent-open" type="button" data-child-session="${escapeText(view.childId)}">Open child transcript</button>` : ""}</div>`;
  }
  if (view.renderer === "task") {
    return `<div class="renderer-task"><div class="renderer-task-head"><span class="renderer-task-state ${view.running ? "running" : "completed"}"></span><strong>${escapeText(view.summary || view.name)}</strong></div>${view.html ? `<div class="renderer-task-detail">${view.html}</div>` : ""}</div>`;
  }
  if (view.interaction?.kind === "request_user_input") {
    const answers = (view.interaction.answers || []).map(answer => `<span class="input-answer"><span>${escapeText(answer.label)}</span><small>${escapeText(answer.id)}</small></span>`).join("");
    const fallback = view.interaction.resolved ? "Answered in the agent client" : "Please return to the agent client to answer; Monitor cannot submit this native prompt.";
    return `<div class="input-request ${view.interaction.resolved ? "resolved" : "waiting"}"><span class="input-request-icon" aria-hidden="true">${view.interaction.resolved ? "✓" : "?"}</span><div class="input-request-copy"><strong>${view.interaction.resolved ? "User input received" : "Waiting for user input"}</strong><p>${escapeText(view.summary || fallback)}</p>${view.summary ? `<small class="input-request-meta">${escapeText(fallback)}</small>` : ""}${answers ? `<div class="input-answers">${answers}</div>` : ""}</div></div>`;
  }
  if (view.attachment) {
    const h = view.attachment;
    const capability = attachmentCapability(h);
    // An image attachment (#80): collapsed to a line at first — a transcript with a hundred
    // screenshots must not be a hundred images — the first click expands it to an inline
    // thumbnail, and the thumbnail opens the full-size lightbox. The classic page shows the
    // image inline; here the reader chooses, per image, and the choice holds through re-renders.
    if (capability.action === "image") {
      const source = h.att_datauri || `/file?path=${encodeURIComponent(h.att_path || "")}&sig=${encodeURIComponent(h.att_fsig || "")}`;
      const name = h.att_name || "image";
      const open = openImages.has(view.id);
      const attrs = `data-attachment="${escapeText(view.id || "")}" data-attachment-action="image" data-path="${escapeText(h.att_path || "")}" data-fsig="${escapeText(h.att_fsig || "")}" data-sig="${escapeText(h.att_sig || "")}"`;
      return `<div class="renderer-image ${open ? "open" : ""}" data-image-block="${escapeText(view.id || "")}"><button type="button" class="renderer-image-toggle" data-image-toggle="${escapeText(view.id || "")}" aria-expanded="${open}">${open ? "Hide" : "Show"} image · ${escapeText(name)}</button>${open ? `<figure class="renderer-image-figure"><button type="button" class="renderer-image-thumb" ${attrs} title="Open ${escapeText(name)} at full size"><img src="${escapeText(source)}" alt="${escapeText(name)}" decoding="async"></button><figcaption>${escapeText(name)} · click for full size</figcaption></figure>` : ""}</div>`;
    }
    return `<div class="renderer-note"><strong>${escapeText(h.att_kind || "file")} · ${escapeText(h.att_name || "attachment")}</strong><p>${capability.action === "copy" ? "This session kept only the original file path." : ""}</p><button class="artifact-link" data-attachment="${escapeText(view.id || "")}" data-attachment-action="${capability.action}" data-path="${escapeText(h.att_path || "")}" data-fsig="${escapeText(h.att_fsig || "")}" data-sig="${escapeText(h.att_sig || "")}">${escapeText(capability.label)} →</button>${capability.action !== "reveal" && h.att_path && h.att_sig ? `<button class="artifact-link artifact-link-secondary" data-attachment="${escapeText(view.id || "")}" data-attachment-action="reveal" data-path="${escapeText(h.att_path)}" data-sig="${escapeText(h.att_sig)}">Reveal in file manager</button>` : ""}</div>`;
  }
  if (view.renderer === "bash") return `<div class="renderer-terminal ${view.error ? "error" : ""}"><span class="output">${bodyHtml(view, state) || "No output recorded"}</span></div>`;
  return bodyHtml(view, state) || `<div class="renderer-note"><p>No additional details recorded.</p></div>`;
}

function renderRenderer(view, index, state) {
  const children = view.children?.length ? `<div class="renderer-children">${view.children.map((child, childIndex) => renderRenderer(child, `${index}.${childIndex}`, state)).join("")}</div>` : "";
  const key = view.id || String(index);
  // Process surfaces stay open, while their completed details start as compact rows. Native
  // input requests and queued/running work remain open because they need immediate attention.
  const defaultClosed = rendererStartsClosed(view);
  const closed = state.folds.has(key) ? state.folds.get(key) : defaultClosed;
  const noninteractive = view.renderer === "thinking" && !view.html && !children;
  const title = view.name || view.renderer || "Record";
  const status = view.error ? "failed" : view.running ? "running" : view.state || (view.renderer === "thinking" ? "reasoning" : "completed");
  const head = noninteractive
    ? `<div class="renderer-head" aria-label="Thinking recorded"><span class="renderer-chevron"></span><span class="renderer-title">${escapeText(title)}</span><span class="renderer-target"></span><span class="renderer-state"></span></div>`
    : `<button class="renderer-head" type="button" aria-expanded="${!closed}"><span class="renderer-chevron"></span><span class="renderer-title">${escapeText(title)}</span>${view.path && (view.fileSig || view.revealSig) ? `<span class="renderer-target renderer-target-link" data-reference-path="${escapeText(view.path)}" data-reference-fsig="${escapeText(view.fileSig || "")}" data-reference-sig="${escapeText(view.revealSig || "")}" title="${referenceAction(view) === "preview" ? "Open in the preview pane" : "Reveal in file manager"}">${escapeText(view.summary || "")}</span>` : `<span class="renderer-target">${escapeText(view.summary || "")}</span>`}<span class="renderer-state" title="${escapeText(status)}">${escapeText(status === "completed" ? view.duration || "" : status)}</span></button>`;
  const toolName = view.t === "tool" ? ` data-tool-name="${escapeText(view.name)}"` : "";
  return `<div class="turn assistant renderer-turn" data-kind="${escapeText(view.t)}"${toolName} data-block-index="${escapeText(index)}"><div class="renderer ${noninteractive ? "noninteractive" : closed ? "closed" : ""}" data-renderer data-record-id="${escapeText(key)}" data-renderer-kind="${escapeText(view.renderer)}" data-state="${escapeText(status)}">${head}${view.children?.length ? `<button class="renderer-children-toggle" type="button" data-renderer-children-bulk aria-pressed="false" title="Expand all nested levels">${svg("expandStack")}</button>` : ""}${noninteractive ? "" : `<div class="renderer-body"><div class="renderer-output">${rendererBody(view, state)}</div>${children}</div>`}</div></div>`;
}

export const rendererStartsClosed = view => !view.running && !view.interaction && view.renderer !== "queue";

function renderProcess(unit, state) {
  const key = unit.key;
  const closed = state.processFolds.get(key) || false;
  const expanded = state.processExpanded.has(key);
  const visibleLimit = 7;
  const failed = unit.views.some(({ view }) => view.error);
  const running = unit.views.some(({ view }) => view.running);
  const queued = unit.views.some(({ view }) => view.renderer === "queue");
  const tone = failed ? "failed" : running ? "running" : queued ? "queued" : "completed";
  const updates = unit.views.filter(({ view }) => view.t === "assistant").length;
  const labels = unit.views.slice(0, 4).map(({ view }) => view.t === "assistant" ? `Progress · ${strip(view.html).slice(0, 42)}` : view.name || view.t);
  const preview = labels.join(" · ") + (unit.views.length > 4 ? ` · +${unit.views.length - 4}` : "");
  const events = unit.views.map(({ index, view }, position) => {
    const content = view.t === "assistant"
      ? `<div class="turn assistant process-commentary" data-kind="assistant" data-phase="commentary" data-block-index="${index}"><span class="process-commentary-mark"></span><span class="process-commentary-label">Progress</span><div class="process-commentary-copy markdown">${view.html}</div></div>`
      : renderRenderer(view, index, state);
    return `<div class="process-event ${position >= visibleLimit && !expanded ? "progressive-hidden" : ""}" data-progressive="${position >= visibleLimit}">${content}</div>`;
  }).join("");
  const hidden = Math.max(0, unit.views.length - visibleLimit);
  return `<section class="process-surface process-${tone} ${closed ? "closed" : ""}" data-process-surface data-process-key="${escapeText(key)}" data-process-state="${tone}" aria-label="Agent process"><div class="process-surface-headbar"><div class="process-surface-summary"><button class="process-section-toggle" type="button" data-process-toggle aria-expanded="${!closed}" title="${closed ? "Expand" : "Collapse"} this section">${svg("chev")}</button><span class="process-surface-label">Agent process</span><span class="process-surface-preview">${escapeText(preview)}</span><span class="process-surface-count">${unit.views.length} events${updates ? ` · ${updates} updates` : ""}</span><button class="process-bulk-toggle" type="button" data-process-bulk aria-pressed="false" title="Expand every detail in this section">${svg("expandStack")}</button></div></div><div class="process-surface-body">${events}${hidden ? `<button class="process-more" type="button" data-process-more aria-expanded="${expanded}"><span>${expanded ? "Show fewer" : `Show ${hidden} more`}</span>${svg("chev")}</button>` : ""}</div></section>`;
}

/** A turn shown as the record the wire carried (parity #8) — the classic `{}` toggle. What the
 *  page has is the record, not markdown source, so "raw" is the record itself, exactly as the
 *  fallback renderer already shows an unknown kind. */
export const rawTurnHtml = record => `<pre class="turn-raw">${escapeText(JSON.stringify(record ?? {}, null, 2))}</pre>`;

/** A user turn as the text the reader typed (#109): the wire's `src`, exactly, whitespace
 *  intact — the classic page's `{}`. A record without one falls back to the record view. */
export const rawTextHtml = record => typeof record?.src === "string"
  ? `<pre class="turn-raw turn-raw-text">${escapeText(record.src)}</pre>`
  : rawTurnHtml(record);

/** Whether this turn shows raw: the per-turn override if there is one, else — for a user turn —
 *  the global preference; an assistant turn has no global. */
export function rawFor(unit, state) {
  if (state.rawTurns?.has(unit.key)) return !!state.rawTurns.get(unit.key);
  return unit.type === "user" && !!state.rawUser;
}

export function renderUnit(unit, state) {
  const spot = unit.view?.id
    ? `<button class="spot-link" type="button" data-spot-link="${escapeText(unit.view.id)}" aria-label="Copy a link to here" title="Copy a link to here"><span aria-hidden="true">#</span></button>`
    : "";
  const raw = rawFor(unit, state);
  const rawWhat = unit.type === "user" ? "as raw text — exactly as typed" : "as the raw record";
  const rawToggle = unit.view?.source && (unit.type === "user" || unit.type === "assistant")
    ? `<button class="spot-link raw-toggle ${raw ? "on" : ""}" type="button" data-raw-toggle="${escapeText(unit.key)}" aria-pressed="${raw}" aria-label="${raw ? "Show this turn rendered" : `Show this turn ${rawWhat}`}" title="${raw ? "Show this turn rendered" : `Show this turn ${rawWhat}`}"><span aria-hidden="true">{}</span></button>`
    : "";
  const body = raw ? (unit.type === "user" ? rawTextHtml(unit.view.source) : rawTurnHtml(unit.view.source)) : unit.view?.html;
  let html;
  if (unit.type === "user") {
    const long = promptShouldCollapse(unit.view.html);
    const expanded = state.promptExpanded.has(unit.key);
    html = `<div class="turn user" data-kind="user" data-block-index="${unit.from}" data-turn="${unit.turn}"><div class="user-prompt ${long ? "prompt-collapsible" : ""}"><div class="prompt-copy-shell ${long && !expanded ? "collapsed" : "expanded"}"><div class="body markdown">${body}</div>${long ? `<button class="prompt-expand" type="button" data-prompt-toggle="${escapeText(unit.key)}" aria-expanded="${expanded}">${expanded ? "Show fewer" : "Show the whole prompt"}<span aria-hidden="true">${expanded ? "⌃" : "⌄"}</span></button>` : ""}</div>${renderPromptAttachments(unit.attachments)}</div>${spot}${rawToggle}</div>`;
  }
  else if (unit.type === "assistant") {
    const final = unit.view.phase === "final";
    const plan = unit.view.presentation === "proposed_plan";
    html = `<div class="turn assistant ${final ? "final-answer" : "assistant-unknown"} ${plan ? "proposed-plan" : ""}" data-kind="assistant" data-phase="${escapeText(unit.view.phase)}" data-block-index="${unit.from}">${plan ? `<div class="proposed-plan-head"><span class="proposed-plan-icon">${svg("turns")}</span><div><span>Proposed plan</span><small>Review before implementation</small></div></div>` : final ? "" : '<span class="assistant-phase-label">Assistant message</span>'}<div class="body markdown">${body}</div>${spot}${rawToggle}</div>`;
  } else html = renderProcess(unit, state);
  const root = element(html);
  // The record id is the renderer's stable deep-link contract (`b<N>` today). Virtualized
  // units are materialized on demand before navigation, so the id belongs on the unit root.
  if (spot) root.id = unit.view.id;
  root.dataset.unitKey = unit.key;
  root.dataset.unitFrom = unit.from;
  return root;
}

export const promptShouldCollapse = html => strip(html).length > 560;

function renderPromptAttachments(attachments = []) {
  if (!attachments.length) return "";
  const cards = attachments.map(view => {
    const h = view.attachment || {};
    const capability = attachmentCapability(h);
    const isImage = capability.action === "image";
    const source = h.att_datauri || (h.att_path && h.att_fsig ? `/file?path=${encodeURIComponent(h.att_path)}&sig=${encodeURIComponent(h.att_fsig)}` : "");
    const action = `data-attachment="${escapeText(view.id || "")}" data-attachment-action="${capability.action}" data-path="${escapeText(h.att_path || "")}" data-fsig="${escapeText(h.att_fsig || "")}" data-sig="${escapeText(h.att_sig || "")}"`;
    if (isImage && source) return `<button class="prompt-attachment prompt-image" type="button" ${action} title="Enlarge ${escapeText(h.att_name || "image")}"><span class="prompt-image-thumb"><img src="${escapeText(source)}" alt=""></span><span class="prompt-file-copy"><strong>${escapeText(h.att_name || "image")}</strong><small>${escapeText(capability.hint)}</small></span><span class="prompt-file-open" aria-hidden="true">⤢</span></button>`;
    const ext = String(h.att_name || "file").split(".").pop().slice(0, 4).toUpperCase();
    const glyph = capability.action === "download" ? "↓" : capability.action === "copy" ? "⎘" : "↗";
    return `<button class="prompt-attachment prompt-file" type="button" ${action}><span class="prompt-file-icon">${escapeText(ext)}</span><span class="prompt-file-copy"><strong>${escapeText(h.att_name || "Attachment")}</strong><small>${escapeText(capability.hint)}</small></span><span class="prompt-file-open" aria-hidden="true">${glyph}</span></button>`;
  }).join("");
  return `<div class="prompt-attachments" aria-label="Prompt attachments">${cards}</div>`;
}

const strip = html => String(html || "").replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();

export function bindComponentEvents(root, state, actions) {
  root.addEventListener("click", event => {
    const rawToggle = event.target.closest("[data-raw-toggle]");
    if (rawToggle) { event.preventDefault(); event.stopPropagation(); const key = rawToggle.dataset.rawToggle; state.rawTurns.set(key, rawToggle.getAttribute("aria-pressed") !== "true"); actions.rerender(); return; }
    const spot = event.target.closest("[data-spot-link]");
    if (spot) { event.preventDefault(); event.stopPropagation(); actions.copySpot?.(spot.dataset.spotLink, spot); return; }
    const copy = event.target.closest(".cpy");
    if (copy) {
      const code = copy.closest(".fence")?.querySelector("pre");
      if (code) {
        const original = copy.textContent;
        const operation = navigator.clipboard?.writeText(code.textContent || "");
        if (!operation) { actions.toast?.("This browser does not support copying"); return; }
        operation.then(() => {
          copy.textContent = "Copied";
          setTimeout(() => { if (copy.isConnected) copy.textContent = original; }, 1200);
        }, () => actions.toast?.("Could not copy the code"));
      }
      return;
    }
    const offered = event.target.closest("[data-reference-path]");
    if (offered) {
      event.preventDefault(); event.stopPropagation();
      actions.openReferenceOffer?.({ path: offered.dataset.referencePath, fileSig: offered.dataset.referenceFsig || "", revealSig: offered.dataset.referenceSig || "" });
      return;
    }
    const reference = event.target.closest(".markdown a");
    if (reference) {
      const href = reference.getAttribute("href") || "";
      if (href.startsWith("/") || href.startsWith("file://")) {
        event.preventDefault();
        actions.openReference?.(href.replace(/^file:\/\//, ""));
        return;
      }
    }
    const child = event.target.closest("[data-child-session]");
    if (child) { actions.openChild(child.dataset.childSession); return; }
    // A cap expander (#108): reveal in place — the rows are already there — and remember a
    // small expansion so a re-render keeps it open; the viewport's observer measures the growth.
    const capMore = event.target.closest("[data-cap-more]");
    if (capMore) {
      event.preventDefault(); event.stopPropagation();
      const scope = capMore.closest(".codebox, .renderer-terminal, .renderer-body") || capMore.parentElement;
      const hidden = scope?.querySelector(`[data-cap-id="${CSS.escape(capMore.dataset.capMore)}"]`);
      if (hidden) hidden.classList.add("shown");
      rememberCap(state.capOpen, capMore.dataset.capRecord, capMore.dataset.capOrd, Number(capMore.dataset.capLines) || 0);
      capMore.remove();
      return;
    }
    const imageToggle = event.target.closest("[data-image-toggle]");
    if (imageToggle) { const id = imageToggle.dataset.imageToggle; openImages.has(id) ? openImages.delete(id) : openImages.add(id); actions.rerender?.(); return; }
    const attachment = event.target.closest("[data-attachment]");
    if (attachment) { actions.openAttachment(attachment.dataset.attachment, attachment.dataset.path, attachment.dataset.fsig, attachment.dataset.attachmentAction, attachment.dataset.sig); return; }
    const prompt = event.target.closest("[data-prompt-toggle]");
    if (prompt) { const key = prompt.dataset.promptToggle; state.promptExpanded.has(key) ? state.promptExpanded.delete(key) : state.promptExpanded.add(key); actions.rerender(); return; }
    const process = event.target.closest("[data-process-surface]");
    if (process && event.target.closest("[data-process-toggle]")) {
      const key = process.dataset.processKey;
      state.processFolds.set(key, !process.classList.contains("closed"));
      actions.rerender(); return;
    }
    if (process && event.target.closest("[data-process-more]")) {
      const key = process.dataset.processKey;
      state.processExpanded.has(key) ? state.processExpanded.delete(key) : state.processExpanded.add(key);
      actions.rerender(); return;
    }
    if (process && event.target.closest("[data-process-bulk]")) {
      const renderers = process.querySelectorAll("[data-renderer]:not(.noninteractive)");
      const open = [...renderers].some(renderer => renderer.classList.contains("closed"));
      renderers.forEach(renderer => state.folds.set(renderer.dataset.recordId, !open));
      actions.rerender(); return;
    }
    const renderer = event.target.closest("[data-renderer]");
    if (renderer && event.target.closest("[data-renderer-children-bulk]")) {
      const descendants = renderer.querySelectorAll(".renderer-children [data-renderer]:not(.noninteractive)");
      const open = [...descendants].some(item => item.classList.contains("closed"));
      descendants.forEach(item => state.folds.set(item.dataset.recordId, !open));
      actions.rerender(); return;
    }
    if (renderer && event.target.closest(".renderer-head") && !renderer.classList.contains("noninteractive")) {
      state.folds.set(renderer.dataset.recordId, !renderer.classList.contains("closed"));
      actions.rerender();
    }
  });
}
