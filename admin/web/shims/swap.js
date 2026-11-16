// arcade.swap (LiteWorldsQuest ArcadeOS): trades between two players of a game. Same messages as the other arcades
// (arcade: 'swap'); the app window around the page does all of it: it asks each player in their own window, signs
// in their browser and sends one transaction that moves both sides or neither. The page never sees a key.
//
//   <script src="/r/swap.js"></script>
//   arcade.swap.onTrade(function (t) { show(t.id, t.status, t.give, t.get); });
//   arcade.swap.trade({with: player.id, give: {inscription: id}, get: {coins: '2.5'}}).then(function (r) { r.id; });
//   arcade.swap.answer(id, true);   arcade.swap.cancel(id);   arcade.swap.items();
//
// Both players must be in one arcade.realtime room and signed in with their @name (guests cannot trade).
// A side is {inscription: id} or {coins: "1.5"}; a list on either side makes it a bundle (several things each way,
// coins one way only). At least one side holds an inscription. Tokens are refused here: this arcade keeps no token
// ledger. Pepecoin testnet only.
// onTrade(fn): fn({type: 'trade', id, status, with, give, get, role, why, txid}); status: proposed, incoming,
// accepted, signed, broadcast, settled, declined, cancelled, failed. give/get are always from YOUR side.
// Not on this arcade yet: shops (shop, offer, accept, buy and the await* helpers reject with a reason).
(function () {
  if (window.arcade && window.arcade.swap) return;
  var seq = 0, waiting = {}, listeners = [];
  function ask(msg, wait) {
    return new Promise(function (resolve, reject) {
      if (window.parent === window) { reject(new Error('no app window: open this page in the arcade')); return; }
      var n = ++seq;
      waiting[n] = { resolve: resolve, reject: reject };
      setTimeout(function () { if (waiting[n]) { delete waiting[n]; reject(new Error('no app window answered')); } }, wait || 60000);
      msg.arcade = 'swap'; msg.seq = n;
      window.parent.postMessage(msg, '*');
    });
  }
  window.addEventListener('message', function (e) {
    var m = e.data || {};
    if (e.source !== window.parent || m.arcade !== 'swap') return;
    if (m.event && m.event.type === 'trade') {
      listeners.forEach(function (fn) { try { fn(m.event); } catch (err) { setTimeout(function () { throw err; }); } });
      return;
    }
    var w = waiting[m.seq];
    if (!w) return;
    delete waiting[m.seq];
    if (m.ok) w.resolve(m); else w.reject(new Error(m.error || 'refused'));
  });
  function strip(m) { var o = {}; for (var k in m) if (k !== 'arcade' && k !== 'seq' && k !== 'ok') o[k] = m[k]; return o; }
  function notHere() { return Promise.reject(new Error('shops are not available on this arcade yet: trades between players are')); }
  var swap = {
    trade: function (t) {
      t = t || {};
      return ask({ op: 'trade', with: String(t.with || ''), give: t.give, get: t.get }, 600000).then(strip);
    },
    answer: function (id, accept, why) {
      return ask({ op: 'answer', id: String(id || ''), accept: !!accept, why: why ? String(why) : '' }, 600000).then(strip);
    },
    cancel: function (id) { return ask({ op: 'cancel', id: String(id || '') }).then(strip); },
    items: function () { return ask({ op: 'items' }).then(strip); },
    onTrade: function (fn) { if (typeof fn === 'function') listeners.push(fn); return swap; },
    shop: notHere, offer: notHere, accept: notHere, status: notHere,
    awaitOffer: notHere, awaitDecision: notHere, awaitSwap: notHere, buy: notHere,
    describe: function (leg) {
      if (!leg) return '?';
      if (leg.inscription) return 'inscription ' + String(leg.inscription).slice(0, 10);
      if (leg.coins) return String(leg.coins) + ' coins';
      if (leg.kind === 'inscription') return 'inscription #' + leg.number;
      if (leg.kind === 'coins') return String(leg.amount) + ' coins';
      return 'something';
    }
  };
  window.arcade = window.arcade || {};
  window.arcade.swap = swap;
})();
