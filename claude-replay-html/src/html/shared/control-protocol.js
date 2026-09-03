// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…), the classic
// rail and the v2 splice (inlined at serve time through {{SHARED}}) and the html crate's pages
// (inlined by html_export/shared.rs). Conventions the inliner relies on: no imports, exactly
// one trailing `export { … };` line.
//
// The /api/send + /api/consent PROTOCOL (#133), once (#48, design/monitor-shell-duplication.md
// §1(c)). Two transports; the backend picks by liveness and the UI sets expectations:
//  • resume — a FINISHED claude/codex session; /api/send resumes it (permissions skipped).
//  • tmux   — a LIVE, PROVEN pane; /api/send injects, but ONLY with standing CONSENT. An
//    unconsented pane makes the button read "Grant & send": one click grants (/api/consent,
//    the passcode in the BODY — never the query string, which can be logged) then sends. The
//    grant step is deliberate — it is where the RISK is stated, because injected text is
//    input to a live agent running with its permissions.
// The shells keep their markup and their intermediate paint; this module holds the rule for
// WHO may compose, the words, the queries, and the meaning of every server answer.

/** Whether — and how — a row can be composed to, given the monitor's pairing. Constraint 2
 *  (#133): a project with another live session offers neither transport — with two live
 *  sessions in one cwd we cannot tell which drives it, so we refuse rather than pick. */
function composeCapability(row, paired) {
  if (!paired || !row) return { inject: false, resume: false, mode: null };
  const inject = !!row.injectable;
  const resume = row.state === "finished" && !row.projActive && ["claude", "codex"].includes(row.agent);
  return { inject, resume, mode: inject ? "tmux" : resume ? "resume" : null };
}

/** The words a compose surface shows for a mode and consent state — the classic rail's. */
function composeCopy(mode, consented, name) {
  const tmux = mode === "tmux";
  return {
    target: (tmux ? "Inject into: " : "Send to: ") + (name || ""),
    placeholder: tmux
      ? "Type a prompt — it is pasted into the live tmux pane and submitted"
      : "Send a prompt — the session resumes and runs it (permissions skipped)",
    // The grant step exists to make the RISK explicit.
    notice: tmux && !consented
      ? "Runs in the LIVE agent with its permissions. “Grant & send” authorises this pane until it restarts."
      : "",
    button: tmux ? (consented ? "Send to pane" : "Grant & send") : "Send prompt",
    revoke: tmux && consented
  };
}

const sendQuery = target => `/api/send?target=${encodeURIComponent(target || "")}`;
const consentQuery = (target, op) => `/api/consent?${op === "revoke" ? "op=revoke&" : ""}target=${encodeURIComponent(target || "")}`;

/** What a /api/consent grant answer means. */
function grantOutcome(g) {
  if (g && g.code === "passcode-required") return { kind: "passcode-required", tone: "", message: "Enter your passcode to authorise this pane." };
  if (g && g.code === "bad-passcode") return { kind: "bad-passcode", tone: "err", message: "Incorrect passcode." };
  if (g && g.code === "locked") return { kind: "locked", tone: "err", message: g.error || "too many attempts — wait a moment." };
  if (!g || !g.ok) return { kind: "error", tone: "err", message: (g && g.error) || "could not grant consent" };
  return { kind: "granted", tone: "", message: "" };
}

/** What a /api/send answer means, for a mode. */
function sendOutcome(d, mode) {
  if (d && d.ok) return { kind: "sent", tone: "ok", message: mode === "tmux" ? "sent into the pane" : "sent — the session is resuming" };
  // Consent lapsed (the pid changed) between opening and sending — re-offer the grant.
  if (d && d.code === "no-consent") return { kind: "no-consent", tone: "err", message: "this pane needs consent — press “Grant & send”" };
  return { kind: "error", tone: "err", message: (d && d.error) || "could not send" };
}

/** What a revoke answer means. */
function revokeOutcome(d) {
  return d && d.ok ? { kind: "revoked", tone: "", message: "consent revoked" } : { kind: "error", tone: "err", message: "could not revoke" };
}

const UNREACHABLE = { kind: "unreachable", tone: "err", message: "could not reach the monitor" };

/**
 * The send flow, transport-agnostic: `post(url, body)` resolves to the parsed answer (a
 * shell's fetch + json). Resume, or an already-consented pane → send. An unconsented pane →
 * grant first (with `passcode`, possibly empty), then send; every grant outcome other than
 * "granted" returns as `{ step: "grant", outcome }` so the shell can reveal the passcode
 * field, show the lockout, or report. `status(text)` paints the intermediate state.
 */
async function runSend({ target, mode, consented, prompt, passcode = "", post, status = () => {} }) {
  let granted = consented;
  if (mode === "tmux" && !consented) {
    status(passcode ? "authorising…" : "granting…");
    let g;
    try { g = await post(consentQuery(target), passcode || ""); } catch (_) { return { step: "grant", outcome: UNREACHABLE, consented: false }; }
    const outcome = grantOutcome(g);
    if (outcome.kind !== "granted") return { step: "grant", outcome, consented: false };
    granted = true;
  }
  status("sending…");
  let d;
  try { d = await post(sendQuery(target), prompt); } catch (_) { return { step: "send", outcome: UNREACHABLE, consented: granted }; }
  const outcome = sendOutcome(d, mode);
  return { step: "send", outcome, consented: outcome.kind === "no-consent" ? false : granted };
}

/** The revoke flow. */
async function runRevoke({ target, post }) {
  let d;
  try { d = await post(consentQuery(target, "revoke"), ""); } catch (_) { return UNREACHABLE; }
  return revokeOutcome(d);
}

export { composeCapability, composeCopy, sendQuery, consentQuery, grantOutcome, sendOutcome, revokeOutcome, runSend, runRevoke };
