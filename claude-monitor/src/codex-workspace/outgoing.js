/**
 * Local delivery previews, separate from authoritative history and delivery receipts.
 * Reconciliation only retires a preview: it never proves delivery or replays an action.
 */
export class OutgoingMessages {
  constructor() { this.entries = new Map(); this.observed = new Map(); this.consumed = new Map(); }
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
    else { entry.status = status; this.reconcile(entry.session); }
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
      if (entry.session !== session || entry.status !== "accepted" || !entry.canReconcile || !entry.text) continue;
      const match = this.observed.get(session)?.find(record => record.text === entry.text && !entry.baseline.has(record.key) && !used.has(record.key));
      if (match) { used.add(match.key); this.entries.delete(entry.id); }
    }
    this.consumed.set(session, used); this.prune();
  }
  prune() {
    const sessions = new Set([...this.entries.values()].map(entry => entry.session));
    for (const key of this.observed.keys()) if (!sessions.has(key)) this.observed.delete(key);
    for (const key of this.consumed.keys()) if (!sessions.has(key)) this.consumed.delete(key);
  }
  list(threadId, queue = []) {
    const queued = new Set(queue.map(entry => entry.clientUserMessageId));
    for (const entry of this.entries.values()) if (entry.threadId === threadId && entry.status === "accepted" && queued.has(entry.id)) this.entries.delete(entry.id);
    this.prune();
    return [...this.entries.values()].filter(entry => entry.threadId === threadId && !queued.has(entry.id));
  }
}
const recordKey = record => JSON.stringify([record.id, record.src, record.ts]);
