# Annotation replies as quotations

*Reading model, implementation status updated 2026-10-16 after review. The first delivery
covers Agent Monitor's App shell Turns list and opened user messages, including phone
portrait/landscape layouts in both themes. The recognition rules and additive data
contract below define the implementation contract. Standalone export is the remaining
presentation follow-up; the [Classic retirement plan](classic-view-deprecation.md)
supersedes adapting that view. Opened replies reuse the existing Markdown
quotation and paragraph styles; Turns retain their quotation marker and additional-item count.*

---

## What was asked for

The requester's words, translated into English:

> "For Agent Monitor's annotation replies, can the left-hand rendering show the
> annotation content? A small icon could indicate that this is a quoted reply,
> such as the vertical bar often used in Markdown."

Two goals, and they belong together: **the Turns list should show what the user
said, and make it apparent that they were replying to a selected passage.**

An annotation reply has two voices. The quotation is a passage selected from an
earlier answer. The annotation, when present, is the user's comment on that passage.
A reader needs to recognize both and understand which words belong to whom. A
selection can also stand alone, without a comment or an additional request.

The additional request, when explicitly delimited, belongs to the same user
message. It may apply to every annotation in that message. Claude's observed format
does not distinguish the final quotation's comment from an additional request;
the viewer must preserve that text without claiming to know its scope.

---

## What is there today

The screenshot shows this in the Turns list:

```text
18 · # Response annotations: Each item...
```

The useful comment is inside the message, after the explanatory text and the
annotation packaging. The reader has to open it and find their own words again.
Several annotation replies can therefore have almost identical list entries,
even when the comments ask different questions.

The goal is to make the comment recognizable from the list and to make the
quotation/comment relationship readable when the message is opened.

---

## Which clients and messages this covers

The scope is replies quoting an earlier assistant answer, from two observed client
formats. These are ordinary user messages containing client-generated text, not
dedicated native annotation message types.

| Client evidence | Native location of the quoted reply |
| --- | --- |
| Codex App `26.1007.21159` | A user `response_item` message's `input_text.text`, starting with the response-annotation envelope. |
| Claude Desktop `2.31226.1`, hosting Claude Code | A user record's `message.content`, starting with a reply marker and Markdown quotation. The inspected records report Claude Code `version: "2.1.295"` and `entrypoint: "claude-desktop"`. |

The desktop application's version and the Claude Code record's version are
different evidence; `2.31226.1` is not the observed Claude Code engine version.
These versions identify the inspected formats, not a promise about all older or
newer clients. They are not a runtime version allowlist: a different client version
with exactly the same envelope can match, while a changed envelope falls back.
No reliable persisted discriminator has been established that proves a user
invoked the quote UI rather than typed the same text.

