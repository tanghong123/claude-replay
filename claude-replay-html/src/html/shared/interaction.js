// The request-user-input card both pages draw (#121, design/rendering-parity-audit.md row 3.17).
// When an agent asks the reader a question through its own client, the server projects the call
// into `head.interaction` — `{kind: "request_user_input", resolved, answers: [{id, label}]}`
// (html_export/mod.rs `request_user_input_projection`). Monitor cannot answer a native prompt,
// so the card's job is to say WHERE the answer goes and, once it has been given, WHAT it was.
// The app shell had this card; the classic page showed a generic tool fold. Now the words, the
// states and the markup are here, and each page passes its own class names.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

const escapeInteraction = value => String(value ?? "").replace(/[&<>"']/g, ch => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch]);

/** The two states, in the words the reader sees. */
const WAITING_TITLE = "Waiting for user input";
const RESOLVED_TITLE = "User input received";
const WAITING_NOTE = "Please return to the agent client to answer; Monitor cannot submit this native prompt.";
const RESOLVED_NOTE = "Answered in the agent client";

/** Is this head a request for user input? Anything else renders as an ordinary call. */
function isInteraction(interaction) {
  return !!interaction && interaction.kind === "request_user_input";
}

/** What the card says: its state, its title, the note under it and the answers given. */
function interactionCard(interaction, summary) {
  const resolved = !!(interaction && interaction.resolved);
  return {
    state: resolved ? "resolved" : "waiting",
    icon: resolved ? "✓" : "?",
    title: resolved ? RESOLVED_TITLE : WAITING_TITLE,
    note: resolved ? RESOLVED_NOTE : WAITING_NOTE,
    // The question itself when the record carries one; the note stands in when it does not,
    // and then moves out of the body so the reader is never told the same thing twice.
    text: summary || (resolved ? RESOLVED_NOTE : WAITING_NOTE),
    meta: summary ? (resolved ? RESOLVED_NOTE : WAITING_NOTE) : "",
    answers: (interaction && interaction.answers) || [],
    // #255: every question the call put, with every option it offered and which came back.
    // The transcript always had this; the card used to show the first question's text and the
    // labels that were picked, which on a four-question call is a fraction of what was asked —
    // and the options that were DECLINED, where the trade-off is written, never appeared.
    asked: (interaction && interaction.asked) || [],
  };
}

/** The card's markup, with the page's own class names.
 *
 *  The questions and their options reuse the ANSWER chip shape — label above, description
 *  below — so the section needs no class of its own on either page. That matters on the app
 *  shell, whose stylesheet is generated from the demo and must not be hand-edited.
 *
 *  When the questions are present the bare answer list is dropped: a ticked option says what
 *  was chosen AND what it meant, so listing the labels again underneath would only repeat it. */
function interactionHtml(interaction, summary, classes) {
  const card = interactionCard(interaction, summary);
  const chip = (label, note, chosen) =>
    `<span class="${classes.answer}"><span>${chosen ? "✓ " : ""}${escapeInteraction(label)}</span>${note ? `<small>${escapeInteraction(note)}</small>` : ""}</span>`;
  const asked = card.asked
    .map(q => {
      const options = (q.options || []).map(o => chip(o.label, o.description, o.chosen)).join("");
      const lead = (q.header ? q.header + " · " : "") + (q.question || "") + (q.multi ? " (any number)" : "");
      return `<small class="${classes.meta}">${escapeInteraction(lead)}</small>${options ? `<div class="${classes.answers}">${options}</div>` : ""}`;
    })
    .join("");
  const answers = asked
    ? ""
    : card.answers.map(a => chip(a.label, a.id, false)).join("");
  return `<div class="${classes.card} ${card.state}"><span class="${classes.icon}" aria-hidden="true">${card.icon}</span><div class="${classes.copy}"><strong>${escapeInteraction(card.title)}</strong><p>${escapeInteraction(card.text)}</p>${card.meta ? `<small class="${classes.meta}">${escapeInteraction(card.meta)}</small>` : ""}${asked}${answers ? `<div class="${classes.answers}">${answers}</div>` : ""}</div></div>`;
}

export { isInteraction, interactionCard, interactionHtml };
