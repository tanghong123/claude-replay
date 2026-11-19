// The monitor's page (#s551, design/app-mount.md, "The monitor's page is its first host"): the page
// carries an empty wrapper, and this draws the web app into it with the page's own server, as it
// loads, so native mode's entry (native-entry.js), which imports this and wraps the write switch as it
// is evaluated, finds the switch bound.
import { mountApp } from "./app-mount.js";
import { backend } from "./backend.js";
import { page } from "./view-root.js";

mountApp(page.byId("appMount"), { backend: backend(), version: page.body().dataset.version || undefined });

export { installNativeController } from "./app-mount.js";
