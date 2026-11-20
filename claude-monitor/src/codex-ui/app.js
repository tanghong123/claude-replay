// The monitor's page (#s551, design/app-mount.md, "The monitor's page is its first host"): the page
// carries an empty wrapper, and this draws the web app into it with the page's own server, as it
// loads, so native mode's entry (native-entry.js), which imports this and wraps the write switch as it
// is evaluated, finds the switch bound.
import { mountApp } from "./app-mount.js";
import { backend } from "./backend.js";
import { page } from "./view-root.js";

// #s577: the server says what this page's writes need by the request that loaded it. The pairing
// cookie is SameSite=Strict, so a page opened from a link in another app arrives without it; on
// Linux a paired monitor lets that same-user load read, and the page is told this browser is not
// paired though every request it makes after carries the cookie. As the gate's refusal page does
// (#341), the page then reloads once from itself, a same-site navigation that brings the cookie,
// hidden meanwhile so the reason does not flash. A browser that really holds no token is told why
// after that one reload.
const retry = writeRetry();
if (retry) page.body().hidden = true;
mountApp(page.byId("appMount"), { backend: backend(), version: page.body().dataset.version || undefined });
if (retry) location.reload();

/** Whether this load is the one to reload: the page was drawn `data-write="browser"`, and no reload
 *  was made within ten seconds before this navigation BEGAN. Judged by the navigation's start
 *  (`performance.timeOrigin`), not by when this module runs: over a slow link (an `ssh -L` tunnel)
 *  the module graph can take longer than that to load, and the reload's own stamp would read as old
 *  and reload it again, for ever. No storage, no reload. */
function writeRetry() {
  if (page.body().dataset.write !== "browser") return false;
  try {
    const key = "am-write-retry", at = +sessionStorage.getItem(key) || 0;
    if (at && performance.timeOrigin - at < 10000) return false;
    sessionStorage.setItem(key, String(Date.now()));
    return true;
  } catch (_) { return false; }
}

export { installNativeController } from "./app-mount.js";
