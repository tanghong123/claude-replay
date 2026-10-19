export const ACCEPTED_PREVIEW_LIMIT = 32;

/**
 * Local delivery previews, separate from authoritative history and delivery receipts.
 * Reconciliation only retires a preview: it never proves delivery or replays an action.
 */
export class OutgoingMessages {
  constructor() { this.entries = new Map(); this.observed = new Map(); this.consumed = new Map(); this.retired = []; }
  takeRetired() { return this.retired.splice(0); }
  restore(entry) {
    this.entries.set(entry.id, { id: entry.id, threadId: entry.payload.threadId, text: entry.payload.text,
      images: entry.payload.images || [], ...entry.preview, baseline: new Set(entry.preview?.baseline || []),
      status: entry.status === "accepted" ? "accepted" : "uncertain" });
  }
  begin(id, payload, snapshot) {
    const existing = this.entries.get(id);
    if (existing) { existing.status = "sending"; return; }
    const records = snapshot?.records || [];
    this.entries.set(id, { id, threadId: payload.threadId, text: payload.text,
      images: payload.images || [], status: "sending", session: snapshot?.session,
      baseline: new Set(records.filter(record => record.kind === "user").map(recordKey)),
      canReconcile: !!snapshot?.ready });
  }
  settle(id, status) {
    const entry = this.entries.get(id);
    if (!entry) return;
    if (status === "rejected") this.entries.delete(id);
    else { if (entry.status !== "accepted") entry.status = status; this.reconcile(entry.session); }
  }
  dismiss(id) { this.entries.delete(id); this.prune(); }
  observe(session, records) {
    if (![...this.entries.values()].some(entry => entry.session === session)) return;
    this.observed.set(session, records.filter(record => record.kind === "user").map(record => ({ key: recordKey(record), text: record.src?.trim() })));
    this.reconcile(session);
  }
  reconcile(session) {
    const used = this.consumed.get(session) || new Set();
    for (const entry of this.entries.values()) {
      if (entry.session !== session || !["accepted", "uncertain"].includes(entry.status) || !entry.canReconcile || !entry.text) continue;
      const match = this.observed.get(session)?.find(record => record.text === entry.text && !entry.baseline.has(record.key) && !used.has(record.key));
      if (match) { if (entry.status === "accepted") { used.add(match.key); this.entries.delete(entry.id); this.retired.push(entry.id); } else entry.historyMatch = true; }
    }
    this.consumed.set(session, used); this.prune();
  }
  prune() {
    const accepted = [...this.entries.values()].filter(entry => entry.status === "accepted");
    for (const entry of accepted.slice(0, -ACCEPTED_PREVIEW_LIMIT)) {
      this.entries.delete(entry.id); this.retired.push(entry.id);
    }
    const sessions = new Set([...this.entries.values()].map(entry => entry.session));
    for (const key of this.observed.keys()) if (!sessions.has(key)) this.observed.delete(key);
    for (const key of this.consumed.keys()) if (!sessions.has(key)) this.consumed.delete(key);
  }
  list(threadId, queue = []) {
    const queued = new Set(queue.map(entry => entry.clientUserMessageId));
    for (const entry of this.entries.values()) if (entry.threadId === threadId && entry.status === "accepted" && queued.has(entry.id)) { this.entries.delete(entry.id); this.retired.push(entry.id); }
    this.prune();
    return [...this.entries.values()].filter(entry => entry.threadId === threadId);
  }
}
const recordKey = record => JSON.stringify([record.id, record.src, record.ts]);
