/** Paired same-origin HTTP client. Native writes have stable IDs; uncertain writes are never replayed automatically. */
export class GatewayError extends Error {
  constructor(message, code, receipt = null, cause = undefined, readOnly = false) { super(message, { cause }); this.name = "GatewayError"; this.code = code; this.receipt = receipt; this.readOnly = readOnly; }
}

/** A failed background read must not hide an action whose acceptance is still unknown. */
export function preserveUncertainActionError(current, incoming) {
  return current?.code === "uncertain" && !current.readOnly && incoming?.readOnly && incoming.code === "unavailable" ? current : incoming;
}

/** Report accepted runtime errors and recovery effects once, including buffered replay. */
export function applyWorkspaceEvent(state, event, reduce, onRecoveryError, onRuntimeError) {
  const next = reduce(state, event);
  const threadId = event.params?.threadId;
  if (event.method === "gateway/recoveryError" && threadId === state.thread?.id && state.thread && next.thread === null && next.lastError) onRecoveryError(threadId, next.lastError);
  const accepted = next !== state && (state.epoch == null || event.epoch === state.epoch && event.sequence === state.sequence + 1);
  if (accepted && event.method === "error" && (!threadId || threadId === state.thread?.id) && next.lastError) onRuntimeError?.(next.lastError);
  return next;
}

/**
 * Generate a version-4 action UUID on HTTPS, loopback or private HTTP.
 * Action IDs identify native writes and must retain cryptographic randomness.
 * @param {Pick<Crypto, "getRandomValues"> & Partial<Pick<Crypto, "randomUUID">>} [source]
 * @returns {string}
 */
