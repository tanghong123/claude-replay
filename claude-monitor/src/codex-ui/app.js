// The monitor's page (#s551, design/app-mount.md, "The monitor's page is its first host"): the page
// carries an empty wrapper, and this draws the web app into it with the page's own server, as it
// loads, so native mode's entry (native-entry.js), which imports this and wraps the write switch as it
// is evaluated, finds the switch bound.
import { mountApp } from "./app-mount.js";
import { backend } from "./backend.js";
import { page } from "./view-root.js";

mountApp(page.byId("appMount"), { backend: backend(), version: page.body().dataset.version || undefined });

// #s577: the server says what this page's writes need by the request that loaded it. The pairing
// cookie is SameSite=Strict, so a page opened from a link in another app arrives without it; on
// Linux a paired monitor lets that same-user load read, and the page is told this browser is not
// paired though every request it makes after carries the cookie. As the gate's refusal page does
// (#341), the page then reloads once from itself, a same-site navigation that brings the cookie; a
// timestamp bounds it to once in ten seconds, so a browser that really holds no token is told why.
if (page.body().dataset.write === "browser") {
  try {
    const key = "am-write-retry", at = +sessionStorage.getItem(key) || 0;
    if (Date.now() - at > 10000) { sessionStorage.setItem(key, String(Date.now())); location.reload(); }
  } catch (_) { /* no storage: no retry, and the reason stands */ }
}

export { installNativeController } from "./app-mount.js";
