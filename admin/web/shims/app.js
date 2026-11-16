// arcade.app (LiteWorldsQuest ArcadeOS): the app's state in its window (open; minimized/closing when a home screen
// says so) and a way to be told: arcade.app.on(fn), arcade.app.state(). Same messages as the other arcades
// (arcade: 'app'); events also as the 'arcade:app' DOM event.
(function () {
  var seq = 0, waiting = {}, fns = [], last = { state: 'open', timeout: null, closesAt: null, inShell: false };
  function strip(m) { var o = {}; for (var k in m) if (k !== 'arcade' && k !== 'seq' && k !== 'event' && k !== 'ok') o[k] = m[k]; return o; }
  window.addEventListener('message', function (e) {
    var m = e.data || {};
    if (e.source !== window.parent || m.arcade !== 'app') return;
    if (m.event) {
      last = strip(m);
      fns.slice().forEach(function (fn) { try { fn(last); } catch (err) { setTimeout(function () { throw err; }); } });
      try { window.dispatchEvent(new CustomEvent('arcade:app', { detail: last })); } catch (err) {}
      return;
    }
    if (!waiting[m.seq]) return;
    var w = waiting[m.seq]; delete waiting[m.seq];
    if (m.ok) w.resolve(m); else w.reject(new Error(m.error || 'refused'));
  });
  function ask(body, wait) {
    return new Promise(function (resolve, reject) {
      if (window.parent === window) { reject(new Error('no app window: open this page in the arcade')); return; }
      var n = ++seq;
      waiting[n] = { resolve: resolve, reject: reject };
      setTimeout(function () { if (waiting[n]) { delete waiting[n]; reject(new Error('no app window answered')); } }, wait);
      body.arcade = 'app'; body.seq = n;
      window.parent.postMessage(body, '*');
    });
  }
  window.arcade = window.arcade || {};
  window.arcade.app = {
    on: function (fn) { if (typeof fn === 'function') fns.push(fn); return fn; },
    off: function (fn) { fns = fns.filter(function (f) { return f !== fn; }); },
    state: function () { return ask({ op: 'state' }, 15000).then(function (m) { last = strip(m); return last; }); }
  };
})();
