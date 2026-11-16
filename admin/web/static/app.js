// ArcadeOS · Arcade einrichten. Schluessel und Woerter bleiben in diesem Browser; zur Box gehen nur
// oeffentlicher Schluessel, Adresse und fertige Signaturen. Assistent wie in den Arcades: Konto -> Guthaben -> @name -> fertig.
import * as C from './crypto.js';

const API = 'api';
const VAULT = 'arcadeos-admin-vault';
const $ = (s) => document.querySelector(s);
const view = $('#view');
let S = null;          // letzter Status der Box
let acct = null;       // entsperrtes Konto (nur im Speicher dieses Tabs)
let draft = null;      // neue Woerter waehrend der Einrichtung
let timer = null;
let tab = 'setup';     // Reiter: setup (Arcade), chains, apps

const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const pep = (s) => (Number(s || 0) / 1e8).toLocaleString('de-DE', { maximumFractionDigits: 8 });

async function api(path, body) {
  const r = await fetch(`${API}${path}`, body === undefined ? { cache: 'no-store' }
    : { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  const j = await r.json().catch(() => ({ error: `HTTP ${r.status}` }));
  if (!r.ok) throw new Error(j.error || `HTTP ${r.status}`);
  return j;
}

function steps(now) {
  document.querySelectorAll('#steps li').forEach((li, i) => { li.className = i < now ? 'done' : i === now ? 'now' : ''; });
}
function msg(el, text, cls = 'err') { const m = el.querySelector('.msg') || el.appendChild(document.createElement('p')); m.className = `msg ${cls}`; m.textContent = text; }

// ------------------------------------------------------------------ Woerter verschluesselt in diesem Browser
// (crypto.js: PBKDF2 + AES-GCM ohne WebCrypto, das es ueber http im LAN nicht gibt)
async function seal(words, password, address) {
  const sealed = await C.sealWords(words, password);
  localStorage.setItem(VAULT, JSON.stringify({ ...sealed, address }));
  // gleich wieder oeffnen: nur was sich entschluesseln laesst, gilt als gesichert
  if (await C.openWords(savedVault(), password) !== words) throw new Error('Speichern im Browser fehlgeschlagen.');
}
async function unseal(password) {
  const v = savedVault();
  if (!v) throw new Error('In diesem Browser ist kein Konto gespeichert.');
  return C.openWords(v, password);
}
// Kopieren: navigator.clipboard gibt es nur auf https-Seiten, sonst das alte execCommand
function copy(text) {
  if (navigator.clipboard && window.isSecureContext) return navigator.clipboard.writeText(text);
  const t = document.createElement('textarea'); t.value = text; t.style.position = 'fixed'; t.style.opacity = '0';
  document.body.appendChild(t); t.select();
  const ok = document.execCommand('copy'); t.remove();
  return ok ? Promise.resolve() : Promise.reject(new Error('Kopieren nicht moeglich, bitte von Hand markieren.'));
}
const savedVault = () => { try { return JSON.parse(localStorage.getItem(VAULT) || 'null'); } catch { return null; } };

// ------------------------------------------------------------------ Ablauf
async function refresh() {
  try { S = await api('/status'); } catch (e) { view.innerHTML = `<div class="box"><p class="msg err">Box nicht erreichbar: ${esc(e.message)}</p></div>`; return; }
  $('#foot').textContent = `${S.version} · Knoten ${S.node_id ? S.node_id.slice(0, 12) + '…' : '?'} · Pepecoin-Testnet Block ${S.chain ? S.chain.blocks : '?'}`;
  render();
}

function render() {
  clearInterval(timer);
  // Reiter Chains und Apps nur fuer den angemeldeten Betreiber (Aenderungen signiert sein Konto)
  const me = !!(S.operator && acct && acct.address === S.operator.address);
  $('#tabs').hidden = !me;
  $('#steps').hidden = me && tab !== 'setup';
  document.querySelectorAll('#tabs button').forEach((b) => { b.className = b.dataset.tab === tab ? 'on' : ''; });
  if (me && tab === 'chains') return chainsView();
  if (me && tab === 'apps') return appsView();
  if (!S.operator) return draft ? confirmWords() : start();
  if (!acct) return unlock();
  if (acct.address !== S.operator.address) {
    steps(0);
    view.innerHTML = `<div class="box warnbox"><h2>Nicht der Betreiber</h2><p>Dieses Konto (<span class="addr">${esc(acct.address)}</span>) ist nicht das Admin-Konto dieser Arcade.</p>
      <div class="row"><button class="alt" id="out">Abmelden</button></div></div>`;
    $('#out').onclick = () => { acct = null; render(); };
    return;
  }
  if (S.tag && S.owner) return done();
  if (S.pending && !S.tag) return pending();
  return fundAndName();
}

function start() {
  steps(0);
  view.innerHTML = `<div class="box"><h2>Admin-Konto anlegen</h2>
    <p>Dein Admin-Konto sind <b>12 Wörter</b>. Sie entstehen hier im Browser und verlassen ihn nie. Mit denselben Wörtern ist es auch in jeder Arcade-Wallet dein Konto.</p>
    <label for="nm">Name der Arcade (frei wählbar, z.B. „LiteWorlds Arcade“)</label><input id="nm" maxlength="60" placeholder="optional">
    <div class="row"><button id="new">Neues Konto erzeugen</button><button class="alt" id="have">Ich habe schon 12 Wörter</button></div></div>`;
  $('#new').onclick = () => { draft = { words: C.newWords(), name: $('#nm').value.trim() }; render(); };
  $('#have').onclick = () => restore($('#nm').value.trim());
}

function confirmWords() {
  steps(0);
  const w = draft.words.split(' ');
  const ask = [2, 6, 10];   // Wort 3, 7, 11
  view.innerHTML = `<div class="box warnbox"><h2>Schreib diese 12 Wörter auf</h2>
    <p>Auf Papier, nicht als Foto, nicht in eine Cloud. Wer sie hat, hat dein Admin-Konto – und ohne sie kommst du nicht mehr hinein.</p>
    <div class="words">${w.map((x) => `<span>${esc(x)}</span>`).join('')}</div></div>
    <div class="box"><h2>Kontrolle</h2>${ask.map((i) => `<label>Wort ${i + 1}</label><input data-i="${i}" autocomplete="off" autocapitalize="none" spellcheck="false">`).join('')}
    <label for="pw">Passwort für diesen Browser (mindestens 10 Zeichen; damit meldest du dich hier wieder an)</label><input id="pw" type="password" autocomplete="new-password">
    <div class="row"><button id="ok">Konto anlegen</button><button class="alt" id="back">Zurück</button></div></div>`;
  $('#back').onclick = () => { draft = null; render(); };
  $('#ok').onclick = async () => {
    const box = $('#ok').closest('.box');
    for (const inp of view.querySelectorAll('input[data-i]')) if (inp.value.trim().toLowerCase() !== w[+inp.dataset.i]) return msg(box, `Wort ${+inp.dataset.i + 1} stimmt nicht.`);
    if ($('#pw').value.length < 10) return msg(box, 'Das Passwort braucht mindestens 10 Zeichen.');
    await createOperator(draft.words, $('#pw').value, draft.name, box);
  };
}

function restore(name) {
  steps(0);
  view.innerHTML = `<div class="box"><h2>Mit 12 Wörtern</h2><label for="wd">Deine 12 Wörter</label><textarea id="wd" autocomplete="off" autocapitalize="none" spellcheck="false"></textarea>
    <label for="pw">Passwort für diesen Browser (mindestens 10 Zeichen)</label><input id="pw" type="password" autocomplete="new-password">
    <div class="row"><button id="ok">Weiter</button><button class="alt" id="back">Zurück</button></div></div>`;
  $('#back').onclick = () => render();
  $('#ok').onclick = async () => {
    const box = $('#ok').closest('.box'), words = $('#wd').value;
    if (!C.validWords(words)) return msg(box, 'Diese Wörter ergeben kein gültiges Konto (Rechtschreibung oder Reihenfolge).');
    if ($('#pw').value.length < 10) return msg(box, 'Das Passwort braucht mindestens 10 Zeichen.');
    if (S.operator) {   // Anmelden auf einem neuen Browser
      const a = C.account(words);
      if (a.address !== S.operator.address) return msg(box, 'Das ist nicht das Admin-Konto dieser Arcade.');
      await seal(words.trim().toLowerCase().split(/\s+/).join(' '), $('#pw').value, a.address);
      acct = a; return refresh();
    }
    await createOperator(words, $('#pw').value, name, box);
  };
}

async function createOperator(words, password, name, box) {
  try {
    msg(box, 'Richte ein …', 'wait');
    const a = C.account(words);
    // erst im Browser sichern, dann die Box einrichten: scheitert das Sichern, ist auf der Box nichts passiert
    await seal(words.trim().toLowerCase().split(/\s+/).join(' '), password, a.address);
    const ch = await api('/challenge');
    try {
      await api('/setup', { pubkey: a.pub, address: a.address, nonce: ch.nonce, sig: C.signMessage(a.priv, ch.message), name });
    } catch (e) { localStorage.removeItem(VAULT); throw e; }
    acct = a; draft = null;
    await refresh();
  } catch (e) { msg(box, e.message); }
}

function unlock() {
  steps(0);
  const v = savedVault();
  if (!v || v.address !== S.operator.address) return restore('');
  view.innerHTML = `<div class="box"><h2>Anmelden</h2><p class="muted">Admin-Konto <span class="addr">${esc(S.operator.address)}</span></p>
    <label for="pw">Passwort</label><input id="pw" type="password" autocomplete="current-password">
    <div class="row"><button id="ok">Entsperren</button><button class="alt" id="words">Mit 12 Wörtern</button></div></div>`;
  const go = async () => {
    const box = $('#ok').closest('.box');
    try { acct = C.account(await unseal($('#pw').value)); refresh(); } catch (e) { msg(box, e.message); }
  };
  $('#ok').onclick = go; $('#pw').onkeydown = (e) => { if (e.key === 'Enter') go(); };
  $('#words').onclick = () => restore('');
  $('#pw').focus();
}

function fundAndName() {
  steps(1);
  view.innerHTML = `<div class="box"><h2>Guthaben (Pepecoin-Testnet)</h2>
      <p>Ein @name wird mit einer kleinen Transaktion beansprucht. Schick dafür etwas Testnet-PEP (0,1 reicht) an deine Admin-Adresse:</p>
      <p class="addr" id="ad">${esc(acct.address)}</p><div class="row"><button class="alt" id="cp">Adresse kopieren</button></div>
      <div class="kv" style="margin-top:12px"><b>Bestätigt</b><span id="bc">…</span><b>Unterwegs</b><span id="bu">…</span></div></div>
    <div class="box"><h2>Dein @name</h2>
      <p class="muted">2–24 Zeichen: Kleinbuchstaben, Ziffern, _. Wer zuerst beansprucht, bekommt ihn.</p>
      <label for="tg">@name</label><input id="tg" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="z.B. liteworlds">
      <p class="msg muted" id="tm">${S.names_ready ? '' : 'Der Namens-Index wird noch aus der Chain aufgebaut …'}</p>
      <label for="nm">Name der Arcade</label><input id="nm" maxlength="60" value="${esc(S.name || '')}">
      <div class="row"><button id="claim" disabled>Beanspruchen &amp; veröffentlichen</button></div></div>`;
  $('#cp').onclick = () => copy(acct.address).then(() => { $('#cp').textContent = 'Kopiert'; }, (e) => { $('#cp').textContent = e.message; });
  let bal = 0, free = false, t = null;
  const upd = () => { $('#claim').disabled = !(free && bal > 0); };
  const loadBal = async () => {
    try { const b = await api('/balance'); bal = b.confirmed + b.unconfirmed; $('#bc').textContent = `${pep(b.confirmed)} PEP`; $('#bu').textContent = `${pep(b.unconfirmed)} PEP`; upd(); } catch (e) { $('#bc').textContent = e.message; }
  };
  loadBal(); timer = setInterval(loadBal, 10000);
  $('#tg').oninput = () => {
    clearTimeout(t); free = false; upd();
    const q = $('#tg').value.trim(); if (!q) { $('#tm').textContent = ''; return; }
    t = setTimeout(async () => {
      try {
        const r = await api(`/name?tag=${encodeURIComponent(q)}`);
        free = !!r.ok && !r.mine;
        $('#tm').className = `msg ${r.ok ? 'ok' : 'err'}`;
        $('#tm').textContent = r.tag ? `@${r.tag}: ${r.reason}${r.by && !r.mine ? ` (${r.by})` : ''}` : r.reason;
        upd();
      } catch (e) { $('#tm').textContent = e.message; }
    }, 350);
  };
  $('#claim').onclick = async () => {
    const box = $('#claim').closest('.box');
    $('#claim').disabled = true;
    try {
      msg(box, 'Baue die Transaktion …', 'wait');
      const offer = await api('/claim', { tag: $('#tg').value });
      if (!confirm(`@${offer.tag} beanspruchen?\nGebühr: ${pep(offer.tx.fee)} PEP (Testnet)`)) { msg(box, 'Abgebrochen.', 'muted'); upd(); return; }
      const hex = C.signTx(offer.tx, acct);                 // signiert hier im Browser
      const sent = await api('/send', { offer: offer.offer, hex });
      await publishOwner($('#nm').value);
      msg(box, `Gesendet: ${sent.txid}`, 'ok');
      await refresh();
    } catch (e) { msg(box, e.message); upd(); }
  };
}

// Besitz-Nachweis (dogecoin-arcade owner.py): "arcade-owner\n<netz>\n<knoten>\n<adresse>\n<ablauf>", signiert wie signmessage
async function publishOwner(name) {
  const expires = Math.floor(Date.now() / 1000) + 400 * 86400 - 3600;
  const words = `arcade-owner\ntest\n${S.node_id}\n${acct.address}\n${expires}`;
  await api('/owner', { expires, sig: C.signMessage(acct.priv, words), name });
}

function pending() {
  steps(2);
  view.innerHTML = `<div class="box"><h2>@${esc(S.pending.tag)} ist unterwegs</h2>
    <p>Die Transaktion ist gesendet und wartet auf den nächsten Block (Testnet: meist wenige Minuten).</p>
    <div class="kv"><b>Transaktion</b><span>${esc(S.pending.txid)}</span></div>
    <p class="msg wait">Warte auf Bestätigung …</p></div>`;
  timer = setInterval(refresh, 15000);
}

function done() {
  steps(4);
  const exp = new Date(S.owner.x * 1000).toLocaleDateString('de-DE');
  view.innerHTML = `<div class="box"><h2>Fertig</h2><p class="big">@${esc(S.tag)}</p>
    <div class="kv"><b>Arcade</b><span>${esc(S.name || '–')}</span><b>Admin-Konto</b><span>${esc(S.operator.address)}</span>
      <b>Knoten</b><span>${esc(S.node_id)}</span><b>Besitz-Nachweis</b><span>gültig bis ${esc(exp)}</span></div>
    <p class="muted">Die anderen Arcades sehen diese Arcade ab jetzt als <b>@${esc(S.tag)}</b> (beim nächsten Austausch, spätestens nach einer Minute).</p>
    <div class="row"><button class="alt" id="ren">Besitz-Nachweis erneuern</button><button class="alt" id="out">Abmelden</button></div></div>`;
  $('#ren').onclick = async () => { try { await publishOwner(S.name); await refresh(); } catch (e) { msg($('#ren').closest('.box'), e.message); } };
  $('#out').onclick = () => { acct = null; render(); };
}

// ------------------------------------------------------------------ Reiter: Chains (teilen, leihen) und Apps

document.querySelectorAll('#tabs button').forEach((b) => { b.onclick = () => { tab = b.dataset.tab; render(); }; });

const nf = (n) => Number(n || 0).toLocaleString('de-DE');

/** Eine signierte Aenderung: Zufallszahl der Box holen, die Nachricht (wie die Box sie nachbaut) signieren, senden */
async function signed(path, head, lines, body) {
  const ch = await api('/challenge');
  const text = `${head}\n${S.node_id}\n${ch.nonce}\n${lines.join('\n')}`;
  return api(path, { ...body, nonce: ch.nonce, sig: C.signMessage(acct.priv, text) });
}

function sw(id, on, disabled = false) {
  return `<label class="sw"><input type="checkbox" id="${esc(id)}" ${on ? 'checked' : ''} ${disabled ? 'disabled' : ''}><span></span></label>`;
}

function nodeInfo(n) {
  if (!n || !n.running) return '<span class="bad">kein Knoten hier erreichbar</span>';
  if (!n.synced) return `<span class="warn">Knoten synchronisiert noch · Block ${nf(n.blocks)} von ${nf(n.headers)}</span>`;
  return `<span class="ok">Knoten läuft · synchron · Block ${nf(n.blocks)}</span>`;
}

async function chainsView() {
  view.innerHTML = '<div class="box"><p class="muted">Lade …</p></div>';
  let c;
  try { c = await api('/chains'); } catch (e) { view.innerHTML = `<div class="box"><p class="msg err">${esc(e.message)}</p></div>`; return; }
  if (tab !== 'chains') return;
  const here = Object.fromEntries(c.chains.map((x) => [x.net, x.node && x.node.running]));
  const share = c.chains.map((x) => `<div class="chain">
      <div class="name">${esc(x.name)}${x.offered ? '<span class="pill on">wird angeboten</span>' : ''}</div>${sw(`sh-${x.net}`, x.share, !(x.node && x.node.running) && !x.share)}
      <div class="info">${nodeInfo(x.node)}</div>
      <input class="pay" id="pt-${esc(x.net)}" placeholder="Zahladresse für den Anteil der Leiher (optional)" value="${esc(x.payto || '')}" autocomplete="off" spellcheck="false">
    </div>`).join('');
  const borrow = c.borrow.chains.map((x) => {
    const best = x.sharers[0];
    const agree = x.sharers_agree ? (x.sharers_agree.agree
      ? `<span class="ok">${Object.keys(x.sharers_agree.hashes).length} Teiler einig bei Block ${nf(x.sharers_agree.height)}</span>`
      : `<span class="bad">Teiler uneinig bei Block ${nf(x.sharers_agree.height)}</span>`) : '';
    const who = x.sharers.length
      ? `${x.sharers.length} Arcade${x.sharers.length > 1 ? 's' : ''} teilen sie${best && best.rtt_ms != null ? ` · schnellste ${nf(Math.round(best.rtt_ms))} ms` : ''}`
      : '<span class="warn">gerade teilt sie keine verbundene Arcade</span>';
    return `<div class="chain">
      <div class="name">${esc(x.name)}${x.borrow ? '<span class="pill on">geliehen</span>' : ''}${here[x.net] ? '<span class="pill">läuft hier</span>' : ''}</div>${sw(`br-${x.net}`, x.borrow)}
      <div class="info">${who}${agree ? ' · ' + agree : ''}${x.borrow && x.mempool ? ` · ${nf(x.mempool)} Transaktionen im Mempool` : ''}</div>
    </div>`;
  }).join('');
  view.innerHTML = `<div class="box"><h2>Teilen</h2>
      <p class="muted">Andere Arcades dürfen die Knoten dieser Box fragen: nur Blöcke, Transaktionen und Mempool lesen und fertig signierte Transaktionen weitergeben – nie die Wallet. Angeboten wird eine Chain nur, wenn ihr Knoten synchron ist.</p>
      ${share}
      <p class="muted">Bisher ${nf(c.answers)} Antworten an andere Arcades, ${nf(c.refused)} abgelehnt, ${nf(c.mempool_pushed)} Mempool-Transaktionen weitergegeben.${c.mesh ? '' : ' <span style="color:var(--warn)">meshd nicht verbunden.</span>'}</p></div>
    <div class="box"><h2>Leihen</h2>
      <p class="muted">Eine Chain, deren Knoten hier nicht läuft, bei einer Arcade lesen, die sie teilt. Blöcke und Transaktionen werden geprüft (Hash, Merkle-Wurzel, Arbeit, bei Kotia die Signatur); Höhe und Mempool sind das Wort des Teilers.</p>
      ${borrow}
      <p class="muted">Bisher ${nf(c.borrow.asked)} Anfragen, ${nf(c.borrow.wrong_answers)} falsche Antworten abgewiesen.</p></div>
    <div class="box"><div class="row" style="margin-top:0"><button id="save">Speichern</button><button class="alt" id="reload">Neu laden</button></div>
      <p class="muted">Speichern signiert die Einstellungen mit deinem Admin-Konto.</p></div>`;
  $('#reload').onclick = () => chainsView();
  $('#save').onclick = async () => {
    const box = $('#save').closest('.box');
    const sh = {}, br = {}, pt = {};
    c.chains.forEach((x) => { sh[x.net] = $(`#sh-${x.net}`).checked; const a = $(`#pt-${x.net}`).value.trim(); if (a) pt[x.net] = a; });
    c.borrow.chains.forEach((x) => { br[x.net] = $(`#br-${x.net}`).checked; });
    const lines = [
      'share ' + c.chains.map((x) => `${x.net}=${sh[x.net] ? 1 : 0}`).join(' '),
      'borrow ' + c.borrow.chains.map((x) => `${x.net}=${br[x.net] ? 1 : 0}`).join(' '),
      'payto ' + c.chains.map((x) => `${x.net}=${pt[x.net] || '-'}`).join(' '),
    ];
    try {
      msg(box, 'Signiere und speichere …', 'wait');
      await signed('/chains', 'arcadeos-chains', lines, { share: sh, borrow: br, payto: pt });
      msg(box, 'Gespeichert. Angebote ändern sich innerhalb von 20 Sekunden.', 'ok');
      setTimeout(() => { if (tab === 'chains') chainsView(); }, 1500);
    } catch (e) { msg(box, e.message); }
  };
  // Stand alle 30 s neu, aber nicht mitten in einer Eingabe
  timer = setInterval(() => { if (tab === 'chains' && !view.contains(document.activeElement)) chainsView(); }, 30000);
}

async function appsView() {
  view.innerHTML = '<div class="box"><p class="muted">Lade …</p></div>';
  let a;
  try { a = await api('/apps'); } catch (e) { view.innerHTML = `<div class="box"><p class="msg err">${esc(e.message)}</p></div>`; return; }
  if (tab !== 'apps') return;
  view.innerHTML = `<div class="box"><h2>Apps</h2>
      <p class="muted">Welche Apps die Nutzer dieser Arcade sehen und starten können. Neue Apps sind zuerst aus.</p>
      ${a.apps.map((x) => `<div class="chain"><div class="name">${esc(x.name)}${x.builtin ? '<span class="pill">von dieser Box</span>' : ''}</div>${sw(`ap-${x.id}`, x.on)}
        <div class="info">${esc(x.about)}</div></div>`).join('')}
      <p class="muted">${esc(a.inscribed_note)}</p>
      <div class="row"><button id="save">Speichern</button></div></div>`;
  $('#save').onclick = async () => {
    const box = $('#save').closest('.box');
    const on = {};
    a.apps.forEach((x) => { on[x.id] = $(`#ap-${x.id}`).checked; });
    try {
      msg(box, 'Signiere und speichere …', 'wait');
      await signed('/apps', 'arcadeos-apps', ['apps ' + a.apps.map((x) => `${x.id}=${on[x.id] ? 1 : 0}`).join(' ')], { apps: on });
      msg(box, 'Gespeichert.', 'ok');
    } catch (e) { msg(box, e.message); }
  };
}

refresh();
