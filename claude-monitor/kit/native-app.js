// agent-monitor's kit, its second entry (#s551, design/app-mount.md, "Kept for the native app, as
// `frame` is"): the web app as one mount, for agent-monitor's native app, which is built with these
// components and pins the same release. It is not offered to hosts: `package.json`'s `exports` names
// `index.js` alone, and nothing here is declared in `index.d.ts` or described in the guide. The doc
// comment over `mountApp` (app-mount.js) is the reference.
export { mountApp } from "./app-mount.js";
