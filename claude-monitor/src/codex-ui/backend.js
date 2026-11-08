// The one request layer (#s416; design/embedding.md, "The order of the work", step 1, which is
// design/monitor-hub.md's stage 0). Every request the page makes to the server that feeds it, and
// every URL to that server the page builds (a file, a reveal, a detached tab, mdrev's bundle and
// routes), goes through a BACKEND HANDLE. So the same components reach their server however the
// page was given them:
// - the monitor's own page: the page's origin, as it always was (the default handle);
// - another web app: under a prefix of its own (`/agents/monitor`), or at another origin;
// - the native app: each paired monitor through its own handle, whose base need not be HTTP (its
//   relay answers `app://d/<device>/…`).
// A handle also says whether the reader is LOCAL to that server (a reveal opens a Finder window on
// the server's machine, so only a reader there is offered one; elsewhere a file is a download, and
// a long session is read by turn, #s277) and where that server's pinned mdrev bundle loads from —
// both per handle, since one page may hold sessions of several monitors. The server paths
// themselves (`/pull`, `/records`, `/file`…) stay the contract's names, built by the shared modules
// and the callers as before; a test refuses a request or URL built from one anywhere but here.
import { revealHere } from "./shared/capabilities.js";

/** Whether a base names this machine: a loopback host, the test the server applies too (#313). */
function loopbackBase(base) {
  let host;
  try { host = new URL(base).hostname; } catch (_) { return false; }
  return host === "localhost" || /^127(\.\d{1,3}){3}$/.test(host) || host === "[::1]" || host === "::1";
}

/** A backend handle.
 *  - `base`: "" for the page's own origin (the default); a same-origin prefix ("/agents/monitor");
 *    an absolute origin, with or without a path ("https://mac.example.ts.net"); or a base in a
 *    scheme of a native host's own ("app://d/mac-mini"). A trailing slash is dropped.
 *  - `credentials`: how a fetch carries credentials ("same-origin", the default, sends the ambient
 *    cookie to the page's own origin only; a host that hands a token over another way says so).
 *  - `local`: whether the reader is at the server's machine. By default: for the page's own origin,
 *    `revealHere()`; for an absolute base, whether it is a loopback host; otherwise not.
 *  - `mdrev`: the base that serves this server's pinned mdrev (`/mdrev/<version>/…`, `/markdown`,
 *    `/api/mdrev…`); the handle's own base unless a host serves mdrev elsewhere. */
export function backendHandle({ base = "", credentials = "same-origin", local, mdrev } = {}) {
  const root = String(base || "").replace(/\/+$/, "");
  const mdrevRoot = mdrev == null ? root : String(mdrev).replace(/\/+$/, "");
  const join = (prefix, path) => {
    if (typeof path !== "string" || !path.startsWith("/")) throw new TypeError(`not a server path: ${path}`);
    return prefix + path;
  };
  const near = local != null ? !!local : root === "" || root.startsWith("/") ? revealHere() : loopbackBase(root);
  return Object.freeze({
    base: root,
    local: near,
    /** The URL of a server path (`/file?path=…`), to fetch, link or load. */
    url: path => join(root, path),
    /** `fetch` of a server path, with the handle's credentials; `init` as `fetch` takes it. */
    fetch: (path, init = {}) => fetch(join(root, path), { credentials, ...init }),
    /** The URL of one of mdrev's paths on this server (its bundle, `/markdown`, `/api/mdrev…`). */
    mdrevUrl: path => join(mdrevRoot, path),
    /** `fetch` of one of mdrev's paths on this server. */
    mdrevFetch: (path, init = {}) => fetch(join(mdrevRoot, path), { credentials, ...init }),
  });
}

/** The base of the server that served this page: the directory its address names. "" for a page
 *  at the root (the monitor's), "/agents/monitor" for one a host serves at "/agents/monitor/"
 *  (#s426), and for that host's detached tabs ("/agents/monitor/markdown?…") alike. The page's own
 *  links are written relative to it for the same reason. A page served at a prefix with no trailing
 *  slash names its parent: a host serves it at the slash. */
export function pageBase(href = typeof location === "undefined" ? "" : location.href) {
  if (!href) return "";
  try {
    return new URL(".", href).pathname.replace(/\/+$/, "");
  } catch (_) {
    return "";
  }
}

let current = backendHandle({ base: pageBase() });

/** The handle the page's components use now. */
export const backend = () => current;

/** Point the page's components at another server: a host calls it before it mounts them. */
export function useBackend(handle) {
  current = handle;
}
