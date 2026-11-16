// App-Fenster (LiteWorldsQuest ArcadeOS): oeffnet eine eingeschriebene App, die der Betreiber freigeschaltet hat, in
// einer Sandbox (eigener leerer Ursprung: kein Zugriff auf diese Seite, ihre Cookies oder den Browser-Speicher mit
// den Woertern) und beantwortet ihre Fragen - nur die aus genau diesem Rahmen:
//   storage  arcade.storage: angemeldet auf der Box (je Konto und Seite), als Gast nur in diesem Browser
//   app      arcade.app: Zustand (offen)
//   access   arcade.access: Status; wallet() fragt den angemeldeten Nutzer (Karte), Zustimmung je Konto und App
//   senden   POST /r/send der App: Karte mit Empfaenger, Betrag/Inschrift, Gebuehr; signiert nur nach dem Ja (hier)
//   realtime arcade.realtime: Raeume mit Spielern aller Arcades (als Gast), schnelle Spur (WebRTC); ohne Medien
const $ = (s) => document.querySelector(s);
const id = new URLSearchParams(location.search).get('id') || '';
let frame = null, page = '', account = null;

async function get(url) {
  const r = await fetch(url, { cache: 'no-store', credentials: 'same-origin' });
  if (!r.ok) throw Object.assign(new Error(`HTTP ${r.status}`), { status: r.status });
  return r.json();
}
async function post(url, body) {
  const r = await fetch(url, { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || `HTTP ${r.status}`);
  return j;
}

// ------------------------------------------------------------------ Speicher
const localKey = () => `arcade-page:${page}`;
function localItems() { try { return JSON.parse(localStorage.getItem(localKey()) || '{}'); } catch { return {}; } }
function localSave(items) { try { localStorage.setItem(localKey(), JSON.stringify(items)); return true; } catch { return false; } }

async function storage(m) {
  if (account) {
    if (m.op === 'load') return { items: (await get(`/api/storage/${page}`)).items || {} };
    const r = await post(`/api/storage/${page}`, { op: m.op, key: m.key, value: m.value });
    return m.op === 'history' ? { history: r.history || {} } : {};
  }
  // Gast: nur in diesem Browser, ohne Verlauf
  const items = localItems();
  if (m.op === 'load') return { items };
  if (m.op === 'history') return { history: {} };
  if (m.op === 'set') { if (String(m.value).length > 65536) throw new Error('a value is at most 65536 characters'); items[String(m.key)] = String(m.value); }
  else if (m.op === 'remove') delete items[String(m.key)];
  else if (m.op === 'clear') for (const k of Object.keys(items)) delete items[k];
  else throw new Error('unknown op');
  if (!localSave(items)) throw new Error('this browser keeps nothing for the page');
  return {};
}

// ------------------------------------------------------------------ Raeume (arcade.realtime)
// Der Raum heisst im Mesh "<diese Seite>/<Raum>"; die Marke kennt nur dieses Fenster. Ereignisse holt es per
// Long-Polling und reicht sie an den Rahmen weiter.
//
// SCHNELLE SPUR (join mit {fast: true}): jedes Spielerpaar wird direkt verbunden (WebRTC, zwei Datenkanaele: 'u'
// ungeordnet ohne Wiederholung, 'r' geordnet zuverlaessig). Ausgehandelt wird ueber den Raum, als versteckte
// Nachrichten im Format der Arcades ("\u001farcade-rtc:" + {to, k, i, n, d}, Stuecke zu 300 Zeichen): die kleinere
// Kennung bietet an, jede Seite schickt EINE verkleinerte Beschreibung mit ihren Kandidaten (kein Nachreichen).
// Kommt keine direkte Verbindung zustande (12 s), traegt der Raum das Paar ('room'), hoechstens 3 Nachrichten/s.
// Die Verbindungen leben HIER, nicht in der App in der Sandbox; die App hoert {type: 'fast'} und {type: 'peer'}.
const RTC = '\u001farcade-rtc:', FAST_WAIT = 12000, FAST_MAX = 16384, FAST_PEERS = 19, FAST_PEERS_MAX = 60, RTC_PIECE = 300, FAST_TO_MAX = 64;
const ICE = [{ urls: 'stun:stun.cloudflare.com:3478' }];
const seats = {};   // Raumname der Seite -> Platz

function tell(name, ev) { if (frame) frame.contentWindow.postMessage({ arcade: 'realtime', event: { ...ev, room: name } }, '*'); }

function b64(buf) { const u = new Uint8Array(buf); let s = ''; for (let i = 0; i < u.length; i++) s += String.fromCharCode(u[i]); return btoa(s); }
function unb64(t) { const x = atob(t), u = new Uint8Array(x.length); for (let i = 0; i < x.length; i++) u[i] = x.charCodeAt(i); return u.buffer; }

// Beschreibung verkleinert (wie die Arcades): ICE-Name und -Passwort, DTLS-Fingerabdruck und Rolle, bis 10 UDP-Kandidaten
const SDP_KEEP = { r: 3, s: 4, p: 2, h: 10 }, SDP_MOST = 10;
function sdpPack(sdp, kind) {
  const out = { t: kind, c: [] }, found = { r: [], s: [], p: [], h: [] };
  for (const l of sdp.split(/\r?\n/)) {
    let m;
    if ((m = /^a=ice-ufrag:(.+)$/.exec(l))) out.u = m[1];
    else if ((m = /^a=ice-pwd:(.+)$/.exec(l))) out.p = m[1];
    else if ((m = /^a=fingerprint:sha-256 (.+)$/i.exec(l))) out.f = m[1].replace(/:/g, '');
    else if ((m = /^a=setup:(\w+)$/.exec(l))) out.s = m[1];
    else if ((m = /^a=candidate:\S+ 1 udp \d+ (\S+) (\d+) typ (host|srflx|prflx|relay)/i.exec(l))) {
      const k = m[3][0].toLowerCase();
      if (found[k].length < SDP_KEEP[k]) found[k].push(`${m[1]} ${m[2]} ${k}`);
    }
  }
  for (const k of ['r', 's', 'p', 'h']) for (const c of found[k]) if (out.c.length < SDP_MOST) out.c.push(c);
  return out;
}
function sdpUnpack(o) {
  const types = { h: ['host', 2130706431], s: ['srflx', 1694498815], p: ['prflx', 1862270975], r: ['relay', 16777215] };
  const fp = String(o.f || '').toUpperCase().match(/../g) || [];
  const lines = ['v=0', `o=- ${Date.now()} 2 IN IP4 127.0.0.1`, 's=-', 't=0 0', 'a=group:BUNDLE 0', 'a=msid-semantic: WMS',
    'm=application 9 UDP/DTLS/SCTP webrtc-datachannel', 'c=IN IP4 0.0.0.0', `a=ice-ufrag:${o.u}`, `a=ice-pwd:${o.p}`,
    `a=fingerprint:sha-256 ${fp.join(':')}`, `a=setup:${o.s}`, 'a=mid:0', 'a=sctp-port:5000', 'a=max-message-size:262144'];
  (o.c || []).forEach((c, i) => {
    const f = String(c).split(' '), t = types[f[2]] || types.h;
    lines.push(`a=candidate:${i + 1} 1 udp ${t[1] - i} ${f[0]} ${f[1]} typ ${t[0]}${t[0] === 'host' ? '' : ' raddr 0.0.0.0 rport 0'}`);
  });
  lines.push('a=end-of-candidates');
  return { type: o.t === 'o' ? 'offer' : 'answer', sdp: lines.join('\r\n') + '\r\n' };
}

const wantsLink = (s, id) => !s.link || s.link.includes(String(id));
function peerSay(name, s, p, state) {
  const peer = s.fast && s.fast.peers[p.id];
  if (peer) { if (peer.state === state) return; peer.state = state; }
  tell(name, { type: 'peer', member: p, state });
}
function fastSignal(name, s, to, body) {
  const text = JSON.stringify(body), k = (++s.fast.k).toString(36), n = Math.ceil(text.length / RTC_PIECE);
  for (let i = 0; i < n; i++) s.fast.out.push({ to, k, i, n, d: text.slice(i * RTC_PIECE, (i + 1) * RTC_PIECE) });
  fastPump(name, s);
}
const viaRoom = (s) => Object.values(s.fast.peers).some((p) => p.state === 'room');
// Was die schnelle Spur im Raum sagt: hoechstens 3/s (die App behaelt Platz fuer ihre eigenen 5/s)
function fastPump(name, s) {
  if (!s.fast || s.fast.pump) return;
  s.fast.pump = setInterval(() => {
    if (!s.fast || seats[name] !== s) return;
    let msg = s.fast.out.shift();
    if (!msg && viaRoom(s)) {
      let it = null;
      if (s.fast.reliable.length) { it = s.fast.reliable.shift(); msg = { f: 1, r: 1, d: it.d }; }
      else if (s.fast.latest != null) { it = s.fast.latest; msg = { f: 1, d: it.d }; s.fast.latest = null; }
      if (it && it.b) msg.b = 1;
      if (it && it.t) msg.t = it.t;
    }
    if (!msg) { if (!viaRoom(s)) { clearInterval(s.fast.pump); s.fast.pump = null; } return; }
    post('/rt/send', { token: s.token, data: RTC + JSON.stringify(msg) }).catch(() => {});
  }, 340);
}
function fastDrop(s, id) {
  const peer = s.fast && s.fast.peers[id];
  if (!peer) return;
  clearTimeout(peer.timer);
  try { if (peer.pc) peer.pc.close(); } catch {}
  delete s.fast.peers[id];
}
function fastMeet(name, s, p) {
  if (!s.fast || p.id === s.me.id || s.fast.peers[p.id] || !wantsLink(s, p.id)) return;
  if (Object.keys(s.fast.peers).length >= (s.fastMax || FAST_PEERS)) return;
  const peer = s.fast.peers[p.id] = { p, state: '', tries: 0, born: Date.now() };
  peerSay(name, s, p, 'connecting');
  if (s.me.id < p.id) fastOffer(name, s, peer);
  peer.timer = setTimeout(() => {
    if (s.fast && s.fast.peers[p.id] === peer && peer.state !== 'direct') { peerSay(name, s, p, 'room'); fastPump(name, s); }
  }, FAST_WAIT);
}
function fastPc(name, s, peer) {
  try { if (peer.pc) peer.pc.close(); } catch {}
  const pc = peer.pc = new RTCPeerConnection({ iceServers: ICE });
  peer.u = pc.createDataChannel('u', { negotiated: true, id: 0, ordered: false, maxRetransmits: 0 });
  peer.r = pc.createDataChannel('r', { negotiated: true, id: 1, ordered: true });
  for (const dc of [peer.u, peer.r]) {
    dc.binaryType = 'arraybuffer';
    dc.onopen = () => { if (peer.pc === pc) { clearTimeout(peer.timer); peerSay(name, s, peer.p, 'direct'); } };
    dc.onmessage = (ev) => tell(name, { type: 'fast', member: peer.p, data: ev.data });
  }
  pc.onconnectionstatechange = () => {
    if (peer.pc !== pc || !s.fast || s.fast.peers[peer.p.id] !== peer) return;
    if (pc.connectionState === 'failed' || pc.connectionState === 'closed') {
      peerSay(name, s, peer.p, 'room');
      fastPump(name, s);
      if (s.me.id < peer.p.id && peer.tries < 3) setTimeout(() => { if (s.fast && s.fast.peers[peer.p.id] === peer) fastOffer(name, s, peer); }, 2000 * peer.tries);
    }
  };
  return pc;
}
// bis alle Kandidaten da sind, hoechstens 2,5 s
function gathered(pc) {
  return new Promise((done) => {
    if (pc.iceGatheringState === 'complete') return done();
    const t = setTimeout(done, 2500);
    pc.addEventListener('icegatheringstatechange', () => { if (pc.iceGatheringState === 'complete') { clearTimeout(t); done(); } });
  });
}
async function fastOffer(name, s, peer) {
  peer.tries++;
  try {
    const pc = fastPc(name, s, peer);
    await pc.setLocalDescription(await pc.createOffer());
    await gathered(pc);
    if (peer.pc === pc) fastSignal(name, s, peer.p.id, sdpPack(pc.localDescription.sdp, 'o'));
  } catch {}
}
async function fastAnswer(name, s, p, o) {
  let peer = s.fast.peers[p.id];
  if (!peer) { fastMeet(name, s, p); peer = s.fast.peers[p.id]; }
  if (!peer) { if (!wantsLink(s, p.id)) fastSignal(name, s, p.id, { t: 'n' }); return; }
  try {
    const pc = fastPc(name, s, peer);
    await pc.setRemoteDescription(sdpUnpack(o));
    await pc.setLocalDescription(await pc.createAnswer());
    await gathered(pc);
    if (peer.pc === pc) fastSignal(name, s, p.id, sdpPack(pc.localDescription.sdp, 'a'));
  } catch {}
}
// Eine versteckte Nachricht der schnellen Spur aus dem Raum
function rtcHeard(name, s, p, text) {
  let msg; try { msg = JSON.parse(text); } catch { return; }
  if (!s.fast || !p || p.id === s.me.id) return;
  if (msg.f) {                                   // Daten, die der Raum fuer ein Paar ohne direkte Verbindung traegt
    const peer = s.fast.peers[p.id];
    if (peer && peer.state === 'direct') return;
    if (msg.t && (!Array.isArray(msg.t) || !msg.t.includes(s.me.id))) return;
    if (typeof msg.d === 'string') { try { tell(name, { type: 'fast', member: p, data: msg.b ? unb64(msg.d) : msg.d }); } catch {} }
    return;
  }
  if (msg.to !== s.me.id || typeof msg.d !== 'string') return;
  const key = `${p.id}/${msg.k}`, got = s.fast.parts[key] || (s.fast.parts[key] = []);
  got[msg.i | 0] = msg.d;
  const n = msg.n | 0;
  if (n < 1 || n > 8) { delete s.fast.parts[key]; return; }
  for (let i = 0; i < n; i++) if (got[i] == null) return;
  delete s.fast.parts[key];
  let o; try { o = JSON.parse(got.join('')); } catch { return; }
  if (o.t === 'n') { if (s.fast.peers[p.id]) { fastDrop(s, p.id); peerSay(name, s, p, 'gone'); } return; }
  if (o.t === 'h') {                             // sie wollen die Verbindung und warten auf unser Angebot
    let ph = s.fast.peers[p.id];
    if (ph && o.s && ph.s && ph.s !== o.s) { fastDrop(s, p.id); ph = null; }
    if (!ph) { fastMeet(name, s, p); ph = s.fast.peers[p.id]; if (ph) ph.s = o.s; return; }
    if (o.s) ph.s = o.s;
    if (ph.state !== 'direct' && s.me.id < p.id && !(ph.tries && Date.now() - ph.born < 8000)) fastOffer(name, s, ph);
    return;
  }
  if (o.t === 'o') fastAnswer(name, s, p, o);
  else if (o.t === 'a') {
    const pr = s.fast.peers[p.id];
    if (pr && pr.pc && pr.pc.signalingState === 'have-local-offer') pr.pc.setRemoteDescription(sdpUnpack(o)).catch(() => {});
  }
}
// fast.send der App: direkt an jedes verbundene Gegenueber, ueber den Raum an die ohne Verbindung
function fastSend(s, m) {
  if (!s.fast) return;
  const bin = m.data instanceof ArrayBuffer;
  const data = bin ? m.data : String(m.data == null ? '' : m.data);
  if ((bin ? data.byteLength : data.length) > FAST_MAX) return;
  const to = Array.isArray(m.to) ? m.to.slice(0, FAST_TO_MAX).map(String) : null;
  let room = false;
  for (const [id, peer] of Object.entries(s.fast.peers)) {
    if (to && !to.includes(id)) continue;
    const dc = m.reliable ? peer.r : peer.u;
    if (peer.state === 'direct' && dc && dc.readyState === 'open') { try { dc.send(data); } catch {} }
    else if (peer.state === 'room') room = true;
  }
  if (room) {
    const d = bin ? b64(data) : data, it = { d };
    if (bin) it.b = 1;
    if (to) it.t = to;
    if (d.length <= 400 && JSON.stringify(it).length <= 470) {
      if (m.reliable) { if (s.fast.reliable.length < 50) s.fast.reliable.push(it); } else s.fast.latest = it;
    }
  }
}
function fastStart(name, s) {
  if (typeof RTCPeerConnection !== 'function') return;     // kein WebRTC: der Raum traegt alles
  s.fast = { peers: {}, out: [], parts: {}, latest: null, reliable: [], k: 0, s: Math.random().toString(36).slice(2, 10) };
  for (const p of Object.values(s.members)) {
    fastMeet(name, s, p);
    if (s.fast.peers[p.id] && p.id < s.me.id) fastSignal(name, s, p.id, { t: 'h', s: s.fast.s });
  }
}
function fastStop(s) {
  if (!s || !s.fast) return;
  for (const id of Object.keys(s.fast.peers)) fastDrop(s, id);
  clearInterval(s.fast.pump);
  s.fast = null;
}

// Ereignisse des Raums: versteckte Nachrichten (\u001f...) gehen nicht an die App
function heard(name, s, ev) {
  const p = ev.member;
  if (ev.type === 'message' && typeof ev.data === 'string' && ev.data.startsWith('\u001f')) {
    if (ev.data.startsWith(RTC)) rtcHeard(name, s, p, ev.data.slice(RTC.length));
    return;
  }
  if (ev.type === 'join' && p) { s.members[p.id] = p; fastMeet(name, s, p); }
  if (ev.type === 'leave' && p) { delete s.members[p.id]; if (s.fast && s.fast.peers[p.id]) { fastDrop(s, p.id); peerSay(name, s, p, 'gone'); } }
  if (ev.type === 'rejoined' && s.fast) { fastStop(s); fastStart(name, s); }
  tell(name, ev);
}
async function pollSeat(name, s) {
  while (s.live) {
    try {
      const r = await get(`/rt/poll?token=${s.token}&wait=25`);
      for (const ev of r.events || []) heard(name, s, ev);
    } catch (e) {
      if (e.status === 404) { s.live = false; fastStop(s); tell(name, { type: 'closed', why: 'the arcade closed this seat' }); return; }
      await new Promise((ok) => setTimeout(ok, 3000));
    }
  }
}
async function fastRoutes(s) {
  const out = {};
  if (!s.fast) return out;
  await Promise.all(Object.entries(s.fast.peers).map(async ([id, peer]) => {
    if (peer.state !== 'direct' || !peer.pc) { out[id] = peer.state === 'room' ? 'none' : 'connecting'; return; }
    try {
      const st = await peer.pc.getStats();
      let pair = null; const byId = {};
      st.forEach((r) => { byId[r.id] = r; });
      st.forEach((r) => { if (r.type === 'transport' && r.selectedCandidatePairId) pair = byId[r.selectedCandidatePairId]; });
      if (!pair) st.forEach((r) => { if (!pair && r.type === 'candidate-pair' && r.nominated && r.state === 'succeeded') pair = r; });
      const near = pair && byId[pair.localCandidateId], far = pair && byId[pair.remoteCandidateId];
      out[id] = near && near.candidateType === 'relay' || far && far.candidateType === 'relay' ? 'relay' : 'direct';
    } catch { out[id] = 'direct'; }
  }));
  return out;
}
async function realtime(m) {
  const name = String(m.room || '');
  if (m.op === 'join') {
    if (seats[name]) { const s = seats[name]; return { online: true, me: s.me, members: Object.values(s.members), fast: !!s.fast, media: false }; }
    const r = await post('/rt/join', { page, room: name });
    if (!r.online) return { online: false, why: r.why || 'offline', fast: false, media: false };
    const s = seats[name] = { token: r.token, me: r.me, members: {}, live: true, fast: null,
      link: Array.isArray(m.link) ? m.link.slice(0, FAST_PEERS_MAX).map(String) : null, fastMax: Math.max(1, Math.min(FAST_PEERS_MAX, (m.max | 0) || FAST_PEERS)) };
    for (const p of r.members || []) s.members[p.id] = p;
    if (m.fast) fastStart(name, s);
    pollSeat(name, s);
    return { online: true, me: r.me, members: r.members || [], fast: !!s.fast, media: false };
  }
  const s = seats[name];
  if (m.op === 'fast') { if (s) fastSend(s, m); return null; }       // viele je Sekunde: nie beantwortet
  if (!s) throw new Error('join the room first');
  if (m.op === 'send') { await post('/rt/send', { token: s.token, data: String(m.data || '') }); return {}; }
  if (m.op === 'leave') { s.live = false; fastStop(s); delete seats[name]; post('/rt/leave', { token: s.token }).catch(() => {}); return {}; }
  if (m.op === 'fastroutes') return { routes: await fastRoutes(s) };
  if (m.op === 'fastlink') {
    s.link = Array.isArray(m.link) ? m.link.slice(0, FAST_PEERS_MAX).map(String) : null;
    s.fastMax = Math.max(1, Math.min(FAST_PEERS_MAX, (m.max | 0) || FAST_PEERS));
    if (s.fast) {
      for (const id of Object.keys(s.fast.peers)) if (!wantsLink(s, id)) { const p = s.fast.peers[id].p; fastDrop(s, id); peerSay(name, s, p, 'gone'); fastSignal(name, s, p.id, { t: 'n' }); }
      for (const p of Object.values(s.members)) { const had = !!s.fast.peers[p.id]; fastMeet(name, s, p); if (!had && s.fast.peers[p.id] && p.id < s.me.id) fastSignal(name, s, p.id, { t: 'h' }); }
    }
    return { fast: !!s.fast };
  }
  throw new Error('not available on this arcade yet');
}
window.addEventListener('pagehide', () => {
  for (const s of Object.values(seats)) {
    s.live = false;
    fastStop(s);
    try { navigator.sendBeacon('/rt/leave', new Blob([JSON.stringify({ token: s.token })], { type: 'application/json' })); } catch {}
  }
});

// ------------------------------------------------------------------ Wallet-Zugriff (arcade.access.wallet)
// Die Karte zeichnet DIESES Fenster ueber dem Rahmen, wo die App nicht hinkommt; die App bekommt nur die Antwort.
// Die Zustimmung liegt auf der Box je Konto und App (/api/access/<Karte>); /r/wallet fragt sie bei jedem Aufruf,
// darum wirken Ja und Zuruecknehmen sofort, ohne die Seite neu zu laden (das Ticket hat der Rahmen schon).
let walletOn = false, asking = null, appInfo = null;

function showWallet() { $('#wallet').classList.toggle('on', walletOn); }

function card(title, text, yes, no) {
  return new Promise((done) => {
    const box = document.createElement('div');
    box.className = 'ask';
    const c = document.createElement('div');
    c.className = 'card';
    const h = document.createElement('h2'); h.textContent = title;
    c.append(h);
    for (const t of text) { const p = document.createElement('p'); p.textContent = t; c.append(p); }
    const row = document.createElement('div'); row.className = 'row';
    const bn = document.createElement('button'); bn.type = 'button'; bn.textContent = no;
    const by = document.createElement('button'); by.type = 'button'; by.className = 'yes'; by.textContent = yes;
    row.append(bn, by); c.append(row); box.append(c);
    const end = (v) => { box.remove(); done(v); };
    bn.onclick = () => end(false);
    by.onclick = () => end(true);
    $('#stage').append(box);
    by.focus();
  });
}

async function walletAsk() {
  if (!account) throw new Error('wallet off: sign in first: wallet access is for a player who is signed in');
  if (walletOn) return { wallet: true, status: 'already' };
  if (asking) return asking;                       // eine Karte zur Zeit; jede Frage bekommt dieselbe Antwort
  asking = (async () => {
    const name = appInfo.name || 'Diese App';
    const yes = await card(`${name} möchte deine Wallet sehen`, [
      `${name}${appInfo.by ? ` von @${appInfo.by}` : ''} möchte wissen, wer spielt: deine Adresse${account.tag ? ` und @${account.tag}` : ''}, dein Guthaben (Pepecoin-Testnet) und wie viele Inschriften du besitzt.`,
      'Ausgeben kann die App damit nichts. Du kannst die Zustimmung oben in der Leiste jederzeit zurücknehmen.',
    ], 'Ja, zeigen', 'Nein');
    if (!yes) return { wallet: false, status: 'declined' };
    await post(`/api/access/${id}`, { wallet: true });
    walletOn = true; showWallet();
    return { wallet: true, status: 'allowed' };
  })();
  try { return await asking; } finally { asking = null; }
}

// ------------------------------------------------------------------ Senden (POST /r/send der App, sendreq.rs)
// Die App stellt nur eine Bitte. Dieses Fenster fragt die Box nach offenen Bitten seiner Seite, laesst die
// UNSIGNIERTE Transaktion bauen und zeigt sie. Nur nach dem Ja entsperrt der Browser den Tresor mit dem Passwort,
// signiert hier (crypto.js signTx) und die Box sendet genau das. Die Schluessel verlassen den Browser nie.
let acct = null, sendBusy = false, C = null;
const pep = (s) => (s / 1e8).toFixed(8).replace(/0+$/, '').replace(/\.$/, '');

async function unlockWith(pw) {
  if (acct) return acct;
  C = C || await import('../crypto.js?v=5');
  const me = await get('/api/me');
  if (!me.tag) throw new Error('Dieses Konto hat noch keinen @name: bitte auf der Arcade-Seite anmelden.');
  const w = await get(`/signin/${encodeURIComponent(me.tag)}`);
  const a = C.fullAccount(await C.arcadeOpen(w.blob, pw));
  if (a.address !== me.address) throw new Error('Dieser Tresor gehört nicht zu diesem Konto.');
  acct = a;
  return acct;
}

function sendCard(r, offer) {
  return new Promise((done) => {
    const name = (appInfo && appInfo.name) || 'Diese App';
    const tx = offer && offer.tx;
    const inSum = tx ? tx.inputs.reduce((s, i) => s + i.amount, 0) : 0;
    const outSum = tx ? tx.outputs.reduce((s, o) => s + o.value, 0) : 0;
    const own = tx ? tx.outputs.filter((o) => o.script === tx.inputs[0].script).reduce((s, o) => s + o.value, 0) : 0;
    const fee = inSum - outSum, total = inSum - own;
    const box = document.createElement('div'); box.className = 'ask';
    const c = document.createElement('div'); c.className = 'card';
    const add = (tag, text, cls) => { const e = document.createElement(tag); e.textContent = text; if (cls) e.className = cls; c.append(e); return e; };
    add('h2', `${name} möchte ${r.kind === 'inscription' ? 'eine Inschrift' : 'Coins'} senden`);
    add('p', `An: ${r.to_tag ? `@${r.to_tag} (${r.to_address})` : r.to_address}`);
    add('p', r.kind === 'inscription' ? `Inschrift: ${r.name || r.inscription}` : `Betrag: ${pep(r.sats)} PEP (Testnet)`);
    if (r.label || r.note) add('p', `Die App sagt: ${[r.label, r.note].filter(Boolean).join(' – ')}`);
    if (tx) add('p', `Gebühr: ${pep(fee)} PEP · zusammen ${pep(total)} PEP von deinem Guthaben`);
    const why = add('p', offer ? '' : 'Die Box konnte die Transaktion nicht bauen.');
    let pw = null;
    if (offer && !acct) {
      pw = document.createElement('input');
      pw.type = 'password'; pw.placeholder = 'Dein Passwort zum Signieren'; pw.autocomplete = 'current-password';
      pw.className = 'pw';
      c.append(pw);
    }
    const row = document.createElement('div'); row.className = 'row';
    const bn = document.createElement('button'); bn.type = 'button'; bn.textContent = 'Nein';
    const by = document.createElement('button'); by.type = 'button'; by.className = 'yes'; by.textContent = 'Ja, senden';
    row.append(bn); if (offer) row.append(by);
    c.append(row); box.append(c);
    const end = (v) => { box.remove(); done(v); };
    bn.onclick = () => end(false);
    const go = async () => {
      by.disabled = bn.disabled = true;
      try {
        why.textContent = 'Signiere …';
        const a = await unlockWith(pw ? pw.value : '');
        const hex = C.signTx(offer.tx, a);
        why.textContent = 'Sende …';
        await post(`/api/sendreqs/${r.id}/send`, { offer: offer.offer, hex });
        end(true);
      } catch (e) {
        why.textContent = String(e.message || e);
        by.disabled = bn.disabled = false;
      }
    };
    by.onclick = go;
    if (pw) pw.onkeydown = (e) => { if (e.key === 'Enter') go(); };
    $('#stage').append(box);
    (pw || by).focus();
    if (!offer) why.textContent = `Geht nicht: ${r.buildError}`;
  });
}

async function sendRequests() {
  if (!account || !walletOn || sendBusy) return;
  let list;
  try { list = (await get(`/api/sendreqs?card=${id}`)).requests || []; } catch { return; }
  if (!list.length) return;
  sendBusy = true;
  try {
    for (const r of list) {
      let offer = null;
      try { offer = await post(`/api/sendreqs/${r.id}/build`, {}); } catch (e) { r.buildError = e.message; }
      const sent = await sendCard(r, offer);
      if (!sent) await post(`/api/sendreqs/${r.id}/deny`, {}).catch(() => {});
    }
  } finally { sendBusy = false; }
}

async function answer(m) {
  if (m.arcade === 'storage') return storage(m);
  if (m.arcade === 'app') return { state: 'open', timeout: null, closesAt: null, inShell: false };
  if (m.arcade === 'realtime') return realtime(m);
  if (m.arcade === 'access') {
    if (m.op === 'status') return { wallet: walletOn, signedIn: !!account, node: location.host, game: true, name: appInfo ? String(appInfo.name || '') : '' };
    if (m.op === 'wallet') return walletAsk();
    if (m.op === 'nodes') return { nodes: [] };
    throw new Error(m.op === 'move' ? 'moving to another arcade is not available on this arcade yet' : 'access: op is status, wallet, nodes or move');
  }
  throw new Error('not answered by this arcade yet');
}

window.addEventListener('message', async (e) => {
  if (!frame || e.source !== frame.contentWindow) return;          // nur der eigene Rahmen
  const m = e.data;
  if (!m || typeof m !== 'object' || typeof m.arcade !== 'string' || typeof m.seq !== 'number') return;
  // schnelle Spur: viele je Sekunde, nie beantwortet
  if (m.arcade === 'realtime' && m.op === 'fast') { const s = seats[String(m.room || '')]; if (s) fastSend(s, m); return; }
  let reply;
  try { reply = { ...(await answer(m)), ok: true }; } catch (err) { reply = { ok: false, error: String(err.message || err) }; }
  // die Seite hat keinen Ursprung (Sandbox): '*' ist der einzige Weg, und nur dieser Rahmen bekommt es
  frame.contentWindow.postMessage({ ...reply, arcade: m.arcade, seq: m.seq }, '*');
});

async function start() {
  let apps;
  try { apps = (await get('/api/apps')).apps || []; } catch (e) { $('#note').textContent = `Die Arcade antwortet nicht (${e.message}).`; return; }
  const app = apps.find((a) => a.card === id);
  if (!app) { $('#note').textContent = 'Diese App ist auf dieser Arcade nicht freigeschaltet.'; return; }
  try { account = await get('/api/me'); } catch { account = null; }
  page = app.page;
  document.title = `${app.name} · LiteWorldsQuest ArcadeOS`;
  $('#name').textContent = app.name;
  $('#by').textContent = app.by ? `von @${app.by}` : '';
  $('#who').textContent = account ? (account.tag ? `@${account.tag}` : 'angemeldet') : 'Gast: Spielstände nur in diesem Browser, Wallet aus';
  appInfo = app;
  // Angemeldet: Zustimmung lesen und das Betrachter-Ticket fuer diese Seite holen (die Seite sieht damit nur etwas,
  // wenn der Nutzer zustimmt: /r/wallet prueft das jedes Mal)
  let ticket = '';
  if (account) {
    try { walletOn = !!(await get(`/api/access/${id}`)).wallet; } catch { walletOn = false; }
    try { ticket = (await post('/api/viewer', { card: id })).ticket || ''; } catch { ticket = ''; }
    showWallet();
    setInterval(sendRequests, 2500);               // Bitten der App zu senden (nur mit Wallet-Zugriff)
    $('#revoke').onclick = async () => {
      try { await post(`/api/access/${id}`, { wallet: false }); walletOn = false; showWallet(); }
      catch (e) { alert(`Zurücknehmen ging nicht: ${e.message}`); }
    };
  }
  frame = document.createElement('iframe');
  frame.setAttribute('sandbox', 'allow-scripts allow-pointer-lock');
  frame.setAttribute('allow', 'fullscreen');
  frame.src = `/content/${page}` + (ticket ? `?v=${encodeURIComponent(ticket)}` : '');
  $('#stage').replaceChildren(frame);
}
start();
