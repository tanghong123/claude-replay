// The document handed to the HTML preview frame (preview.js). No imports on purpose: the
// contract test loads this under node, where there is no DOM.
//
// The policy is placed by RULE, never by searching the artifact for a place to put it. The
// previous code looked for `<head>` with a regex; a `<head>` inside a leading comment put the
// meta tag inside that comment, and the frame — which allows scripts — ran with no policy at
// all (found in review). The only token that may precede the policy is a DOCTYPE, because the
// tokenizer recognises it only at the very start (after whitespace and complete comments) and
// keeping it keeps the artifact in standards mode; anything else, including a comment that
// merely contains the word, gets the policy in front of it. The HTML parser hoists a leading
// `<meta>` into the implied `<head>` and ignores the artifact's own `<html>`/`<head>` start
// tags, so the policy is in force before a single artifact byte is parsed.
//
// What it buys: nothing on the monitor page is reachable (the frame has no `allow-same-origin`,
// so its origin is opaque) and nothing loads over the network. What it cannot buy: a sandboxed
// frame may still navigate ITSELF to a URL, which no CSP directive forbids — so an artifact can
// carry its own bytes off the machine, which its author already had.
export const PREVIEW_CSP = "default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; script-src 'unsafe-inline'; font-src data:; media-src data: blob:; connect-src 'none'; form-action 'none'; base-uri 'none'";

const DOCTYPE_FIRST = /^\uFEFF?(?:\s|<!--[\s\S]*?-->)*<!doctype[^>]*>/i;

export function sandboxDocument(html) {
  const source = String(html ?? "");
  const meta = `<meta http-equiv="Content-Security-Policy" content="${PREVIEW_CSP}">`;
  const doctype = DOCTYPE_FIRST.exec(source);
  const at = doctype ? doctype[0].length : 0;
  return source.slice(0, at) + meta + source.slice(at);
}
