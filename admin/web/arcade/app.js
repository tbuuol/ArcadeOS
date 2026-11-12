// Atlantis · Arcade (ArcadeOS). Konten wie in den Arcades: 12 Woerter, im Browser mit dem Passwort verschluesselt;
// die Box bewahrt nur dieses verschluesselte Wallet unter dem @name auf und kann es nicht oeffnen. Anmelden mit
// @name + Passwort, auch fuer Konten anderer Arcades (die Box holt das Wallet dort, wenn sie es nicht hat).
import * as C from './crypto.js';

const $ = (s) => document.querySelector(s);
const view = $('#view');
let acct = null;      // entsperrtes Konto (nur in diesem Tab): {words, priv, pub, address, login, msg}
let timer = null;
const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const pep = (s) => (Number(s || 0) / 1e8).toLocaleString('de-DE', { maximumFractionDigits: 8 });
const clean = (t) => String(t || '').trim().replace(/^@/, '').toLowerCase();

async function api(path, body) {
  const r = await fetch(path, body === undefined ? { cache: 'no-store', credentials: 'same-origin' }
    : { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  const j = await r.json().catch(() => ({ error: `HTTP ${r.status}` }));
  if (!r.ok) { const e = new Error(j.error || `HTTP ${r.status}`); e.status = r.status; throw e; }
  return j;
}
function msg(el, text, cls = 'err') { const m = el.querySelector('.msg') || el.appendChild(document.createElement('p')); m.className = `msg ${cls}`; m.textContent = text; }
function busy(btn, on) { btn.disabled = on; }

// ------------------------------------------------------------------ Anmelden / neues Konto
function start(tab = 'in') {
  clearInterval(timer);
  view.innerHTML = `<div class="tabs"><button data-t="in" class="${tab === 'in' ? 'on' : ''}">Anmelden</button><button data-t="new" class="${tab === 'new' ? 'on' : ''}">Neues Konto</button></div><div id="pane"></div>`;
  view.querySelectorAll('.tabs button').forEach((b) => { b.onclick = () => start(b.dataset.t); });
  tab === 'in' ? signinPane() : signupPane();
}

function signinPane() {
  $('#pane').innerHTML = `<div class="box"><h2>Anmelden</h2>
    <p class="muted">Mit deinem @name und Passwort – auch wenn du dein Konto in einer anderen Arcade angelegt hast.</p>
    <label for="tg">@name</label><div class="at"><input id="tg" autocomplete="username" autocapitalize="none" spellcheck="false"></div>
    <label for="pw">Passwort</label><input id="pw" type="password" autocomplete="current-password">
    <div class="row"><button id="go">Anmelden</button></div></div>`;
  const go = async () => {
    const box = $('#go').closest('.box'), tag = clean($('#tg').value), pw = $('#pw').value;
    if (!tag || !pw) return msg(box, 'Bitte @name und Passwort eingeben.');
    busy($('#go'), true);
    try {
      msg(box, 'Suche dein Konto …', 'wait');
      const w = await api(`/signin/${encodeURIComponent(tag)}`);
      msg(box, 'Entsperre (das dauert ein paar Sekunden) …', 'wait');
      const words = await C.arcadeOpen(w.blob, pw);
      const a = C.fullAccount(words);
      if (a.login.pubkey !== w.pubkey) throw new Error('Dieses Wallet passt nicht zu dem Konto.');
      const ch = await api('/api/challenge');
      await api('/api/login', { pubkey: a.login.pubkey, nonce: ch.nonce, sig: C.signLogin(a.login.secret, ch.message), tag });
      acct = a;
      home();
    } catch (e) { msg(box, e.status === 404 ? `@${tag}: ${e.message}` : e.message); busy($('#go'), false); }
  };
  $('#go').onclick = go; $('#pw').onkeydown = (e) => { if (e.key === 'Enter') go(); };
  $('#tg').focus();
}

function signupPane() {
  $('#pane').innerHTML = `<div class="box"><h2>Neues Konto</h2>
    <p class="muted">Du bekommst 12 Wörter – sie <b>sind</b> dein Konto, in jeder Arcade. Mit dem Passwort meldest du dich hier an.</p>
    <label for="tg">Dein @name</label><div class="at"><input id="tg" autocomplete="username" autocapitalize="none" spellcheck="false" placeholder="2–24 Zeichen: a–z, 0–9, _"></div>
    <p class="msg muted" id="tm"></p>
    <label for="pw">Passwort (mindestens ${C.MIN_PASSWORD} Zeichen – ein paar Wörter hintereinander sind gut)</label><input id="pw" type="password" autocomplete="new-password">
    <label for="pw2">Passwort wiederholen</label><input id="pw2" type="password" autocomplete="new-password">
    <div class="row"><button id="go" disabled>Weiter</button></div></div>`;
  let free = false, t = null;
  $('#tg').oninput = () => {
    clearTimeout(t); free = false; $('#go').disabled = true;
    const q = clean($('#tg').value); if (!q) { $('#tm').textContent = ''; return; }
    t = setTimeout(async () => {
      try {
        const r = await api(`/api/name?tag=${encodeURIComponent(q)}`);
        free = !!r.ok;
        $('#tm').className = `msg ${r.ok ? 'ok' : 'err'}`;
        $('#tm').textContent = r.tag ? `@${r.tag}: ${r.reason}` : r.reason;
        $('#go').disabled = !free;
      } catch (e) { $('#tm').textContent = e.message; }
    }, 350);
  };
  $('#go').onclick = () => {
    const box = $('#go').closest('.box'), pw = $('#pw').value;
    if ([...pw].length < C.MIN_PASSWORD) return msg(box, `Das Passwort braucht mindestens ${C.MIN_PASSWORD} Zeichen.`);
    if (pw !== $('#pw2').value) return msg(box, 'Die Passwörter sind nicht gleich.');
    words(clean($('#tg').value), pw, C.newWords());
  };
}

function words(tag, pw, w) {
  const list = w.split(' '), ask = [1, 5, 9];
  view.innerHTML = `<div class="box warnbox"><h2>Schreib diese 12 Wörter auf</h2>
    <p>Auf Papier. Mit ihnen kommst du in jeder Arcade an dein Konto, auch wenn du das Passwort vergisst. Wer sie hat, hat dein Konto.</p>
    <div class="words">${list.map((x) => `<span>${esc(x)}</span>`).join('')}</div></div>
    <div class="box"><h2>Kontrolle</h2>${ask.map((i) => `<label>Wort ${i + 1}</label><input data-i="${i}" autocomplete="off" autocapitalize="none" spellcheck="false">`).join('')}
    <div class="row"><button id="ok">Konto anlegen</button><button class="alt" id="back">Zurück</button></div></div>`;
  $('#back').onclick = () => start('new');
  $('#ok').onclick = async () => {
    const box = $('#ok').closest('.box');
    for (const inp of view.querySelectorAll('input[data-i]')) if (inp.value.trim().toLowerCase() !== list[+inp.dataset.i]) return msg(box, `Wort ${+inp.dataset.i + 1} stimmt nicht.`);
    busy($('#ok'), true);
    try {
      msg(box, 'Verschlüssle dein Wallet (ein paar Sekunden) …', 'wait');
      const a = C.fullAccount(w);
      const blob = await C.arcadeSeal(w, pw);
      if (await C.arcadeOpen(blob, pw) !== a.words) throw new Error('Verschlüsseln fehlgeschlagen.');
      const ch = await api('/api/challenge');
      await api('/api/signup', { tag, pubkey: a.login.pubkey, address: a.address, coinpub: a.pub, blob,
        nonce: ch.nonce, sig: C.signLogin(a.login.secret, ch.message) });
      acct = a;
      home();
    } catch (e) { msg(box, e.message); busy($('#ok'), false); }
  };
}

// ------------------------------------------------------------------ Mein Konto
async function home() {
  clearInterval(timer);
  let me;
  try { me = await api('/api/me'); } catch (e) { if (e.status === 401) return start(); view.innerHTML = `<div class="box"><p class="msg err">${esc(e.message)}</p></div>`; return; }
  const held = me.held, want = me.tag, bal = me.confirmed + me.unconfirmed;
  const nameLine = held ? `<p class="name">@${esc(held)}</p>`
    : me.pending ? `<p class="name want">@${esc(me.pending.tag)}</p><p class="msg wait">Wird gerade beansprucht – wartet auf den nächsten Block …</p>`
    : `<p class="name want">@${esc(want)}</p><p class="muted">Noch nicht auf der Chain beansprucht.</p>`;
  view.innerHTML = `<div class="box">${nameLine}
      <div class="kv" style="margin-top:10px"><b>Guthaben</b><span>${pep(me.confirmed)} PEP${me.unconfirmed ? ` (+${pep(me.unconfirmed)} unterwegs)` : ''}</span>
      <b>Adresse</b><span class="addr">${esc(me.address)}</span></div>
      <div class="row" id="acts"></div></div>
    <div class="row"><button class="link" id="out">Abmelden</button></div>`;
  const acts = $('#acts');
  if (!me.faucet && bal === 0) {
    const b = document.createElement('button'); b.textContent = `Startguthaben holen (${me.faucet_amount} PEP)`; acts.appendChild(b);
    b.onclick = async () => { busy(b, true); try { await api('/api/faucet', {}); msg(acts.parentNode, 'Unterwegs – kommt mit dem nächsten Block.', 'ok'); setTimeout(home, 3000); } catch (e) { msg(acts.parentNode, e.message); busy(b, false); } };
  }
  if (!held && !me.pending && bal > 0) {
    const b = document.createElement('button'); b.textContent = `@${want} beanspruchen`; acts.appendChild(b);
    b.onclick = () => claim(want, b);
  }
  $('#out').onclick = async () => { await api('/api/logout', {}).catch(() => {}); acct = null; start(); };
  timer = setInterval(() => { if (!document.hidden) home(); }, 20000);
}

async function claim(tag, btn) {
  const box = btn.closest('.box');
  if (!acct) {   // nach dem Neuladen: Schluessel nur nach erneutem Entsperren
    const pw = prompt('Zum Signieren brauche ich dein Passwort noch einmal:');
    if (!pw) return;
    try {
      msg(box, 'Entsperre …', 'wait');
      const me = await api('/api/me');
      const w = await api(`/signin/${encodeURIComponent(me.tag)}`);
      acct = C.fullAccount(await C.arcadeOpen(w.blob, pw));
    } catch (e) { return msg(box, e.message); }
  }
  busy(btn, true);
  try {
    msg(box, 'Baue die Transaktion …', 'wait');
    const offer = await api('/api/claim', { tag });
    if (!confirm(`@${offer.tag} beanspruchen?\nGebühr: ${pep(offer.tx.fee)} PEP (Testnet)`)) { busy(btn, false); return msg(box, 'Abgebrochen.', 'muted'); }
    const hex = C.signTx(offer.tx, acct);
    await api('/api/send', { offer: offer.offer, hex });
    home();
  } catch (e) { msg(box, e.message); busy(btn, false); }
}

// Beim Laden: angemeldet (Sitzung) -> Konto, sonst Anmelden
api('/api/me').then(() => home()).catch(() => start());
