import { nativePageStore, nativeDraftTab } from "./native-page-store.js";

/** Browser-owned recovery data; server receipts remain the authority for delivery. */
export class DeliveryJournal {
  static tab(createId) { return nativeDraftTab(createId); }
  constructor(scope, tab, storage = nativePageStore()) { Object.assign(this, { scope, tab, storage }); }
  load() { return this.storage.change(this.scope, value => structuredClone(value)); }
  draft(thread, draft) {
    return this.storage.change(this.scope, value => {
      const key = JSON.stringify([this.tab, thread]);
      if (!draft.text && !draft.images.length) delete value.drafts[key];
      else value.drafts[key] = draft;
    });
  }
  /** Commit the exact send before HTTP; an identical unresolved send shares its identity. */
  prepare(id, payload, preview) {
    return this.storage.change(this.scope, value => {
      const previous = value.attempts.find(entry => entry.status !== "accepted" && JSON.stringify(entry.payload) === JSON.stringify(payload));
      if (previous) return { ...structuredClone(previous), recovered: true };
      if (value.attempts.length >= 64) throw new Error("Review outstanding message previews before sending more messages.");
      const entry = { id, payload, preview, status: "sending" };
      value.attempts.push(entry);
      return structuredClone(entry);
    });
  }
  settle(id, status) {
    return this.storage.change(this.scope, value => {
      if (status === "rejected") value.attempts = value.attempts.filter(entry => entry.id !== id);
      else {
        const entry = value.attempts.find(entry => entry.id === id);
        if (entry) entry.status = status;
      }
    });
  }
  retire(id) { return this.settle(id, "rejected"); }
}
