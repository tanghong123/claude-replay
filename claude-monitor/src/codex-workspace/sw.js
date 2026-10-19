// Cache only static assets. API responses, conversations, pairing and draft storage never enter this cache.
const CACHE = "codex-workspace-shell-v5";
const SHELL = ["/codex-workspace", "/codex-workspace/monitor-controls.js", "/codex-workspace/session-conflict.js", "/codex-workspace/project-picker.js", "/codex-workspace/attachments.js", "/codex-workspace/monitor-model.js", "/codex-workspace/monitor.css", "/codex-workspace/api.js", "/codex-workspace/model.js", "/codex-workspace/requests.js", "/codex-workspace/manifest.webmanifest", "/codex-workspace/icon.svg", "/codex-workspace/apple-touch-icon.png", "/codex-workspace/icon-192.png", "/codex-workspace/icon-512.png"];
self.addEventListener("install", event => event.waitUntil(caches.open(CACHE).then(cache => cache.addAll(SHELL))));
self.addEventListener("activate", event => event.waitUntil(caches.keys().then(keys => Promise.all(keys.filter(key => key.startsWith("codex-workspace-shell-") && key !== CACHE).map(key => caches.delete(key)))).then(() => self.clients.claim())));
self.addEventListener("fetch", event => {
  const url = new URL(event.request.url);
  const staticAsset = SHELL.includes(url.pathname) || url.pathname.startsWith("/monitor-ui/") && /\.(js|css)$/.test(url.pathname);
  if (event.request.method !== "GET" || url.origin !== self.location.origin || !staticAsset || url.search) return;
  event.respondWith(fetch(event.request).then(response => { if (response.ok && response.type === "basic") { const copy = response.clone(); event.waitUntil(caches.open(CACHE).then(cache => cache.put(event.request, copy))); } return response; }).catch(async error => { const cached = await caches.match(event.request); if (cached) return cached; throw error; }));
});
