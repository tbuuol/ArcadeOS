// arcade.storage (LiteWorldsQuest ArcadeOS): what this arcade keeps for this page. The page runs in a sandbox
// without a storage of its own; the app window around it keeps it (per account and page), in the same shape as
// localStorage: getItem, setItem, removeItem, clear, key, length, all(), history(key). Reads are synchronous once
// `ready` has resolved; writes return promises. Same messages as the other arcades (arcade: 'storage').
(function () {
  var items = {}, seq = 0, waiting = {};
  function ask(msg, raw) {
    return new Promise(function (resolve, reject) {
      if (window.parent === window) { reject(new Error('no app window: open this page in the arcade')); return; }
      var n = ++seq;
      waiting[n] = { resolve: resolve, reject: reject, raw: !!raw };
      setTimeout(function () { if (waiting[n]) { delete waiting[n]; reject(new Error('no app window answered')); } }, 8000);
      msg.arcade = 'storage'; msg.seq = n;
      window.parent.postMessage(msg, '*');
    });
  }
  window.addEventListener('message', function (e) {
    var m = e.data || {};
    if (e.source !== window.parent || m.arcade !== 'storage' || !waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq];
    if (!m.ok) { w.reject(new Error(m.error || 'refused')); return; }
    if (w.raw) { w.resolve(m.history || {}); return; }
    if (m.items) items = m.items;
    w.resolve(store);
  });
  var has = function (k) { return Object.prototype.hasOwnProperty.call(items, k); };
  var store = {
    getItem: function (k) { k = String(k); return has(k) ? items[k] : null; },
    setItem: function (k, v) {
      k = String(k); v = String(v);
      var had = has(k), before = items[k];
      items[k] = v;
      return ask({ op: 'set', key: k, value: v }).catch(function (err) { if (had) items[k] = before; else delete items[k]; throw err; });
    },
    removeItem: function (k) { k = String(k); delete items[k]; return ask({ op: 'remove', key: k }); },
    clear: function () { items = {}; return ask({ op: 'clear' }); },
    history: function (k) { return ask({ op: 'history', key: k === undefined ? '' : String(k) }, true); },
    key: function (i) { return Object.keys(items)[i] || null; },
    get length() { return Object.keys(items).length; },
    all: function () { var o = {}; for (var k in items) o[k] = items[k]; return o; }
  };
  store.ready = ask({ op: 'load' });
  window.arcade = window.arcade || {};
  window.arcade.storage = store;
})();
