// arcade.access (LiteWorldsQuest ArcadeOS): what this app may use. status(); wallet() asks the player for wallet
// access; nodes() and move() (playing on another arcade). Same messages as the other arcades (arcade: 'access').
(function () {
  var seq = 0, waiting = {};
  window.addEventListener('message', function (e) {
    var m = e.data || {};
    if (e.source !== window.parent || m.arcade !== 'access' || !waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq];
    if (m.ok) w.resolve(m); else w.reject(new Error(m.error || 'refused'));
  });
  function ask(body, wait) {
    return new Promise(function (resolve, reject) {
      if (window.parent === window) { reject(new Error('no app window: open this page in the arcade')); return; }
      var n = ++seq;
      waiting[n] = { resolve: resolve, reject: reject };
      setTimeout(function () { if (waiting[n]) { delete waiting[n]; reject(new Error('no app window answered')); } }, wait);
      body.arcade = 'access'; body.seq = n;
      window.parent.postMessage(body, '*');
    });
  }
  function strip(m) { var o = {}; for (var k in m) if (k !== 'arcade' && k !== 'seq') o[k] = m[k]; return o; }
  window.arcade = window.arcade || {};
  window.arcade.access = {
    status: function () { return ask({ op: 'status' }, 15000).then(strip); },
    wallet: function () { return ask({ op: 'wallet' }, 300000).then(strip); },
    nodes: function () { return ask({ op: 'nodes' }, 30000).then(function (m) { return m.nodes || []; }); },
    move: function (base) { if (base && typeof base === 'object') base = base.base; return ask({ op: 'move', to: String(base || '') }, 300000).then(strip); }
  };
})();
