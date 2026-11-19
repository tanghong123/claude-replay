// The page `/detached` serves (#s433; `/markdown` and `/image` are the same page): a tab of its own,
// drawn by the kit's `mountDetached` from the page's own address (a session's transcript, a Markdown
// document or an image). The page is static, so nothing in its address is ever written into its
// HTML. Its theme is the app's, read from the setting the app keeps, and follows a toggle made in any
// tab.
import { mountDetached } from "./detached.js";

const THEME_KEY = "am-demo-theme";
const stored = () => { try { return localStorage.getItem(THEME_KEY) === "dark" ? "dark" : "light"; } catch (_) { return "light"; } };
const detached = mountDetached(document.getElementById("detachedView"), { theme: stored() });
document.documentElement.dataset.theme = stored();
addEventListener("storage", event => {
  if (event.key !== THEME_KEY) return;
  document.documentElement.dataset.theme = stored();
  detached.setTheme(stored());
});
