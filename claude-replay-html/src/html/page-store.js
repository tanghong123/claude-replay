// The page's storage (#s124): every read and write this page makes to the browser's own stores
// goes through `crPageStore`, so an export built with `--no-storage` can swap in
// page-store-memory.js and carry no reference to them at all. Each access is guarded: a private
// window, a frame sandboxed without its origin or a full quota throws, and the page then forgets
// instead of failing. `get`/`set`/`del` persist; `tabGet`/`tabSet` last for the tab.
var crPageStore = (function () {
  function guard(fn, fallback) { try { return fn(); } catch (e) { return fallback; } }
  return {
    get: function (k) { return guard(function () { return localStorage.getItem(k); }, null); },
    set: function (k, v) { guard(function () { localStorage.setItem(k, v); }); },
    del: function (k) { guard(function () { localStorage.removeItem(k); }); },
    tabGet: function (k) { return guard(function () { return sessionStorage.getItem(k); }, null); },
    tabSet: function (k, v) { guard(function () { sessionStorage.setItem(k, v); }); }
  };
})();
