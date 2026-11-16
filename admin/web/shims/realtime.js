// arcade.realtime (LiteWorldsQuest ArcadeOS): rooms with players on any arcade of the mesh. Same messages as the
// other arcades (arcade: 'realtime'): the app window around the page joins the room for it, as "<this app>/<room>".
//
//   arcade.realtime.join('lobby').then(function (room) {
//     room.on('message', function (data, from) { ... });   // data: parsed JSON when it is JSON
//     room.on('join', function (p) {}); room.on('leave', function (p) {});
//     room.send({x: 1});                                    // at most 512 bytes, about 5 a second
//     room.members(); room.leave();
//   });
//
// A room that cannot be joined still resolves, with online: false and why (playing alone is always possible).
// Not on this arcade yet: the fast lane and media (room.fast, room.media are null), area(), ring().
(function () {
  if (window.arcade && window.arcade.realtime) return;
  var seq = 0, waiting = {}, rooms = {};
  function ask(msg, wait) {
    return new Promise(function (resolve, reject) {
      if (window.parent === window) { reject(new Error('no app window: open this page in the arcade')); return; }
      var n = ++seq;
      waiting[n] = { resolve: resolve, reject: reject };
      setTimeout(function () { if (waiting[n]) { delete waiting[n]; reject(new Error('no app window answered')); } }, wait || 20000);
      msg.arcade = 'realtime'; msg.seq = n;
      window.parent.postMessage(msg, '*');
    });
  }
  window.addEventListener('message', function (e) {
    var m = e.data || {};
    if (e.source !== window.parent || m.arcade !== 'realtime') return;
    if (m.event) { var r = rooms[m.event.room]; if (r) r._hear(m.event); return; }
    var w = waiting[m.seq];
    if (!w || m.pending) return;
    delete waiting[m.seq];
    if (m.ok) w.resolve(m); else w.reject(new Error(m.error || 'refused'));
  });
  function Room(name, said) {
    this.name = name;
    this.online = !!said.online;
    this.me = said.me || null;
    this.why = said.why || '';
    this.fast = null;
    this.media = null;
    this._members = {};
    this._on = { message: [], join: [], leave: [], closed: [], rejoined: [], reconnecting: [] };
    var self = this;
    (said.members || []).forEach(function (p) { self._members[p.id] = p; });
  }
  Room.prototype.on = function (type, fn) {
    if (!this._on[type]) throw new Error('events are: message, join, leave, closed, rejoined, reconnecting');
    this._on[type].push(fn);
    return this;
  };
  Room.prototype._emit = function (type, a, b) {
    this._on[type].forEach(function (fn) { try { fn(a, b); } catch (err) { setTimeout(function () { throw err; }); } });
  };
  Room.prototype._hear = function (ev) {
    var p = ev.member;
    if (ev.type === 'join') { this._members[p.id] = p; this._emit('join', p); }
    else if (ev.type === 'leave') { delete this._members[p.id]; this._emit('leave', p); }
    else if (ev.type === 'closed') { this.online = false; this._emit('closed', ev.why || ''); }
    else if (ev.type === 'reconnecting') { this.online = false; this._emit('reconnecting'); }
    else if (ev.type === 'rejoined') { this.me = p || this.me; this.online = true; this._emit('rejoined', this.me, null); }
    else if (ev.type === 'message') {
      var data = ev.data;
      if (typeof data === 'string') { try { data = JSON.parse(data); } catch (err) {} }
      this._emit('message', data, p);
    }
  };
  Room.prototype.members = function () { var out = []; for (var k in this._members) out.push(this._members[k]); return out; };
  Room.prototype.send = function (value) {
    if (!this.online) return Promise.reject(new Error('this room is not online'));
    return ask({ op: 'send', room: this.name, data: typeof value === 'string' ? value : JSON.stringify(value) });
  };
  Room.prototype.leave = function () {
    delete rooms[this.name];
    this.online = false;
    return ask({ op: 'leave', room: this.name }).catch(function () {});
  };
  function joinOne(name, options) {
    if (rooms[name]) return Promise.resolve(rooms[name]);
    return ask({ op: 'join', room: name, game: options.game || '', fast: !!options.fast, waits: true }, 60000).then(function (said) {
      var room = new Room(name, said);
      if (room.online) rooms[name] = room;
      return room;
    }, function (err) { return new Room(name, { online: false, why: String(err && err.message || err) }); });
  }
  function join(name, options) {
    options = options || {};
    name = String(name || '');
    var cap = options.cap | 0;
    if (!cap) return joinOne(name, options);
    function tryN(n) {
      var label = n === 1 ? name : name + '#' + n;
      return joinOne(label, options).then(function (room) {
        if (!room.online || n >= 50) return room;
        var others = room.members().filter(function (p) { return !room.me || p.id !== room.me.id; });
        if (others.length < cap) return room;
        return room.leave().then(function () { return tryN(n + 1); });
      });
    }
    return tryN(1);
  }
  function notYet() { return Promise.reject(new Error('not available on this arcade yet')); }
  window.arcade = window.arcade || {};
  window.arcade.realtime = { join: join, area: notYet, ring: notYet, ringed: function () { return Promise.resolve(null); }, MAX_BYTES: 512 };
})();