export function createActionId(source = globalThis.crypto) {
  if (typeof source?.randomUUID === "function") return source.randomUUID();
  if (typeof source?.getRandomValues !== "function") throw new GatewayError("This browser cannot generate secure action IDs.", "unavailable");
  const bytes = source.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40; bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export class GatewayApi {
  /** Deadlines include response decoding. A timed-out write remains uncertain, never replayed. */
  constructor(fetcher = globalThis.fetch.bind(globalThis), { readTimeoutMs = 30000, actionTimeoutMs = 60000, schedule = globalThis.setTimeout.bind(globalThis), cancel = globalThis.clearTimeout.bind(globalThis) } = {}) {
    for (const value of [readTimeoutMs, actionTimeoutMs]) if (!Number.isFinite(value) || value <= 0) throw new TypeError("Gateway request deadlines must be positive finite milliseconds");
    Object.assign(this, { fetcher, readTimeoutMs, actionTimeoutMs, schedule, cancel });
  }
  request(route, payload = null, options = {}) { return this.requestJson(`/api/codex/${route}`, payload, options); }
  /** Read the original Monitor index under the same pairing, deadline and read-failure policy. */
  readMonitorIndex() { return this.requestJson("/api/sessions"); }
  async requestJson(path, payload = null, { readOnly = false } = {}) {
    const readsState = payload == null || readOnly;
    const controller = new AbortController();
    let timer;
    const deadline = new Promise((_, reject) => {
      timer = this.schedule(() => {
        const cause = new Error(`Gateway request ${path} exceeded its deadline`);
        reject(new GatewayError(readsState ? "Loading the workspace timed out. Reconnect to load its state." : "The action response timed out. It may have been accepted; retry only with its saved action ID.", readsState ? "unavailable" : "uncertain", null, cause, readsState));
        controller.abort(cause);
      }, readsState ? this.readTimeoutMs : this.actionTimeoutMs);
    });
    const request = (async () => {
      let response;
      try {
        response = await this.fetcher(path, {
          signal: controller.signal, method: payload == null ? "GET" : "POST", credentials: "same-origin", cache: "no-store",
          headers: payload == null ? {} : { "Content-Type": "application/json" },
          ...(payload == null ? {} : { body: JSON.stringify(payload) }),
        });
      } catch (error) {
        throw new GatewayError(`The execution computer could not be reached. ${readsState ? "Reconnect to load its state." : "This action may already have been accepted; its saved action ID can be checked safely."}`, readsState ? "unavailable" : "uncertain", null, error, readsState);
      }
      // The HTTP pairing/origin gate returns plain text and has not reached native receipt lookup.
      if (response.status === 401 || response.status === 403) throw new GatewayError(response.status === 401 ? "Pair this browser with Agent Monitor before opening the workspace." : "The Gateway refused access to this workspace.", response.status === 401 ? "pairing_required" : "forbidden", null, new Error(`Gateway request ${path} refused with HTTP ${response.status}`), readsState);
      let value;
      try { value = await response.json(); }
      catch (error) { throw new GatewayError("The Gateway returned an unreadable response", readsState ? "unavailable" : "uncertain", null, error, readsState); }
      const object = value && typeof value === "object" && !Array.isArray(value);
      const errorEnvelope = value?.error && typeof value.error === "object" && !Array.isArray(value.error)
        && typeof value.error.code === "string" && value.error.code.length > 0 && typeof value.error.message === "string";
      const validEnvelope = object && (path === "/api/sessions" && readsState ? Array.isArray(value.groups)
        : typeof value.ok === "boolean" && (value.ok ? response.ok : errorEnvelope));
      if (!validEnvelope) throw new GatewayError("The Gateway returned an invalid response envelope", readsState ? "unavailable" : "uncertain", null, new Error(`Gateway request ${path} returned a non-object or missing result flag`), readsState);
      if (!response.ok || value.ok === false) {
        const code = value.error?.code || (readsState && response.status >= 500 ? "unavailable" : "gateway_error");
        const message = value.error?.message || `Gateway request failed (${response.status})`;
        // An interrupted read has no execution effect to reconcile and can be retried safely.
        throw new GatewayError(message, readsState && code === "uncertain" ? "unavailable" : code, value.receipt,
          new Error(`Gateway request ${path} failed (${code}): ${message}`), readsState);
      }
      return value;
    })();
    try { return await Promise.race([request, deadline]); }
    finally { this.cancel(timer); }
  }

  read(route, query = {}) {
    const values = Object.entries(query).filter(([, value]) => value != null && value !== "");
    return this.request(`${route}${values.length ? `?${new URLSearchParams(values)}` : ""}`);
  }
  action(route, payload, actionId = createActionId()) { return this.request(route, { ...payload, actionId }); }
  open(threadId) { return this.request("open", { threadId }, { readOnly: true }); }
}

/** Recover catalog/history reads and the latest requested task; never replay native actions. */
export class WorkspaceRecovery {
  constructor({ loadStatus, loadCatalog, openThread, onError, onRecovered, schedule = globalThis.setTimeout.bind(globalThis), cancel = globalThis.clearTimeout.bind(globalThis) }, threadId = null) {
    Object.assign(this, { loadStatus, loadCatalog, openThread, onError, onRecovered, schedule, cancel });
    this.threadId = threadId; this.threadPending = !!threadId; this.catalogReady = false;
    this.epoch = null; this.revision = 0; this.selectionRevision = 0; this.catalogRevision = 0; this.threadRevision = 0; this.pending = null; this.retry = null; this.retryDelay = 500; this.error = null;
  }
  remember(threadId) { this.threadId = threadId; this.threadPending = !!threadId; this.revision++; this.selectionRevision++; this.threadRevision++; }
  opened(threadId) { if (threadId === this.threadId) this.threadPending = false; }
  forget(threadId) { if (threadId === this.threadId) this.remember(null); }
  select(threadId) { this.remember(threadId); return this.recover(); }
  recover({ catalog = false, thread = false } = {}) {
    if (catalog) { this.catalogReady = false; this.catalogRevision++; }
    if (thread) { this.threadPending = !!this.threadId; this.threadRevision++; }
    this.revision++;
    if (this.retry != null) { this.cancel(this.retry); this.retry = null; }
    if (this.pending) return this.pending;
    this.pending = this.readState().finally(() => { this.pending = null; });
    return this.pending;
  }
  defer() {
    if (this.retry != null) this.cancel(this.retry);
    this.retry = this.schedule(() => { this.retry = null; this.recover(); }, this.retryDelay);
    this.retryDelay = Math.min(5000, this.retryDelay * 2);
  }
  async readState() {
    for (;;) {
      const revision = this.revision;
      const requestedThread = this.threadId;
      try {
        const status = await this.loadStatus();
        if (this.epoch != null && status.epoch !== this.epoch) { this.catalogReady = false; this.threadPending = !!this.threadId; }
        this.epoch = status.epoch;
        if (status.connection !== "connected") {
          if (typeof status.connectionError === "string" && status.connectionError) {
            this.error = new GatewayError(status.connectionError, "unavailable", null, new Error(`Native status: ${status.connectionError}`), true);
            this.onError(this.error);
          }
          this.defer(); return;
        }
        if (!this.catalogReady) {
          const catalogRevision = this.catalogRevision;
          await this.loadCatalog();
          if (catalogRevision !== this.catalogRevision) continue;
          this.catalogReady = true;
        }
        if (this.threadPending && this.threadId) {
          const threadId = this.threadId, selectionRevision = this.selectionRevision, threadRevision = this.threadRevision;
          const confirmed = await this.openThread(threadId);
          // A-to-B-to-A is a new selection even though its thread ID matches the old read.
          if (selectionRevision !== this.selectionRevision || threadRevision !== this.threadRevision) continue;
          if (confirmed === false) { this.defer(); return; }
          this.opened(threadId);
        }
        if (revision !== this.revision) continue;
        this.onRecovered(this.error); this.error = null; this.retryDelay = 500;
        return;
      } catch (error) {
        // Do not surface an older task's error after the user selects another task.
        if (requestedThread !== this.threadId || revision !== this.revision) continue;
        // A superseded refusal is not evidence about the current browser/session access.
        this.error = error; this.onError(error);
        if (error.code === "unavailable") this.defer();
        return;
      }
    }
  }
}

/** Consume opaque native cursors and stop on a repeated cursor rather than looping forever. */
export async function readAllPages(api, route, query = {}) {
  const rows = [];
  const seen = new Set();
  let cursor = null;
  do {
    const page = await api.read(route, { ...query, cursor });
    if (!Array.isArray(page.data)) throw new GatewayError(`${route} returned an invalid page`, "gateway_error");
    rows.push(...page.data);
    cursor = page.nextCursor || null;
    if (cursor && seen.has(cursor)) throw new GatewayError(`${route} returned a repeated history cursor`, "gateway_error");
    if (cursor) seen.add(cursor);
  } while (cursor);
  return rows;
}
