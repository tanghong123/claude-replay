// The optional enabling control is the only native UI loaded before activation.
import { mountNativeMenu, renderNativeMenu, renderNativeServerStatus, positionNativeMenu, readNativeStatus } from "./native-menu.js";
import { backend } from "./backend.js";
import { controlState } from "./state.js";
import { installNativeController } from "./app.js";
import { page, view } from "./view-root.js";

const byId = view.byId;
const { button, menu } = mountNativeMenu(() => openMenu());
const originalSwitch = page.byId("writeSwitch").onclick;
page.byId("writeSwitch").onclick = event => {
  originalSwitch(event); button.hidden = !controlState.write;
  if (!controlState.write && menu.matches(":popover-open")) menu.hidePopover();
};
button.hidden = !controlState.write;
let busy = false;
function toast(message) { const node = page.byId("toast"); node.textContent = message; node.classList.add("show"); setTimeout(() => node.classList.remove("show"), 3000); }
function nativeStyles() {
  if (byId("codexNativeStyles")) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const link = document.createElement("link"); link.id = "codexNativeStyles"; link.rel = "stylesheet"; link.href = backend().url("/codex-workspace/monitor.css");
    link.onload = resolve; link.onerror = () => { link.remove(); reject(new Error("Cannot load native controls. Retry activation.")); };
    view.sheets().append(link);
  });
}
const position = () => positionNativeMenu(menu, button);
new ResizeObserver(position).observe(menu);
visualViewport?.addEventListener("resize", position); visualViewport?.addEventListener("scroll", position);
async function openMenu() {
  if (menu.matches(":popover-open")) { menu.hidePopover(); return; }
  if (!controlState.write || busy) return;
  busy = true; button.disabled = true;
  try {
    const status = await readNativeStatus(backend().fetch);
    const enable = renderNativeMenu(menu, { enabled: false, status }, async () => {
      if (busy || !controlState.write) return;
      busy = true; enable.disabled = true; page.byId("writeSwitch").disabled = true;
      try {
        const { GatewayApi } = await import(backend().url("/codex-workspace/api.js"));
        const activated = await new GatewayApi(backend().fetch).request("start", { writeMode: true, startIfMissing: true });
        await nativeStyles();
        const { NativeModeControls } = await import("./native-mode.js");
        page.byId("writeSwitch").onclick = originalSwitch;
        // Keep the open surface through controller installation. A user who
        // dismissed it during startup must not have it reopened afterward.
        const controller = await installNativeController(NativeModeControls, activated, () => byId("codexNativeStyles")?.remove(), nativeStyles, { button, menu });
        if (menu.matches(":popover-open")) controller.showMenu();
      } catch (error) {
        renderNativeServerStatus(menu, { enabled: false, status, activationFailed: true });
        toast(error.message); enable.disabled = false;
      }
      finally { busy = false; page.byId("writeSwitch").disabled = false; }
    });
    menu.showPopover(); position(); enable.focus();
  } catch (error) { toast(`Cannot check Codex server status: ${error.message}`); }
  finally { busy = false; button.disabled = false; }
}
