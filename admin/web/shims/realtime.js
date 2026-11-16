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
// With {fast: true}: room.fast (see Fast). Not on this arcade yet: media (room.media is null), area(), ring().
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
  // The fast lane: every pair of players in a room joined with {fast: true} is linked directly (WebRTC data,
  // made by the app window); a pair that cannot link directly is carried over the room. Events: message, peer.
  function Fast(room) { this.room = room; this._peers = {}; this._on = { message: [], peer: [] }; }
  Fast.prototype.on = function (type, fn) {
    if (!this._on[type]) throw new Error('fast lane events are: message, peer');
    this._on[type].push(fn);
    return this;
  };
  Fast.prototype._emit = function (type, a, b) { this._on[type].forEach(function (fn) { try { fn(a, b); } catch (err) { setTimeout(function () { throw err; }); } }); };
  Fast.prototype.send = function (value, options) {
    options = options || {};
    var data;
    if (value instanceof ArrayBuffer) data = value.slice(0);
    else if (ArrayBuffer.isView(value)) data = value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength);
    else data = typeof value === 'string' ? value : JSON.stringify(value);
    var m = { arcade: 'realtime', seq: 0, op: 'fast', room: this.room.name, data: data, reliable: !!options.reliable };
    if (options.to != null) m.to = [].concat(options.to).map(String);
    window.parent.postMessage(m, '*');
  };
  Fast.prototype.routes = function () {
    return ask({ op: 'fastroutes', room: this.room.name }).then(function (s) { return s.routes || {}; }, function () { return {}; });
  };
  Fast.prototype.link = function (ids, options) {
    return ask({ op: 'fastlink', room: this.room.name, link: ids == null ? null : [].concat(ids).map(String), max: (options && options.max) | 0 })
      .then(function (s) { return { ok: true, fast: !!s.fast }; }, function (err) { return { ok: false, error: err.message }; });
  };
  Fast.prototype.peers = function () { var o = {}; for (var k in this._peers) o[k] = this._peers[k]; return o; };
  function Room(name, said) {
    this.name = name;
    this.online = !!said.online;
    this.me = said.me || null;
    this.why = said.why || '';
    this.fast = said.fast ? new Fast(this) : null;
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
    else if (ev.type === 'fast' && this.fast) {
      var said = ev.data;
      if (typeof said === 'string') { try { said = JSON.parse(said); } catch (err) {} }
      this.fast._emit('message', said, p);
    }
    else if (ev.type === 'peer' && this.fast) {
      if (ev.state === 'gone') delete this.fast._peers[p.id]; else this.fast._peers[p.id] = ev.state;
      this.fast._emit('peer', p, ev.state);
    }
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
    var m = { op: 'join', room: name, game: options.game || '', fast: !!options.fast, waits: true };
    if (Array.isArray(options.link)) m.link = options.link.map(String);
    if (options.max) m.max = options.max | 0;
    return ask(m, 60000).then(function (said) {
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
