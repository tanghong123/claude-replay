// A case's wrapper in front of the session view (#s429 stage 6). The case answers the app's request
// for `/monitor-ui/session-view.js` with this module (harness::answer_in_browser), so the app shell
// mounts the view through it unchanged; it imports the real module under another address (the
// monitor drops the query) and records each event the view raises into a handler the APP passed,
// then calls the app's own. Nothing of it is served by the monitor.
export * from "/monitor-ui/session-view.js?real=1";
import { mountSessionView as mount } from "/monitor-ui/session-view.js?real=1";

/** Plain data: what JSON carries, with no node, function or class instance anywhere in it. */
const plain = value =>
  value === null || ["string", "number", "boolean"].includes(typeof value) ||
  (Array.isArray(value) ? value.every(plain) : typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype && Object.values(value).every(plain));

export function mountSessionView(element, options) {
  const heard = (window.__viewHeard = { passed: [], said: [] });
  const wrapped = { ...options };
  for (const [name, own] of Object.entries(options)) {
    if (!/^on[A-Z]/.test(name) || typeof own !== "function") continue;
    heard.passed.push(name);
    wrapped[name] = (...args) => {
      heard.said.push({ name, args: JSON.parse(JSON.stringify(args)), plain: args.every(plain) });
      return own(...args);
    };
  }
  return mount(element, wrapped);
}