The [quoted-user-reply validation cases](../docs/adapter-rendering-validation.md#quoted-user-replies)
contain the complete synthetic examples, exact envelopes, version evidence, and
positive/negative controls. Their prose and identifiers are invented for public
documentation; the packaging follows the observed clients. They are not real
conversations, nor evidence that the proposal is already implemented.

File selections, ordinary Markdown blockquotes, and other contextual attachments
do not independently qualify as assistant-answer replies. Context attached to a
recognized reply must still remain visible.

---

## The quoted reply model, drawn

The vertical bar in these sketches means **a quoted reply in the list** and
**selected earlier text in the transcript**. The examples are illustrative.

```text
Turns

17 · Check the session's state.
18 · | Does idle describe the process or the session?...

Opened reply

     | Check whether the process is idle.

     Does idle describe the process or the session?
```

The bar is small and quiet. It marks the relationship to earlier text, while the
user's comment supplies the words that make this reply recognizable.

**One user message remains one turn.** Several annotations in the same message
belong to that turn together. This three-item sketch is a Codex example; the first
Claude recognizer accepts only the observed one- and two-quotation layouts:

```text
Turns

19 · | Why does this message remain in the queue?...  +2

Opened reply

     Annotation 1
     | The message waits in the queue.
     Why does this message remain in the queue?

     Annotation 2
     | The session is idle.
     Can it receive another message now?

     Annotation 3
     | The process is still running.
     What work is it doing?

     Please explain how these three states relate.
```

Here `+2` means two more annotations in this message. It tells the reader that
opening the turn will reveal more than the first comment.

---

## The rules for reading it

| Question | Proposed behavior |
| --- | --- |
| What supplies the Turns label? | The first item's comment, or its following text for Claude. |
| What if there are no user-written words for that item? | The selected passage supplies the label. No empty comment or request is invented. |
| What if there are several annotations? | Keep one row, show the first preview, and show how many more annotations the message contains. |
| What if there is also an additional request? | Keep the first-item preview in the list. Show an explicitly delimited Codex request after the pairs; retain Claude's undelimited text after the final quotation. |
| What does opening the message show? | Every quotation with its associated text immediately below it, in source order. Codex comments are explicit; Claude's following text is displayed without labeling all of it as a comment or a global request. |
| What does clicking the Turns row do? | Take the reader to this user reply. |
| What if the passage spans several lines? | Keep its wording and line breaks readable, including a selection taken from code or a table. |
| What if a preview is long? | Shorten the preview while keeping the quote marker and additional-annotation count visible. |

The selected passage and the comment must remain visually distinct. A comment
should not appear to be part of the earlier answer, and a quotation should not
appear to be a new request authored by the user.

Full content is available by opening the turn. The first delivery must work with
keyboard, mouse, and touch in the App shell, in both themes; hover is supplementary.
The existing phone layouts are covered by the first delivery. Standalone export's new
presentation remains a follow-up. Classic retains its complete ordinary user-message
display until retirement; export and TUI continue to display the complete ordinary text.

---

## When special presentation is allowed

**The known packaging must be matched exactly, and nothing looser.** Recognition
runs on native user-input text at the message boundary, before adapter cleanup.
It does not search arbitrary prose for a heading, tag, or blockquote, or recursively
interpret strings inside a recognized envelope. Match without trimming, case
folding, or CRLF-to-LF normalization.

This section defines the acceptance contract. The
[versioned input examples](../docs/adapter-rendering-validation.md#observed-clients-and-synthetic-input)
provide the literal client packaging and synthetic content used to exercise it.
Examples record format evidence; the rules here decide which inputs qualify.

### Codex exact match

- At the start of the input, require the leading LF, exact `# Response annotations:`
  heading, entire explanatory paragraph, and LF-delimited opening and closing
  `<response-annotations>` tags shown in the versioned example. Matching only the
  heading or tags is insufficient.
- Between the tags, require a nonempty compact JSON array on one line. Every item
  has keys in this order: `text`, optional `annotation`, optional `source`. Unknown
  fields and reordered keys reject the whole candidate.
- `text` is a nonempty string. If present, `annotation` is a nonempty string;
  absence means a selection without a comment.
- If present, `source` has exactly `messageId`, `startOffset`, and `endOffset`, in
  that order: a nonempty string id and nonnegative integer offsets with
  `endOffset > startOffset`. Invalid present source data rejects the candidate;
  it is not silently dropped.
- The array's bytes must equal JavaScript `JSON.stringify` of the validated value
  with that key order. Pretty JSON, formatting whitespace outside strings, and
  equivalent alternate JSON spellings or escapes do not qualify. The guide's
  pretty-printed copy is a reading aid, not an additional accepted format.
- Display the supplied `text` as the quote and preserve string contents literally.
  JSON, headings, or reply markers inside `text` or `annotation` remain content.
  Source offsets do not establish how to slice raw Markdown from another JSONL
  assistant message, so never reconstruct the quote from them.
- Preserve the complete tail after the closing tag, including file and attachment
  context. An explicit `## My request:` boundary can identify an overall request;
  other context may precede it. Extract that request only when the tail can be
  partitioned without hiding or duplicating anything. A comment and an overall
  request are both optional.

### Claude exact match

- At the start of the input, accept only the observed `<!-- reply 1 -->`
  single-quotation layout, or `<!-- reply 2 -->` followed by exactly one standalone
  `<!-- reply -->` second block. Match the literal marker lines, spaces, and LF.
  Do not accept other numbers, an unnumbered first marker, or additional blocks.
  The number's meaning is unconfirmed; this is an allowlist of observed layouts.
- Each marker is immediately followed by consecutive quote lines with the exact
  prefix `> ` and nonempty selected text. Remove one quote prefix per line while
  preserving the selected content and line breaks. An unprefixed blank line
  separates the quotation from its following text.
- In the two-block layout, two LF separate nonempty first following text from the
  bare marker. Markers inside quoted text or fenced code are literal content,
  not item boundaries. Ambiguous or incomplete boundaries reject the whole input.
- Preserve each following-text span and its internal whitespace. Claude has no
  structured boundary between the last quotation's comment and an additional
  request. Three LF before another paragraph do not establish one: the complete
  final span stays in `following_text`, and no overall request is inferred.
- Empty following text is proposed quote-only support, not verified native-client
  output. Keep the quote/following-text separator: a single quote-only input ends
  with two LF after the selected text, followed by EOF. With two blocks, an empty
  first span puts the bare marker directly after that separator, without another
  blank line; an empty final span ends at the separator. Do not repair a missing
  separator or invent comment text. The guide includes a synthetic control for
  this proposed behavior.

### Complete fallback and accepted limitations

For either client, convert the complete message only when it can be split without
losing or duplicating content. Ordinary Markdown blockquotes do not qualify by
themselves. A selection need not have a comment or an overall request.

If a prefix, explanatory sentence, delimiter, required field, or supported layout
changes, recognition fails for the whole message. It receives the existing
ordinary text/Markdown presentation, including any visible client packaging;
there is no partial conversion and no hidden remainder. A user can also inspect
the original input. This fallback is a normal readable message, not a blank row
or an error page.

The unavoidable false positive is a user manually typing or pasting the complete
accepted package at the beginning of a message. Its bytes are indistinguishable
from client-generated text, so it may receive special presentation. A format
change can conversely cause a false negative and show the full ordinary message.
Exact matching reduces accidental matches; it cannot authenticate a UI gesture.
`--unknown` scans record shapes and will not catch these changes inside an already
known text field. Versioned examples and negative controls are required instead.

---

## Parse once; add presentation without breaking existing readers

The Rust Codex and Claude adapters recognize their own native envelopes. They
normalize the result into an agent-neutral addition to the shared `Message`/`Block`
model. The HTML emitter serializes that data; App shell consumes it without
repeating Codex- or Claude-specific text matching in JavaScript.

The proposed HTML record keeps `kind: "user"`, supported `md`/`raw` body parts,
and the original source text, and adds optional `reply_context`. This is
a new viewer field, not a field emitted by either agent client. The following
fragment illustrates the addition, not a complete HTML record; a response and an
overall request are both optional.

```json
{
  "reply_context": {
    "version": 1,
    "items": [
      {
        "quote": "This is a selected excerpt.",
        "response": {
          "kind": "comment",
          "text": "Please explain this point."
        }
      }
    ],
    "request": "Please include an example."
  }
}
```

Codex's explicit annotation maps to `response.kind: "comment"`. Claude text after
a quotation maps to `response.kind: "following_text"`; the final value includes
any additional request whose boundary cannot be established. A quote-only item
omits `response`. Codex `source` data is optional positioning evidence: display
the supplied quote text, not text reconstructed from offsets into another JSONL
message. Navigating to that source is outside this proposal.

| Consumer or boundary | Required behavior |
| --- | --- |
| Rust adapters | Preserve the full native input before existing text cleanup. Only add reply metadata after a complete exact match; otherwise keep the complete message in the ordinary path. |
| Shared model and HTML emitter | Retain the legacy body and source alongside the additive data. Do not introduce a body part such as `p: "annotation"` that existing renderers cannot display. |
| Adapted App shell | Prefer valid `reply_context` at a supported version. Display the quotes, associated text, explicit request, and remaining context exactly once. Missing, invalid, or unsupported metadata uses the ordinary body. |
| Unadapted Classic, export, and TUI | Continue reading the existing supported representation. They must not depend on the optional metadata to recover the user's words. |

Current Classic skips unknown body parts, while App shell exposes an unknown-part
fallback. Keeping supported body parts is therefore necessary for complete
ordinary rendering in consumers that have not adopted the optional metadata.

The current HTML `src` is derived from adapter output; it is not by itself proof
that the original native envelope survived. Implementation must retain that input
before Codex `desktop_prompt` or Claude `strip_caveat` cleanup, including any context
around `## My request:`. Existing body parts carrying files, images, or other
trailing context remain available. If the implementation cannot partition them
without omission or duplication, it must
omit `reply_context` for the whole message and use the ordinary presentation.

When implemented, changes to cached canonical data require `FOLD_VERSION` to
advance; changes to HTML records require the corresponding `RECORD_SCHEMA`
invalidation. The live `RecordStore` persists rendered records, so changing the
parser without invalidating incompatible records can still serve old output.
This documentation change does not change either version or claim that the Rust
parser or App shell already supports this field.

---

## First delivery and follow-ups

1. Add adapter recognition and the compatible shared metadata, then use it in
   App shell's Turns list and opened user messages. Validate the one-turn model,
   quote/comment association, quote-only input, both themes, and complete fallback.
2. Verify that existing Classic, export, and TUI consumers still show complete
   ordinary user messages with the additive records. This compatibility check is
   part of the first delivery even though new styling on those surfaces is not.
3. Once App shell's reading experience and boundary cases are acceptable, adapt
   standalone export using the same data. Classic's retirement supersedes its new
   quotation presentation. The existing phone layouts are already covered by this delivery.

The validation guide owns versioned source examples and the adapter test method;
this RFC owns recognition rules, the shared-data contract, the reading model, and
delivery scope. Before implementation is accepted, exercise the guide's positive
and negative cases through Rust normalization, emitted records, and the actual
App shell renderer. Synthetic examples define controlled cases; a private
native-session smoke test provides
separate evidence that the current clients still generate the accepted formats.

---

## What this costs the reader

**The list shows one preview.** A compact row cannot show every passage and every
comment. The first preview identifies the message; the count makes the remaining
annotations discoverable; opening the turn supplies their context.

**The first item's text takes precedence over the additional request.** This makes the
annotation visible even when a broad request accompanies it. The broad request
is still part of the opened message. Codex can display it after all pairs; Claude
retains it within the final item's following text unless a future evidenced format
provides an explicit boundary.

**A small bar carries a specific meaning.** It must be distinguishable from the
current-turn highlight and from any task or activity status. Its meaning should
also be available to a reader who cannot see the bar.

---

## Boundaries

This proposal concerns how existing annotation replies are read in Agent Monitor.
The first UI adaptation covers App shell's Turns preview and the quotation/text
relationship in opened messages, with compatible data for later consumers.

- The quote marker identifies a reply to selected text. It does not report that
  a comment is unresolved, addressed, unread, or waiting for action.
- Clicking the row opens this reply. Jumping back to the earlier answer and
  highlighting the original selection are separate proposals.
- Several annotations stay together in the user's message; they do not become
  extra turns, tasks, or a new review queue.
- Ordinary messages and quotations remain readable as written. Quoted text in a
  normal message does not by itself mean that the message is an annotation reply.
- The reader can still inspect the full original message. When a reply cannot
  be confidently presented as quote/comment pairs, its original content remains
  readable and complete.

The outcome to judge is whether a reader can find their comment in the Turns
list, recognize the passage it refers to, and read every comment in context.
