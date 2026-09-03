import { indexState } from "./state.js";
import { groupSessions } from "./session-visibility.js";

export class SessionIndexStore {
  constructor(handlers) { this.handlers = handlers; this.timer = 0; this.loading = false; }
  start() { this.refresh(); }
  stop() { clearTimeout(this.timer); }
  async refresh() {
    if (this.loading) return;
    clearTimeout(this.timer); this.loading = true;
    try {
      const response = await fetch("/api/sessions", { cache: "no-store" });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = await response.json();
      const previous = indexState.rows;
      indexState.groups = (data.groups || []).map(group => ({
        ...group,
        rows: (group.rows || []).map(incoming => {
          const stable = previous.get(incoming.id);
          if (!stable) return incoming;
          for (const key of Object.keys(stable)) if (!(key in incoming) && key !== "_group") delete stable[key];
          return Object.assign(stable, incoming);
        })
      }));
      indexState.ignoredCount = data.ignoredCount || 0;
      indexState.rows = new Map();
      for (const group of indexState.groups) for (const row of group.rows || []) { row._group = group; indexState.rows.set(row.id, row); }
      this.handlers.update?.();
    } catch (error) { this.handlers.error?.(error); }
    finally { this.loading = false; this.timer = setTimeout(() => this.refresh(), 5000); }
  }
  grouped() { return groupSessions(indexState.groups); }
}
