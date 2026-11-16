// App-Fenster (LiteWorldsQuest ArcadeOS): oeffnet eine eingeschriebene App, die der Betreiber freigeschaltet hat, in
// einer Sandbox (eigener leerer Ursprung: kein Zugriff auf diese Seite, ihre Cookies oder den Browser-Speicher mit
// den Woertern) und beantwortet ihre Fragen - nur die aus genau diesem Rahmen:
//   storage  arcade.storage: angemeldet auf der Box (je Konto und Seite), als Gast nur in diesem Browser
//   app      arcade.app: Zustand (offen)
//   access   arcade.access: Status; Wallet-Zugriff gibt es hier noch nicht
//   realtime arcade.realtime: Raeume mit Spielern aller Arcades (als Gast; ohne schnelle Spur und Medien)
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
const seats = {};   // Raumname der Seite -> {token, live}
function tell(ev) { if (frame) frame.contentWindow.postMessage({ arcade: 'realtime', event: ev }, '*'); }
async function pollSeat(name, s) {
  while (s.live) {
    try {
      const r = await get(`/rt/poll?token=${s.token}&wait=25`);
      for (const ev of r.events || []) tell({ ...ev, room: name });
    } catch (e) {
      if (e.status === 404) { s.live = false; tell({ room: name, type: 'closed', why: 'the arcade closed this seat' }); return; }
      await new Promise((ok) => setTimeout(ok, 3000));
    }
  }
}
async function realtime(m) {
  const name = String(m.room || '');
  if (m.op === 'join') {
    if (seats[name]) return { online: true, me: seats[name].me, members: [], fast: false, media: false };
    const r = await post('/rt/join', { page, room: name });
    if (!r.online) return { online: false, why: r.why || 'offline', fast: false, media: false };
    const s = seats[name] = { token: r.token, me: r.me, live: true };
    pollSeat(name, s);
    return { online: true, me: r.me, members: r.members || [], fast: false, media: false };
  }
  const s = seats[name];
  if (m.op === 'send') { if (!s) throw new Error('join the room first'); await post('/rt/send', { token: s.token, data: String(m.data || '') }); return {}; }
  if (m.op === 'leave') { if (s) { s.live = false; delete seats[name]; post('/rt/leave', { token: s.token }).catch(() => {}); } return {}; }
  if (m.op === 'fast') return {};   // keine schnelle Spur hier: still verworfen
  throw new Error('not available on this arcade yet');
}
window.addEventListener('pagehide', () => {
  for (const s of Object.values(seats)) {
    s.live = false;
    try { navigator.sendBeacon('/rt/leave', new Blob([JSON.stringify({ token: s.token })], { type: 'application/json' })); } catch {}
  }
});

async function answer(m) {
  if (m.arcade === 'storage') return storage(m);
  if (m.arcade === 'app') return { state: 'open', timeout: null, closesAt: null, inShell: false };
  if (m.arcade === 'realtime') return realtime(m);
  if (m.arcade === 'access') {
    if (m.op === 'status') return { wallet: false, signedIn: !!account };
    if (m.op === 'nodes') return { nodes: [] };
    throw new Error(m.op === 'wallet' ? 'wallet access is not available on this arcade yet' : 'not available on this arcade yet');
  }
  throw new Error('not answered by this arcade yet');
}

window.addEventListener('message', async (e) => {
  if (!frame || e.source !== frame.contentWindow) return;          // nur der eigene Rahmen
  const m = e.data;
  if (!m || typeof m !== 'object' || typeof m.arcade !== 'string' || typeof m.seq !== 'number') return;
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
  $('#who').textContent = account ? (account.tag ? `@${account.tag}` : 'angemeldet') : 'Gast: Spielstände nur in diesem Browser';
  frame = document.createElement('iframe');
  frame.setAttribute('sandbox', 'allow-scripts allow-pointer-lock');
  frame.setAttribute('allow', 'fullscreen');
  frame.src = `/content/${page}`;
  $('#stage').replaceChildren(frame);
}
start();
