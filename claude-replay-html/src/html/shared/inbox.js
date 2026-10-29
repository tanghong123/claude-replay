// A relayed message from the person, verified (#s272): Hong's DingTalk messages, dashboard replies
// and card taps, which aries-inbox types into the Aries session. The record is the person's turn
// (`kind: "user"`, their words in its body) with an `inbox` object beside it: who sent it, where
// from, what kind, when, what it is about, a relay's agent and task, what it replies to (`reply_to`)
// and the relay's instructions to the agent (`do`). One vocabulary for both pages: the chips a
// card's head shows, in order, and what each says.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

const INBOX_VIA = { dingtalk: "DingTalk", dashboard: "dashboard", card: "card tap" };

/** The record's `inbox` when it is one a page can draw, else null (the plain prompt is drawn). */
function inboxOf(record) {
  const v = record && record.inbox;
  return v && typeof v === "object" && !Array.isArray(v) && typeof v.from === "string" && v.from ? v : null;
}

/** The head's chips, in order: `{ text, title, cls }`. `fmtTime` formats epoch seconds
 *  (shared/time.js's `fmtTime`), for the time it was sent. */
function inboxChips(inbox, fmtTime) {
  const chips = [];
  const s = v => (typeof v === "string" ? v : "");
  if (s(inbox.via)) chips.push({ text: "via " + (INBOX_VIA[inbox.via] || inbox.via), title: "Where it was written", cls: "via" });
  if (s(inbox.kind)) chips.push({ text: inbox.kind, title: "What kind of message", cls: "kind" });
  for (const a of Array.isArray(inbox.about) ? inbox.about : []) {
    if (s(a)) chips.push({ text: a, title: "The issue it is about", cls: "about" });
  }
  if (s(inbox.to)) {
    const guessed = inbox.guess === "repo";
    chips.push({
      text: "→ " + inbox.to + (guessed ? " ?" : ""),
      title: guessed ? "The agent to relay to, guessed from the repo" : "The agent to relay to",
      cls: "to",
    });
  }
  if (s(inbox.task)) chips.push({ text: inbox.task, title: "The task", cls: "task" });
  if (s(inbox.at)) {
    const ms = Date.parse(inbox.at);
    const shown = Number.isFinite(ms) && fmtTime ? fmtTime(ms / 1000) : "";
    chips.push({ text: "sent " + (shown || inbox.at), title: "Sent " + inbox.at, cls: "at" });
  }
  return chips;
}

/** What the folded instructions' control says. */
const INBOX_DO_LABEL = "Instructions for the agent";

export { inboxOf, inboxChips, INBOX_DO_LABEL };
