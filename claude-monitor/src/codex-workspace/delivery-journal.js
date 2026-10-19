import { nativePageStore, nativeDraftTab } from "./native-page-store.js";
import { ACCEPTED_PREVIEW_LIMIT } from "./outgoing.js";

const DRAFT_RETENTION_MS = 7 * 24 * 60 * 60 * 1000;

/** Browser-owned recovery data; server receipts remain the authority for delivery. */
export class DeliveryJournal {
  static tab(createId) { return nativeDraftTab(createId); }
  constructor(scope, tab, storage = nativePageStore(), now = Date.now) { Object.assign(this, { scope, tab, storage, now }); }
  change(update) {
    return this.storage.change(this.scope, value => {
      const now = this.now();
      for (const [key, draft] of Object.entries(value.drafts)) {
        // Session storage cannot tell another tab whether its owner was killed.
        // Give legacy drafts a full grace period, then expire other inactive tabs.
        if (!Number.isFinite(draft.updatedAt)) draft.updatedAt = now;
        if (JSON.parse(key)[0] !== this.tab && now - draft.updatedAt > DRAFT_RETENTION_MS) delete value.drafts[key];
      }
      trimAccepted(value);
      const result = update(value);
      trimAccepted(value);
      return result;
    });
  }
  load() { return this.change(value => structuredClone(value)); }
  draft(thread, draft) {
    return this.change(value => {
      const key = JSON.stringify([this.tab, thread]);
      if (!draft.text && !draft.images.length) delete value.drafts[key];
      else value.drafts[key] = { ...draft, updatedAt: this.now() };
    });
  }
  /** Commit the exact send before HTTP; an identical unresolved send shares its identity. */
  prepare(id, payload, preview) {
    return this.change(value => {
      const previous = value.attempts.find(entry => entry.status !== "accepted" && JSON.stringify(entry.payload) === JSON.stringify(payload));
      if (previous) return { ...structuredClone(previous), recovered: true };
      if (value.attempts.filter(entry => entry.status !== "accepted").length >= 64) throw new Error("Review outstanding message previews before sending more messages.");
      const entry = { id, payload, preview, status: "sending" };
      value.attempts.push(entry);
      return structuredClone(entry);
    });
  }
  settle(id, status) {
    return this.change(value => {
      if (status === "rejected") value.attempts = value.attempts.filter(entry => entry.id !== id);
      else {
        const entry = value.attempts.find(entry => entry.id === id);
        if (entry && entry.status !== "accepted") entry.status = status;
      }
    });
  }
  retire(id) { return this.settle(id, "rejected"); }
}

function trimAccepted(value) {
  const accepted = value.attempts.filter(entry => entry.status === "accepted");
  const expired = new Set(accepted.slice(0, -ACCEPTED_PREVIEW_LIMIT).map(entry => entry.id));
  value.attempts = value.attempts.filter(entry => !expired.has(entry.id));
}
