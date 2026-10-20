import { escapeText } from "./model.js";

const QUESTION_METHOD = "item/tool/requestUserInput";
const APPROVAL_METHODS = new Set(["item/commandExecution/requestApproval", "item/fileChange/requestApproval"]);

export function requestKind(request) {
  if (request.canRespond === false || request.method === "item/tool/call") return "desktop";
  if (request.method === QUESTION_METHOD) return "questions";
  if (APPROVAL_METHODS.has(request.method)) return "approval";
  if (request.method === "item/permissions/requestApproval") return "permissions";
  return "unsupported";
}

/** Only native decisions advertised by this request are offered. Broader grants are omitted. */
export function approvalDecisions(request) {
  const advertised = request.params?.availableDecisions;
  const allowed = Array.isArray(advertised) ? advertised.filter(decision => typeof decision === "string") : ["accept", "decline"];
  return ["decline", "accept"].filter(decision => allowed.includes(decision));
}

export function questionResponse(request, values) {
  const answers = {};
  for (const question of request.params?.questions || []) {
    const value = values[question.id];
    const texts = (Array.isArray(value) ? value : [value]).map(text => String(text || "").trim()).filter(Boolean);
    if (!texts.length) throw new Error(`Answer “${question.header || question.question || question.id}” before submitting`);
    answers[question.id] = { answers: texts };
  }
  if (!Object.keys(answers).length) throw new Error("This request has no supported questions");
  return { answers };
}

export function permissionResponse(request, allow) {
  return { permissions: allow ? request.params?.permissions || {} : {}, scope: "turn" };
}

/** Bind an answer to the exact Gateway request, including native ID type. */
export function requestResponsePayload(request, response) {
  if (!request || typeof request.requestToken !== "string" || !request.requestToken || typeof request.params?.threadId !== "string" || !(typeof request.id === "string" || Number.isSafeInteger(request.id))) throw new Error("This runtime request needs a fresh task snapshot before it can be answered");
  return { threadId: request.params.threadId, requestId: request.id, requestToken: request.requestToken, response };
}

export function requestHtml(request, index, disabled = false) {
  const kind = requestKind(request);
  const params = request.params || {};
  const disable = disabled ? " disabled" : "";
  const base = `class="ws-request ws-${kind}" data-testid="${kind === "questions" ? "question-card" : kind === "approval" || kind === "permissions" ? "approval-card" : "desktop-tool-card"}" data-request-index="${index}" data-request-id="${escapeText(request.id)}" data-request-token="${escapeText(request.requestToken || "")}"`;
  if (kind === "desktop" || kind === "unsupported") return `<article ${base}><strong>${kind === "desktop" ? "Codex desktop is handling this tool" : "Open Codex desktop to handle this request"}</strong><p>${escapeText(params.tool || request.method)}</p><small>This browser observes this request.</small></article>`;
  const heading = kind === "questions" ? "Codex needs your answer" : kind === "permissions" ? "Codex requests permissions" : request.method.includes("fileChange") ? "Approve file changes" : "Approve command execution";
  const details = params.command || params.reason || params.cwd || "";
  if (kind === "questions") {
    const fields = (params.questions || []).map((question, questionIndex) => `<fieldset><legend>${escapeText(question.header || "Question")}<span>${escapeText(question.question || "")}</span></legend>${(question.options || []).map(option => `<label class="ws-question-option"><input type="radio" name="q-${index}-${questionIndex}" value="${escapeText(option.label)}" data-question-id="${escapeText(question.id)}"${disable}><span>${escapeText(option.label)}<small>${escapeText(option.description || "")}</small></span></label>`).join("")}<label class="ws-other-answer">${question.options?.length ? "Other answer" : "Your answer"}<input type="${question.isSecret ? "password" : "text"}" data-question-id="${escapeText(question.id)}" data-free-answer placeholder="Type your answer" autocomplete="off"${disable}></label></fieldset>`).join("");
    return `<form ${base}><strong>${heading}</strong>${fields}<button class="ws-primary" type="submit" data-testid="submit-answer"${disable}>Submit answer</button><small>The first answer from either client resolves this request.</small></form>`;
  }
  const choices = kind === "permissions" ? ["decline", "accept"] : approvalDecisions(request);
  const permissions = kind === "permissions" ? `<pre>${escapeText(JSON.stringify(params.permissions || {}, null, 2))}</pre>` : "";
  return `<article ${base}><strong>${heading}</strong><p>${escapeText(params.reason || "")}</p>${details ? `<pre>${escapeText(details)}</pre>` : ""}${permissions}<div class="ws-request-actions">${choices.map(decision => `<button type="button" data-decision="${decision}" data-testid="${decision === "accept" ? "approve-request" : "deny-request"}" class="${decision === "accept" ? "ws-primary" : ""}"${disable}>${decision === "accept" ? kind === "permissions" ? "Allow for this turn" : "Allow once" : "Deny"}</button>`).join("")}</div><small>The first response from either client resolves this request.</small></article>`;
}
