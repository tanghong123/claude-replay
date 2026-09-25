// The request-user-input card both pages draw (#121, design/rendering-parity-audit.md row 3.17).
// When an agent asks the reader a question through its own client, the server projects the call
// into `head.interaction` — `{kind: "request_user_input", resolved, answers: [{id, label}]}`
// (html_export/mod.rs `request_user_input_projection`), and a Claude `AskUserQuestion` adds
// `asked: [{header, question, multi, options: [{label, description, chosen}], typed?, notes?}]`
// — every question put, and what the reader said to each (#255, #280; `asked_section`). A call
// that came back WITHOUT an answer is `resolved` too, with `unanswered: {why, seconds?}` saying
// why — timed out, declined, failed, or none (#281).
// Monitor cannot answer a native prompt, so the card's job is to say WHERE the answer goes and,
// once it has been given, WHAT it was.
// The app shell had this card; the classic page showed a generic tool fold. Now the words, the
// states and the markup are here, and each page passes its own class names.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

const escapeInteraction = value => String(value ?? "").replace(/[&<>"']/g, ch => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch]);

/** The three states, in the words the reader sees. */
const WAITING_TITLE = "Waiting for user input";
const RESOLVED_TITLE = "User input received";
const WAITING_NOTE = "Please return to the agent client to answer; Monitor cannot submit this native prompt.";
const RESOLVED_NOTE = "Answered in the agent client";
/** #281: a call that came back with NO answer is not waiting either — it says why. */
const UNANSWERED_TITLE = "No answer";
const unansweredNote = ({ why, seconds } = {}) =>
  why === "timeout"
    ? `The agent client stopped waiting${seconds ? ` after ${seconds}s` : ""}; the agent went on without an answer.`
    : why === "declined"
      ? "Declined in the agent client; the agent was told not to proceed."
      : why === "failed"
        ? "The question failed before it was answered."
        : "The call came back without an answer.";
/** #280: what the reader wrote themselves, captioned so it never reads as one of the options. */
const TYPED_CAPTION = "Typed answer";
const NOTES_CAPTION = "Notes";

/** Is this head a request for user input? Anything else renders as an ordinary call. */
function isInteraction(interaction) {
  return !!interaction && interaction.kind === "request_user_input";
}

/** What the card says: its state, its title, the note under it and the answers given. */
function interactionCard(interaction, summary) {
  const resolved = !!(interaction && interaction.resolved);
  // #281: `unanswered` rides only on a call that has come back, and it outranks "received".
  const unanswered = resolved && interaction.unanswered ? interaction.unanswered : null;
  const note = unanswered ? unansweredNote(unanswered) : resolved ? RESOLVED_NOTE : WAITING_NOTE;
  // #255: every question the call put, with every option it offered and which came back.
  // The transcript always had this; the card used to show the first question's text and the
  // labels that were picked, which on a four-question call is a fraction of what was asked —
  // and the options that were DECLINED, where the trade-off is written, never appeared.
  const asked = (interaction && interaction.asked) || [];
  return {
    state: unanswered ? "unanswered" : resolved ? "resolved" : "waiting",
    icon: unanswered ? "–" : resolved ? "✓" : "?",
    title: unanswered ? UNANSWERED_TITLE : resolved ? RESOLVED_TITLE : WAITING_TITLE,
    note,
    // The question itself when the record carries one; the note stands in when it does not,
    // and then moves out of the body so the reader is never told the same thing twice.
    //
    // #280: never when the card lists its questions. The summary is the call's target — the
    // FIRST question, " +1" — so it printed that question twice, once at reading size and once
    // in the note type every question's lead used, and the owner read the second question as a
    // footnote to the first: "the two questions are not rendered equally". Listed, each
    // question is drawn once and alike, and the body says where the answer went.
    text: asked.length ? note : summary || note,
    meta: !asked.length && summary ? note : "",
    answers: (interaction && interaction.answers) || [],
    asked,
  };
}

/** The card's markup, with the page's own class names.
 *
 *  Each question is one `question` block, the same markup for every one of them (#280): its
 *  header (and "any number" on a multi-select) in the note type, the question at reading size,
 *  the options it offered as ANSWER chips — label, then description — with the picks ticked,
 *  and then what the reader wrote themselves, if anything, as a `reply`: the words they typed
 *  instead of picking (ticked, since they ARE the answer) and the notes they attached. Neither
 *  is an option, so neither is drawn as one.
 *
 *  When the questions are present the bare answer list is dropped: a ticked option says what
 *  was chosen AND what it meant, so listing the labels again underneath would only repeat it. */
function interactionHtml(interaction, summary, classes) {
  const card = interactionCard(interaction, summary);
  const chip = (label, note, chosen) =>
    `<span class="${classes.answer}"><span>${chosen ? "✓ " : ""}${escapeInteraction(label)}</span>${note ? `<small>${escapeInteraction(note)}</small>` : ""}</span>`;
  const reply = (caption, text, chosen) =>
    `<div class="${classes.reply}"><small>${escapeInteraction(caption)}</small><span>${chosen ? "✓ " : ""}${escapeInteraction(text)}</span></div>`;
  const asked = card.asked
    .map(q => {
      const options = (q.options || []).map(o => chip(o.label, o.description, o.chosen)).join("");
      const lead = [q.header, q.multi ? "any number" : ""].filter(Boolean).join(" · ");
      return `<div class="${classes.question}">${lead ? `<small class="${classes.meta}">${escapeInteraction(lead)}</small>` : ""}<p>${escapeInteraction(q.question || "")}</p>${options ? `<div class="${classes.answers}">${options}</div>` : ""}${q.typed ? reply(TYPED_CAPTION, q.typed, true) : ""}${q.notes ? reply(NOTES_CAPTION, q.notes, false) : ""}</div>`;
    })
    .join("");
  const answers = asked
    ? ""
    : card.answers.map(a => chip(a.label, a.id, false)).join("");
  return `<div class="${classes.card} ${card.state}"><span class="${classes.icon}" aria-hidden="true">${card.icon}</span><div class="${classes.copy}"><strong>${escapeInteraction(card.title)}</strong><p>${escapeInteraction(card.text)}</p>${card.meta ? `<small class="${classes.meta}">${escapeInteraction(card.meta)}</small>` : ""}${asked}${answers ? `<div class="${classes.answers}">${answers}</div>` : ""}</div></div>`;
}

export { isInteraction, interactionCard, interactionHtml };
