// The page's storage held in memory (#s124, `--no-storage`): for a page shown where the browser
// refuses its stores, such as a frame sandboxed with scripts but without its origin. The theme,
// the reading preferences, the folds and the view last for the page's life and no longer, and
// the page's text names none of the browser's storage interfaces.
var crPageStore = (function () {
  var kept = {}, tab = {};
  function has(map, k) { return Object.prototype.hasOwnProperty.call(map, k); }
  return {
    get: function (k) { return has(kept, k) ? kept[k] : null; },
    set: function (k, v) { kept[k] = String(v); },
    del: function (k) { delete kept[k]; },
    tabGet: function (k) { return has(tab, k) ? tab[k] : null; },
    tabSet: function (k, v) { tab[k] = String(v); }
  };
})();
