// agent-monitor's kit (#s432, design/embedding.md, "Delivery"): the session view and the session list
// as a host mounts them, and a tab of its own for a transcript, a document or an image (#s433), from
// one release it pins. Every module beside this file is the one that
// release's agent-monitor serves at /monitor-ui/, byte for byte; the guide (docs/embedding-guide.md)
// says how to mount them, and index.d.ts declares what they take and answer. The doc comments over
// `mountSessionView` (session-view.js), `mountSessionList` (session-list-view.js) and `mountDetached`
// (detached.js) are the reference.
export { mountSessionView } from "./session-view.js";
export { mountSessionList } from "./session-list-view.js";
export { mountDetached } from "./detached.js";
export { backendHandle, useBackend } from "./backend.js";
export { persist } from "./state.js";
