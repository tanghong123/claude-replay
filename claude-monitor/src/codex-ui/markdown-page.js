// The page `/markdown` serves (#271): the Markdown document the preview pane shows, in a tab of its
// own — mdrev's viewer as the whole page. `mountStandalone` (mdrev-pane.js) does the work; the page
// is static, so nothing in its address is ever written into its HTML.
import { mountStandalone, Unavailable } from "./mdrev-pane.js";

const doc = document.getElementById("doc");
mountStandalone(doc).catch(error => {
  doc.classList.add("unavailable");
  doc.textContent = error instanceof Unavailable ? error.message : `This document cannot be shown: ${error.message}`;
});
