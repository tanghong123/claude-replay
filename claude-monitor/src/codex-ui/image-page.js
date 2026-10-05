// The page `/image` serves (#s11): an image the preview pane shows, in a tab of its own — the
// whole window, fit, zoomable and pannable by the same shared engine the pane uses (#228). The page
// is static, so nothing in its address is ever written into its HTML. It reads what the pane was
// OFFERED and nothing more: a file through `/file` with the stamp the page minted for it, or an
// image the transcript embeds, handed over through sessionStorage (which `window.open` copies into
// the tab it makes — the opener must never use `noopener`, as for Markdown, #271).
import { createImageView } from "./shared/image-view.js";

const stage = document.getElementById("stage");
const img = stage.querySelector("img");
const params = new URLSearchParams(location.search);
const name = params.get("name") || "image";
document.title = name;
img.alt = name;

const follow = () => { document.documentElement.dataset.theme = localStorage.getItem("am-demo-theme") === "dark" ? "dark" : ""; };
follow();
addEventListener("storage", event => { if (event.key === "am-demo-theme") follow(); });

function unavailable(title, words) {
  stage.classList.add("unavailable");
  stage.innerHTML = "";
  const head = document.createElement("strong"); head.textContent = title;
  const body = document.createElement("span"); body.textContent = words;
  stage.append(head, body);
}

/** Where the bytes come from: the file the pane was offered, at full size; the copy the transcript
 *  embeds, handed over by the opener, where there is no file or the file has gone since. */
async function source() {
  const held = params.get("held") ? sessionStorage.getItem(params.get("held")) : null;
  if (!params.get("path")) {
    if (held) return held;
    throw Object.assign(new Error(params.get("held") ? "This image was handed to the tab that opened it, and that copy is gone. Open it again from its session." : "This address names no image."), { title: "This image is no longer here" });
  }
  try { return await fromFile(); } catch (error) { if (held) return held; throw error; }
}

async function fromFile() {
  const path = params.get("path"), sig = params.get("sig");
  const response = await fetch(`/file?path=${encodeURIComponent(path)}&sig=${encodeURIComponent(sig || "")}`, { cache: "no-store" });
  // #s7: the route says which — gone, refused, unpaired — and the words are the server's.
  if (response.status === 410) throw Object.assign(new Error(`Nothing is at ${path} any more: it was moved or deleted after the session named it.`), { title: "This image is gone" });
  if (response.status === 401) throw Object.assign(new Error("Reading local files requires pairing — run `agent-monitor --pair`."), { title: "Not paired" });
  if (!response.ok) throw Object.assign(new Error(await response.text() || `HTTP ${response.status}`), { title: "This monitor may not show this image" });
  if (!(response.headers.get("content-type") || "").startsWith("image/")) throw Object.assign(new Error(`${path} is not an image this page can show.`), { title: "Not an image" });
  return URL.createObjectURL(await response.blob());
}

source().then(src => {
  const view = createImageView(stage, img, {});
  if (typeof ResizeObserver !== "undefined") new ResizeObserver(() => view.sync()).observe(stage);
  img.src = src;
}, error => unavailable(error.title || "This image cannot be shown", error.message));
