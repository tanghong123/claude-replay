import { indexState } from "./state.js";
import { groupSessions } from "./shared/session-visibility.js";

export class SessionIndexStore {
  constructor(handlers, options = {}) { this.handlers = handlers; this.load = options.load; this.group = options.group; this.timer = 0; this.loading = null; this.revision = 0; this.stopped = false; this.generation = 0; }
  start() { this.stopped = false; return this.refresh(); }
  stop() { this.stopped = true; this.generation++; this.loading = null; clearTimeout(this.timer); this.timer = 0; }
  refresh() {
    if (this.stopped) return Promise.resolve();
    clearTimeout(this.timer); this.revision++;
    if (!this.loading) {
      const loading = this.refreshLatest(this.generation).finally(() => {
        if (this.loading !== loading) return;
        this.loading = null;
        if (!this.stopped) this.timer = setTimeout(() => this.refresh(), 5000);
      });
      this.loading = loading;
    }
    return this.loading;
  }
  async refreshLatest(generation) {
    // A filter change or newly created session must await fresh rows rather than
    // silently skipping a refresh while the previous scan is in flight.
    let revision;
    do {
      revision = this.revision;
      try {
        let data;
        if (this.load) data = await this.load();
        else {
          const response = await fetch("/api/sessions", { cache: "no-store" });
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          data = await response.json();
        }
        if (generation !== this.generation || this.stopped) return;
        if (revision !== this.revision) continue;
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
      } catch (error) { if (generation === this.generation && !this.stopped && revision === this.revision) this.handlers.error?.(error); }
    } while (generation === this.generation && !this.stopped && revision !== this.revision);
  }
  grouped() { return this.group ? this.group(indexState.groups, groupSessions) : groupSessions(indexState.groups); }
}
