// Atlantis · Arcade (ArcadeOS). Konten wie in den Arcades: 12 Woerter, im Browser mit dem Passwort verschluesselt;
// die Box bewahrt nur dieses verschluesselte Wallet unter dem @name auf und kann es nicht oeffnen. Anmelden mit
// @name + Passwort, auch fuer Konten anderer Arcades (die Box holt das Wallet dort, wenn sie es nicht hat).
import * as C from './crypto.js';

const $ = (s) => document.querySelector(s);
const view = $('#view');
let acct = null;      // entsperrtes Konto (nur in diesem Tab): {words, priv, pub, address, login, msg}
let timer = null;
const draft = { to: '', tx: '' };   // halb geschriebene Nachricht ueberlebt das Auffrischen
let inbox = { next: 0, mine: [], keys: {}, seen: new Set() };   // entschluesselte Nachrichten (nur in diesem Tab)
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
    <div class="row"><button id="go">Anmelden</button></div>
    <div class="row"><button class="link" id="forgot">Passwort vergessen?</button></div></div>`;
  $('#forgot').onclick = () => recoverPane(clean($('#tg').value));
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

function recoverPane(tag0 = '') {
  $('#pane').innerHTML = `<div class="box"><h2>Neues Passwort</h2>
    <p class="muted">Mit deinen 12 Wörtern setzt du hier ein neues Passwort. Die Wörter bleiben in deinem Browser.</p>
    <label for="tg">@name</label><div class="at"><input id="tg" autocomplete="username" autocapitalize="none" spellcheck="false" value="${esc(tag0)}"></div>
    <label for="wd">Deine 12 Wörter</label><textarea id="wd" rows="3" autocomplete="off" autocapitalize="none" spellcheck="false"></textarea>
    <label for="pw">Neues Passwort (mindestens ${C.MIN_PASSWORD} Zeichen)</label><input id="pw" type="password" autocomplete="new-password">
    <label for="pw2">Neues Passwort wiederholen</label><input id="pw2" type="password" autocomplete="new-password">
    <div class="row"><button id="go">Passwort setzen</button><button class="alt" id="back">Zurück</button></div></div>`;
  $('#back').onclick = () => start('in');
  $('#go').onclick = async () => {
    const box = $('#go').closest('.box'), tag = clean($('#tg').value), pw = $('#pw').value;
    const w = C.normaliseWords($('#wd').value);
    if (!tag) return msg(box, 'Bitte deinen @name eingeben.');
    if (!C.validWords(w)) return msg(box, 'Die 12 Wörter stimmen nicht (Rechtschreibung oder Reihenfolge).');
    if ([...pw].length < C.MIN_PASSWORD) return msg(box, `Das Passwort braucht mindestens ${C.MIN_PASSWORD} Zeichen.`);
    if (pw !== $('#pw2').value) return msg(box, 'Die Passwörter sind nicht gleich.');
    busy($('#go'), true);
    try {
      msg(box, 'Verschlüssle dein Wallet neu (ein paar Sekunden) …', 'wait');
      const a = C.fullAccount(w);
      const blob = await C.arcadeSeal(a.words, pw);
      if (await C.arcadeOpen(blob, pw) !== a.words) throw new Error('Verschlüsseln fehlgeschlagen.');
      const ch = await api('/api/challenge');
      await api('/api/recover', { tag, pubkey: a.login.pubkey, address: a.address, coinpub: a.pub, blob, nonce: ch.nonce,
        sig: C.signLogin(a.login.secret, ch.message), coinsig: C.signMessage(a.priv, ch.message) });
      acct = a;
      home();
    } catch (e) { msg(box, e.message); busy($('#go'), false); }
  };
  (tag0 ? $('#wd') : $('#tg')).focus();
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
    <div class="box" id="msgbox"></div>
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
  $('#out').onclick = async () => { await api('/api/logout', {}).catch(() => {}); acct = null; forgetKeys(); inbox = { next: 0, mine: [], keys: {}, seen: new Set() }; start(); };
  messenger(me, bal);
  timer = setInterval(() => { if (!document.hidden && !document.querySelector('#msgbox textarea:focus, #msgbox input:focus')) home(); }, 30000);
}

// Nach dem Neuladen sind die Schluessel weg (nur die Sitzung bleibt): einmal mit dem Passwort entsperren
async function unlock(box, why = 'Zum Signieren brauche ich dein Passwort noch einmal:') {
  if (acct) return true;
  const pw = prompt(why);
  if (!pw) return false;
  try {
    msg(box, 'Entsperre …', 'wait');
    const me = await api('/api/me');
    const w = await api(`/signin/${encodeURIComponent(me.tag)}`);
    acct = C.fullAccount(await C.arcadeOpen(w.blob, pw));
    msg(box, '', 'muted');
    return true;
  } catch (e) { msg(box, e.message); return false; }
}

// Angebot der Box pruefen lassen, signieren, senden
async function signAndSend(box, offer, what) {
  const own = offer.tx.outputs.at(-1);
  const spent = offer.tx.inputs.reduce((a, i) => a + i.amount, 0) - (own && own.script === offer.tx.inputs[0].script ? own.value : 0);
  if (!confirm(`${what}\nKosten: ${pep(spent)} PEP (Testnet)`)) { msg(box, 'Abgebrochen.', 'muted'); return null; }
  const hex = C.signTx(offer.tx, acct);
  return api('/api/send', { offer: offer.offer, hex });
}

async function claim(tag, btn) {
  const box = btn.closest('.box');
  if (!await unlock(box)) return;
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

// ------------------------------------------------------------------ Messenger
// Nachrichten liegen verschluesselt auf der Pepecoin-Testnet-Chain. Der Browser probiert jede, ob sie an dich geht;
// die Box sieht nie, was drinsteht oder an wen. Gesendetes kann man selbst nicht mehr entschluesseln - eine Kopie
// bleibt deshalb hier im Browser.
const sentKey = (a) => `arcade-sent:${a}`;
// Nur der Messenger-Schluessel wird im Browser gemerkt (Lesen ohne Passwort). Coins ausgeben braucht weiter das Passwort.
const msgKeyName = (a) => `arcade-msgkey:${a}`;
function msgKey(me) {
  if (acct) {
    try { localStorage.setItem(msgKeyName(me.address), JSON.stringify({ secret: C.bytesToHex(acct.msg.secret), pubkey: acct.msg.pubkey })); } catch { /* egal */ }
    return acct.msg;
  }
  try {
    const k = JSON.parse(localStorage.getItem(msgKeyName(me.address)) || 'null');
    return k && { secret: C.hexToBytes(k.secret), pubkey: k.pubkey };
  } catch { return null; }
}
function forgetKeys() { try { Object.keys(localStorage).filter((k) => k.startsWith('arcade-msgkey:')).forEach((k) => localStorage.removeItem(k)); } catch { /* egal */ } }
const sentList = (a) => { try { return JSON.parse(localStorage.getItem(sentKey(a)) || '[]'); } catch { return []; } };
const who = (pub) => { const k = inbox.keys[pub]; return k ? (k.tag ? `@${k.tag}` : (k.name || `${k.address.slice(0, 10)}…`)) : `unbekannt (${pub.slice(0, 8)})`; };

async function messenger(me, bal) {
  const box = $('#msgbox');
  if (!box) return;
  const mk = msgKey(me);
  if (!mk) {
    box.innerHTML = `<h2>Nachrichten</h2><p class="muted">Zum Lesen brauchst du deinen Schlüssel – der bleibt in deinem Browser.</p><div class="row"><button id="un">Nachrichten öffnen</button></div>`;
    $('#un').onclick = async () => { if (await unlock(box, 'Passwort, um deine Nachrichten zu öffnen:')) messenger(me, bal); };
    return;
  }
  const mine = mk.pubkey, on = me.msgkey && me.msgkey.pubkey === mine;
  let head = '';
  if (!on) {
    head = me.msgkey
      ? `<p class="msg wait">Für deine Adresse ist ein anderer Messenger-Schlüssel veröffentlicht (z. B. aus einer anderen Arcade). Nachrichten an den alten kann dieser Browser nicht lesen.</p>`
      : `<p class="muted">Damit man dir schreiben kann, veröffentlichst du einmal deinen Messenger-Schlüssel auf der Chain.</p>`;
    head += `<div class="row"><button id="ann" ${bal > 0 ? '' : 'disabled'}>${me.msgkey ? 'Meinen Schlüssel neu veröffentlichen' : 'Messenger einschalten'}</button></div>`;
  }
  box.innerHTML = `<h2>Nachrichten</h2>${head}
    <label for="to">An</label><div class="at"><input id="to" autocapitalize="none" spellcheck="false" placeholder="name"></div>
    <label for="tx">Nachricht</label><textarea id="tx" rows="3" maxlength="7000"></textarea>
    <div class="row"><button id="snd" ${bal > 0 ? '' : 'disabled'}>Senden</button></div>
    <div id="list"><p class="muted">Suche deine Nachrichten …</p></div>`;
  $('#to').value = draft.to; $('#tx').value = draft.tx;
  $('#to').oninput = () => { draft.to = $('#to').value; }; $('#tx').oninput = () => { draft.tx = $('#tx').value; };
  if ($('#ann')) $('#ann').onclick = async () => {
    try {
      if (!await unlock(box)) return;
      msg(box, 'Baue die Transaktion …', 'wait');
      const offer = await api('/api/announce', { pubkey: mine });
      if (await signAndSend(box, offer, 'Messenger-Schlüssel veröffentlichen?')) msg(box, 'Gesendet – ab dem nächsten Block kann man dir schreiben.', 'ok');
    } catch (e) { msg(box, e.message); }
  };
  $('#snd').onclick = async () => {
    const to = clean($('#to').value), text = $('#tx').value.trim();
    if (!to || !text) return msg(box, 'Bitte Empfänger und Text eingeben.');
    try {
      msg(box, `Suche den Schlüssel von @${to} …`, 'wait');
      const k = await api(`/api/key?to=${encodeURIComponent(to)}`);
      if (k.error) return msg(box, k.error);
      if (!k.pubkey) return msg(box, `@${to} hat noch keinen Messenger-Schlüssel veröffentlicht.`);
      if (!await unlock(box, 'Zum Senden brauche ich dein Passwort (es signiert die Transaktion):')) return;
      const payload = C.arcmSeal(acct.msg, k.pubkey, text);
      const offer = await api('/api/message', { payload, coinpub: acct.pub });
      const r = await signAndSend(box, offer, `Nachricht an @${k.tag || to} senden?`);
      if (!r) return;
      const list = sentList(me.address); list.push({ to: k.tag || to, text, txid: r.txid, at: Date.now() });
      try { localStorage.setItem(sentKey(me.address), JSON.stringify(list.slice(-200))); } catch { /* egal */ }
      $('#tx').value = ''; draft.tx = '';
      msg(box, 'Gesendet – kommt mit dem nächsten Block an.', 'ok');
      showList(me);
    } catch (e) { msg(box, e.message); }
  };
  try {
    const r = await api(`/api/msgs?from=${inbox.next}`);
    for (const k of r.keys) inbox.keys[k.pubkey] = k;
    for (const it of r.items) {
      if (inbox.seen.has(it.txid)) continue;
      inbox.seen.add(it.txid);
      const m = C.arcmOpen(mk, it.p);
      if (m && !m.forged) inbox.mine.push({ ...m, h: it.h, txid: it.txid });
    }
    inbox.next = r.next;
  } catch (e) { $('#list').innerHTML = `<p class="msg err">${esc(e.message)}</p>`; return; }
  showList(me);
}

// Einladungen in Gruppen (dogecoin-arcade group.py) kommen als "arcade-group/1" + JSON mit dem Gruppen-Geheimnis:
// nicht roh zeigen, nur wer einlaedt und wohin. Gruppen selbst kommen spaeter.
function shown(text) {
  const m = /^arcade-group\/1\s*\n([\s\S]*)$/.exec(text || '');
  if (!m) return text;
  try {
    const g = JSON.parse(m[1]);
    return `Einladung in die Gruppe „${String(g.name || '?').slice(0, 60)}“ (${(g.members || []).length} Mitglieder) – Gruppen kann diese Arcade noch nicht öffnen.`;
  } catch { return 'Einladung in eine Gruppe (unlesbar).'; }
}

function showList(me) {
  const el = $('#list');
  if (!el) return;
  const rows = [
    ...inbox.mine.map((m) => ({ in: true, who: who(m.sender), text: m.text, file: m.file, when: `Block ${m.h}`, sort: m.h * 1e13 })),
    ...sentList(me.address).map((m) => ({ in: false, who: `@${m.to}`, text: m.text, when: new Date(m.at).toLocaleString('de-DE'), sort: m.at })),
  ].sort((a, b) => b.sort - a.sort);
  if (!rows.length) { el.innerHTML = '<p class="muted">Noch keine Nachrichten.</p>'; return; }
  el.innerHTML = rows.map((r) => `<div class="m ${r.in ? 'in' : 'out'}"><div class="mh"><b>${r.in ? 'von' : 'an'} ${esc(r.who)}</b><span>${esc(r.when)}</span></div>
    <div class="mt">${esc(shown(r.text))}${r.file ? `<p class="muted">[Anhang: ${esc(r.file.name)} – noch nicht unterstützt]</p>` : ''}</div>
    ${r.in && r.who.startsWith('@') ? `<button class="link rp" data-to="${esc(r.who.slice(1))}">Antworten</button>` : ''}</div>`).join('');
  el.querySelectorAll('.rp').forEach((b) => { b.onclick = () => { $('#to').value = draft.to = b.dataset.to; $('#tx').focus(); }; });
}
