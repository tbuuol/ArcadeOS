#!/usr/bin/env python3
"""Baut aus Syfy-UI/index.html die ArcadeOS-Monitoring-Seite: gleiche Optik, echte Daten aus status.json.

    tools/build-monitor-page.py <Syfy-UI/index.html> br-external/board/arcadeos/overlay/usr/share/arcadeos/monitor/index.html
"""
import re
import sys

src, dst = sys.argv[1], sys.argv[2]
s = open(src, encoding="utf-8").read()


def rep(old, new, count=1):
    global s
    if old not in s:
        sys.exit(f"nicht gefunden: {old[:80]!r}")
    s = s.replace(old, new, count)


def section(start_marker, end_marker):
    """Text zwischen zwei JS-Abschnittsmarken (Start inklusive, Ende exklusive)"""
    a = s.index(start_marker)
    b = s.index(end_marker, a)
    return a, b


M = "/* ---------------------------------------------------------------- "

# ---------------------------------------------------------------- Kopf, CSS
rep("<title>Kontrollraum</title>", "<title>ArcadeOS Monitor</title>\n<meta name=\"robots\" content=\"noindex\">")
rep("/* ------------------------------------------------------------------ boot */",
    """/* ------------------------------------------------------------------ monitor */
.sector.o .dot { background:var(--muted); box-shadow:none; animation:none; }
.sector.o .stt { color:var(--muted); }
.foot.lost .live { color:var(--warn); background:rgba(255,80,60,.14); border-right-color:var(--warn); }
.foot.lost .live i { background:var(--warn); box-shadow:0 0 8px var(--warn); }
.modes span b { font-weight:inherit; color:var(--text); }
.stat .v.sm { font-size:clamp(.85rem, .7vw + .3rem, 1.2rem); }

/* ------------------------------------------------------------------ boot */""")

# ---------------------------------------------------------------- Markup
rep('<p style="animation-delay:.1s" id="boot1">KERN-SYSTEME INITIALISIEREN <b>OK</b></p>',
    '<p style="animation-delay:.1s" id="boot1">ARCADEOS MONITOR <b>OK</b></p>')
rep('<p style="animation-delay:.45s">NEURALE SCHNITTSTELLE <b>OK</b></p>', '<p style="animation-delay:.45s">NUR LESEN <b>OK</b></p>')
rep('<p style="animation-delay:.8s">ENERGIEMATRIX SYNCHRON <b>OK</b></p>', '<p style="animation-delay:.8s">STATUS ABRUFEN <b>OK</b></p>')
rep('<p style="animation-delay:1.15s">SENSORNETZ KALIBRIEREN <b>OK</b></p>', '<p style="animation-delay:1.15s">MESH-VERBINDUNGEN <b>OK</b></p>')
rep('<p style="animation-delay:1.5s">KONTROLLE FREIGEGEBEN <b>&#9670;</b></p>', '<p style="animation-delay:1.5s">LIVE <b>&#9670;</b></p>')
rep('data-atl="Atlantis · <em>Kontrollraum</em>" data-oas="OASIS <em>// Nexus</em>">Atlantis · <em>Kontrollraum</em></h1>',
    'data-atl="ArcadeOS · <em>Monitor</em>" data-oas="ARCADEOS <em>// Monitor</em>">ArcadeOS · <em>Monitor</em></h1>')
rep('<span class="on">Kontrolle</span><span>Energie</span><span>Sensoren</span><span>Archiv</span>',
    '<span class="on" id="navHost">—</span><span>Slot <b id="navSlot">—</b></span><span id="navVer">—</span><span>nur lesen</span>')
rep('aria-label="Modi"', 'aria-label="Gerät"')
rep('data-tab="left" aria-selected="false">Status</button>', 'data-tab="left" aria-selected="false">System</button>')
rep('data-tab="core" aria-selected="true">Tor</button>', 'data-tab="core" aria-selected="true">Mesh</button>')
rep('data-tab="right" aria-selected="false">Sensoren</button>', 'data-tab="right" aria-selected="false">Protokoll</button>')
rep('<span data-atl="Energie · ZPM" data-oas="Power Cores">Energie · ZPM</span><span class="code">SYS-01</span>',
    '<span data-atl="Leistung" data-oas="Power Cores">Leistung</span><span class="code" id="cpuModel">CPU</span>')
rep('<div class="sh">Systeme<span class="code">SYS-02</span></div>', '<div class="sh">Sensoren<span class="code" id="loadCode">LAST</span></div>')
rep('<span data-atl="Sektoren" data-oas="Zonen">Sektoren</span><span class="code">SEC-07</span>',
    '<span data-atl="Dienste" data-oas="Prozesse">Dienste</span><span class="code" id="svcCode">—</span>')
rep('<span data-atl="Subraum-Frequenz" data-oas="Signal-Spektrum">Subraum-Frequenz</span><span class="code" id="freq">4.7 THz</span>',
    '<span data-atl="Netzverkehr" data-oas="Datenstrom">Netzverkehr</span><span class="code" id="freq">—</span>')
rep('<span class="lab" data-atl="Zieladresse" data-oas="Zielkoordinate">Zieladresse</span>',
    '<span class="lab" data-atl="Mesh-Verbindungen" data-oas="Mesh-Links">Mesh-Verbindungen</span>')
rep('<b id="statusMain">Wählvorgang</b><span id="statusSub">Chevron 0 verriegelt</span>',
    '<b id="statusMain">Verbinde</b><span id="statusSub">warte auf Gerät</span>')
rep('aria-label="Tor und Nahbereichs-Radar"', 'aria-label="Mesh-Verbindungen als Radar"')
rep('TORKERN <b id="tagCore">STABIL</b><br>ROT. <b id="tagRot">0.0°</b>', 'SYSTEM <b id="tagCore">—</b><br>ROT. <b id="tagRot">0.0°</b>')
rep('RADAR <b>12 KM</b><br>KONTAKTE <b id="tagContacts">7</b>', 'MESH <b id="tagContacts">—</b><br>RTT Ø <b id="tagRtt">—</b>')
rep('<div class="stat"><div class="k" data-atl="Zielwelt" data-oas="Sektor">Zielwelt</div><div class="v" id="sWorld">M4R-227</div><div class="spark"><i style="width:62%"></i></div></div>',
    '<div class="stat"><div class="k">IPv4</div><div class="v sm" id="sIp">—</div><div class="spark"><i id="sIpBar" style="width:0%"></i></div></div>')
rep('<div class="stat"><div class="k">Distanz</div><div class="v" id="sDist">3.42<small>Mio LJ</small></div><div class="spark"><i style="width:40%"></i></div></div>',
    '<div class="stat"><div class="k">Netz</div><div class="v sm" id="sNet">—</div><div class="spark"><i id="sNetBar" style="width:0%"></i></div></div>')
rep('<div class="stat"><div class="k">Energiebedarf</div><div class="v" id="sPow">12.8<small>%</small></div><div class="spark a"><i id="sPowBar" style="width:13%"></i></div></div>',
    '<div class="stat"><div class="k">Laufzeit</div><div class="v" id="sUp">—</div><div class="spark a"><i id="sUpBar" style="width:0%"></i></div></div>')
rep('<div class="stat"><div class="k" data-atl="Schild" data-oas="Firewall">Schild</div><div class="v" id="sShield">87<small>%</small></div><div class="spark"><i style="width:87%"></i></div></div>',
    '<div class="stat"><div class="k">Uhr</div><div class="v sm" id="sTime">—</div><div class="spark"><i id="sTimeBar" style="width:0%"></i></div></div>')
rep('<span data-atl="Sensoren · Stadt" data-oas="Weltkarte">Sensoren · Stadt</span><span class="code">SNS-03</span>',
    '<span data-atl="Prozesse" data-oas="Weltkarte">Prozesse</span><span class="code" id="kernel">—</span>')
rep('<span class="corner c1">N 00°</span><span class="corner c2">R 2.4 KM</span><span class="corner c3">LIVE</span><span class="corner c4" id="mapAlert">3 UNBEK.</span>',
    '<span class="corner c1" id="mapC1">—</span><span class="corner c2" id="mapC2">—</span><span class="corner c3">LIVE</span><span class="corner c4" id="mapAlert">—</span>')
rep('<div><div class="k" data-atl="Lebenszeichen" data-oas="Avatare">Lebenszeichen</div><div class="v" id="life">214</div></div>',
    '<div><div class="k">Prozesse</div><div class="v" id="life">—</div></div>')
rep('<div><div class="k">Unbekannt</div><div class="v a" id="unk">3</div></div>',
    '<div><div class="k">Dienste aus</div><div class="v a" id="unk">—</div></div>')
rep('<div><div class="k" data-atl="Jumper" data-oas="Fahrzeuge">Jumper</div><div class="v" id="jump">2</div></div>',
    '<div><div class="k">Mesh-Links</div><div class="v" id="jump">—</div></div>')
rep('<span data-atl="Komm-Protokoll" data-oas="Chat-Feed">Komm-Protokoll</span><span class="code">CH-12</span>',
    '<span data-atl="Protokoll" data-oas="Feed">Protokoll</span><span class="code">AUDIT</span>')
rep('<div class="fcode" id="fcode">NODE 07 · 42 ms</div>', '<div class="fcode" id="fcode">—</div>')
rep('<footer class="foot panel">', '<footer class="foot panel" id="foot">')

# ---------------------------------------------------------------- Skript
# Theme: nur noch Umschalten der Beschriftungen, keine erfundenen Texte
a, b = section(M + "theme */", M + "header bits */")
s = s[:a] + M + """theme */
let D = null;   // letzter Stand vom Geraet (status.json)
let theme = 'atlantis';
try { theme = localStorage.getItem('scifi-theme') || theme; } catch (e) {}
const qp = new URLSearchParams(location.search).get('theme');
if (qp === 'oasis' || qp === 'atlantis') theme = qp;
let C = {};
function readColors() { C = { pri: css('--pri'), acc: css('--acc'), ok: css('--ok'), warn: css('--warn'), priRgb: css('--pri-rgb'), accRgb: css('--acc-rgb'), bg0: css('--bg0') }; }

function setTheme(t, initial) {
  theme = t;
  root.dataset.theme = t;
  try { localStorage.setItem('scifi-theme', t); } catch (e) {}
  document.querySelectorAll('.switch button').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.t === t)));
  document.querySelectorAll('[data-atl]').forEach(e => { e.innerHTML = t === 'oasis' ? e.dataset.oas : e.dataset.atl; });
  const title = $('#title'); title.dataset.text = title.textContent;
  readColors();
  if (!initial) { bg.reset(); if (D) render(D); }
}
document.querySelectorAll('.switch button').forEach(b => b.addEventListener('click', () => setTheme(b.dataset.t)));

/* mobile tabs */
document.querySelectorAll('.tabs button').forEach(b => b.addEventListener('click', () => {
  $('#app').dataset.tab = b.dataset.tab;
  document.querySelectorAll('.tabs button').forEach(x => x.setAttribute('aria-selected', String(x === b)));
  requestAnimationFrame(() => { sizeCanvas(waveC); sizeCanvas(mapC); });
}));

/* ---------------------------------------------------------------- formatting */
const esc = v => String(v == null ? '' : v).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const pct = (u, t) => t > 0 ? 100 * u / t : 0;
function bytes(n) { const u = ['B', 'kB', 'MB', 'GB', 'TB']; let i = 0; n = +n || 0; while (n >= 1000 && i < u.length - 1) { n /= 1000; i++; } return (i ? n.toFixed(n < 10 ? 1 : 0) : n) + ' ' + u[i]; }
const kb = n => bytes((n || 0) * 1024);
function dur(s) { s = +s || 0; const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60); return d ? `${d}d ${h}h` : h ? `${h}h ${m}m` : `${m}m ${Math.floor(s % 60)}s`; }
function hash(str) { let h = 2166136261; for (const c of String(str)) { h ^= c.charCodeAt(0); h = Math.imul(h, 16777619); } return h >>> 0; }

""" + s[b:]

# Uhr: Datumszeile zeigt Gerätelaufzeit statt Zyklus
rep("""  $('#date').textContent = `${d.toLocaleDateString('de-DE')} · ZYKLUS ${d.getFullYear() - 1800}.${String(doy).padStart(3, '0')}`;""",
    """  $('#date').textContent = `${d.toLocaleDateString('de-DE')} · ${D ? 'GERÄT ' + esc(D.host || '?').toUpperCase() : 'VERBINDE …'}`;""")

# Linkes Panel: Ringe, Balken, Dienste aus den Daten
a, b = section(M + "left panel */", M + "canvases helpers */")
s = s[:a] + M + """left panel */
function ring(v, label, st, cls) {
  const circ = 2 * Math.PI * 38, val = v == null ? 0 : clamp(v, 0, 100);
  let ticks = '';
  for (let k = 0; k < 36; k++) { const a = k * 10 * Math.PI / 180, r1 = 46, r2 = k % 3 ? 48 : 50; ticks += `<line x1="${50 + r1 * Math.sin(a)}" y1="${50 - r1 * Math.cos(a)}" x2="${50 + r2 * Math.sin(a)}" y2="${50 - r2 * Math.cos(a)}"/>`; }
  const hex = Array.from({ length: 6 }, (_, k) => { const a = (k * 60 + 30) * Math.PI / 180; return `${50 + 24 * Math.cos(a)},${50 + 24 * Math.sin(a)}`; }).join(' ');
  return `<div class="zpm ${cls}"><svg viewBox="0 0 100 100" aria-hidden="true"><g class="ticks">${ticks}</g><circle class="track" cx="50" cy="50" r="38"/><circle class="val" cx="50" cy="50" r="38" transform="rotate(-90 50 50)" stroke-dasharray="${circ * val / 100} ${circ}"/><polygon class="hex" points="${hex}"/><text class="pct" x="50" y="50">${v == null ? '–' : Math.round(v) + '%'}</text></svg><div class="lbl">${esc(label)}</div><div class="st">${esc(st).toUpperCase()}</div></div>`;
}
const level = (v, hi, crit) => v == null ? '' : v >= crit ? 'dead' : v >= hi ? 'low' : '';
function buildZpms(d) {
  const data = d.disks.find(x => x.mount === '/state') || d.disks[0];
  const dp = data ? pct(data.used_kb, data.total_kb) : null;
  const mp = pct(d.mem.used_kb, d.mem.total_kb);
  $('#zpms').innerHTML =
    ring(d.cpu.pct, 'CPU', `${d.cpu.cores} Kern${d.cpu.cores === 1 ? '' : 'e'}`, level(d.cpu.pct, 70, 90)) +
    ring(mp, 'RAM', `${(d.mem.used_kb * 1024 / 1e9).toFixed(1)}/${kb(d.mem.total_kb)}`, level(mp, 75, 90)) +
    ring(dp, 'Disk', data ? `${data.name} ${kb(data.total_kb)}` : 'nur RAM', level(dp, 80, 95));
}
function bar(n, v, txt, warn) {
  const on = Math.round(clamp(v || 0, 0, 100) / 100 * 24);
  return `<div class="bar ${warn ? 'warn' : ''}"><div class="row"><span>${esc(n)}</span><b>${esc(txt)}</b></div><div class="segs">${Array.from({ length: 24 }, (_, i) => `<i class="${i < on ? 'on' : ''}"></i>`).join('')}</div></div>`;
}
function wifiPct(dbm) { return clamp(2 * (dbm + 100), 0, 100); }
function buildBars(d) {
  const out = [];
  out.push(d.temp_c != null ? bar('Temperatur', d.temp_c, d.temp_c.toFixed(0) + ' °C', d.temp_c >= 80) : bar('Temperatur', 0, 'kein Sensor'));
  const bat = d.power && d.power.battery;
  if (bat) out.push(bar('Akku', bat.pct, `${bat.pct} % · ${({ Charging: 'lädt', Discharging: 'entlädt', Full: 'voll', 'Not charging': 'hält' })[bat.status] || bat.status || ''}`, bat.pct <= 20 && !d.power.ac));
  else out.push(bar('Akku', d.power && d.power.ac ? 100 : 0, d.power && d.power.ac === true ? 'Netzteil' : 'kein Akku'));
  const l1 = d.cpu.load && d.cpu.load[0];
  if (l1 != null) out.push(bar('Last (1 min)', 100 * l1 / Math.max(1, d.cpu.cores), l1.toFixed(2), l1 > d.cpu.cores));
  // System (/) ist schreibgeschuetzt und immer voll: nur die Datenpartition kann volllaufen
  for (const x of d.disks) out.push(bar(`${x.name} ${x.mount}`, pct(x.used_kb, x.total_kb), `${kb(x.used_kb)} / ${kb(x.total_kb)}`, x.mount !== '/' && pct(x.used_kb, x.total_kb) > 90));
  const w = d.net.find(n => n.type === 'wifi' && n.wifi);
  if (w) out.push(w.wifi.signal_dbm != null ? bar('WLAN-Signal', wifiPct(w.wifi.signal_dbm), `${w.wifi.signal_dbm} dBm`, w.wifi.signal_dbm < -75)
                                            : bar('WLAN-Signal', 0, w.wifi.state === 'off' ? 'aus' : 'sucht', w.wifi.state !== 'off'));
  $('#bars').innerHTML = out.join('');
}
function buildSectors(d) {
  const wifiOn = d.net.some(n => n.type === 'wifi' && n.wifi && n.wifi.state !== 'off');
  let down = 0;
  $('#sectors').innerHTML = d.services.map(sv => {
    const optional = (sv.proc === 'wpa_supplicant' && !wifiOn) || (sv.proc === 'meshd' && !d.mesh.running && !sv.up);
    const cls = sv.up ? '' : optional ? 'o' : 'a';
    if (cls === 'a') down++;
    return `<div class="sector ${cls}"><i class="dot"></i><span class="nm">${esc(sv.name)}</span><span class="stt">${sv.up ? 'läuft' : optional ? 'aus' : 'gestoppt'}</span></div>`;
  }).join('');
  $('#svcCode').textContent = `${d.services.filter(x => x.up).length}/${d.services.length}`;
  return down;
}

""" + s[b:]

# Netzverkehr-Kurve statt Sinuswellen: Empfang (pri) und Senden (acc) aus dem Verlauf
a, b = section("/* waveform */", M + "city map */")
s = s[:a] + """/* waveform: Netzverkehr der letzten Minuten */
function drawWave(t) {
  const [w, h] = sizeCanvas(waveC); if (w < 4 || h < 4) return;
  wctx.clearRect(0, 0, w, h);
  wctx.strokeStyle = rgba(C.priRgb, .12); wctx.lineWidth = 1;
  for (let x = 0; x < w; x += 18 * dpr) { wctx.beginPath(); wctx.moveTo(x, 0); wctx.lineTo(x, h); wctx.stroke(); }
  for (let y = 0; y <= h; y += h / 4) { wctx.beginPath(); wctx.moveTo(0, y); wctx.lineTo(w, y); wctx.stroke(); }
  if (!D || !D.hist) return;
  const rx = D.hist.rx || [], tx = D.hist.tx || [];
  const max = Math.max(1024, ...rx, ...tx) * 1.15;
  for (const [arr, col] of [[rx, C.pri], [tx, C.acc]]) {
    if (arr.length < 2) continue;
    wctx.beginPath(); wctx.strokeStyle = col; wctx.lineWidth = 1.4 * dpr; wctx.shadowColor = col; wctx.shadowBlur = 8 * dpr;
    arr.forEach((v, i) => { const x = w - (arr.length - 1 - i) * w / 89, y = h - 2 - v / max * (h - 4); i ? wctx.lineTo(x, y) : wctx.moveTo(x, y); });
    wctx.stroke();
  }
  wctx.shadowBlur = 0;
}
const waveC = $('#wave'), wctx = waveC.getContext('2d');

""" + s[b:]

# Prozesskarte: ein Punkt je Prozess (bis 60), abgebrochene Dienste als auffällige Punkte
rep("const lifes = Array.from({ length: 34 }, (_, i) => { const r = rng(i + 99); return { a: r() * Math.PI * 2, d: .1 + r() * .75, v: (r() - .5) * .002, unk: i < 3 }; });",
    """let lifes = [];
function setLifes(n, unk) {
  n = clamp(n, 0, 60);
  if (lifes.length === n && lifes.filter(p => p.unk).length === unk) return;
  lifes = Array.from({ length: n }, (_, i) => { const r = rng(i + 99); return { a: r() * Math.PI * 2, d: .1 + r() * .75, v: (r() - .5) * .002, unk: i < unk }; });
}""")

# Radarpunkte: Mesh-Gegenstellen statt Zufallspunkten
rep("""const blipG = el('g', {}, radar);
const blips = Array.from({ length: 7 }, (_, i) => {
  const r = rng(i + 7), a = r() * 360, d = 40 + r() * 125;
  const c = el('circle', { class: 'blip' + (i % 3 ? ' f' : ''), r: i % 3 ? 3 : 4.2, cx: d * Math.sin(a * Math.PI / 180), cy: -d * Math.cos(a * Math.PI / 180), filter: 'url(#glow)' }, blipG);
  return { c, a };
});""", """const blipG = el('g', {}, radar);
let blips = [];
function setBlips(links) {
  blipG.innerHTML = '';
  blips = links.map(l => {
    const a = hash(l.id) % 360, d = 40 + clamp((l.rtt_ms || 200) / 400, 0, 1) * 125;
    const c = el('circle', { class: 'blip' + (l.transport === 'websocket' ? '' : ' f'), r: l.transport === 'websocket' ? 4.2 : 3.4, cx: d * Math.sin(a * Math.PI / 180), cy: -d * Math.cos(a * Math.PI / 180), filter: 'url(#glow)' }, blipG);
    return { c, a };
  });
}""")
rep("const ctB = el('text', { class: 'ctext b', y: 14 }, ctr); ctB.textContent = 'WÄHLEN';",
    "const ctB = el('text', { class: 'ctext b', y: 14 }, ctr); ctB.textContent = '…';")

# Wählvorgang: Ring dreht langsam, Chevrons/Adressfelder = Mesh-Verbindungen
a, b = section(M + "dialing state machine */", M + "comm log */")
s = s[:a] + M + """mesh display */
let ringAngle = 0, sweepAngle = 0, ringTarget = 0;
const ORDER = [0, 1, 8, 2, 7, 3, 6, 4, 5];
function setStatus(main, sub, ok) {
  $('#statusMain').textContent = main; $('#statusSub').textContent = sub;
  $('#status').classList.toggle('ok', !!ok);
}
let shownLinks = '';
function renderMesh(m) {
  const links = (m.links || []).slice().sort((a, b) => a.id < b.id ? -1 : 1);
  const key = links.map(l => l.id).join(',');
  const rtts = links.map(l => l.rtt_ms).filter(x => x != null);
  const avg = rtts.length ? rtts.reduce((a, b) => a + b, 0) / rtts.length : null;
  chevs.forEach((c, j) => c.classList.toggle('lit', ORDER.indexOf(j) < links.length));
  glyphEls.forEach(g => g.classList.remove('sel'));
  slotEls.forEach((el2, i) => {
    const l = links[i];
    if (l) { const g = hash(l.id) % 39; glyphEls[g].classList.add('sel'); el2.innerHTML = glyphSvg(g, 1.2) + `<span>${i + 1}</span>`; el2.classList.add('on'); el2.title = `${l.id} · ${l.transport} · ${l.rtt_ms != null ? l.rtt_ms + ' ms' : '–'} · seit ${dur(l.up_s)}`; }
    else { el2.classList.remove('on'); el2.innerHTML = `<span>${i + 1}</span>`; el2.title = ''; }
  });
  if (key !== shownLinks) {
    if (links.length) ringTarget = -(hash(links[links.length - 1].id) % 39) * 360 / 39 - 360;
    if (shownLinks !== '') {
      const before = shownLinks ? shownLinks.split(',') : [];
      for (const l of links) if (!before.includes(l.id)) addLog(`Verbindung zu ${l.id} (${l.transport}${l.rtt_ms != null ? ', ' + l.rtt_ms + ' ms' : ''})`, 'MESH', false);
      for (const id of before) if (!links.some(l => l.id === id)) addLog(`Verbindung zu ${id} getrennt`, 'MESH', true);
    }
    shownLinks = key; setBlips(links);
  }
  eh.classList.toggle('on', links.length > 0);
  if (!m.running) { ctS.textContent = 'MESH'; ctB.textContent = 'AUS'; setStatus('Mesh aus', 'meshd läuft nicht'); }
  else if (!links.length) { ctS.textContent = 'MESH'; ctB.textContent = 'SUCHT'; setStatus('Suche', `${m.dialing || 0} Versuche · ${m.addrs_known || 0} Adr.`); }
  else { ctS.textContent = 'MESH-LINKS'; ctB.textContent = String(links.length); setStatus(theme === 'oasis' ? 'Portal offen' : 'Verbunden', `${links.length} Link${links.length === 1 ? '' : 's'}${avg != null ? ' · Ø ' + avg.toFixed(0) + ' ms' : ''}`, true); }
  $('#tagContacts').textContent = String(links.length);
  $('#tagRtt').textContent = avg != null ? avg.toFixed(0) + ' ms' : '–';
  $('#jump').textContent = String(links.length);
  $('#fcode').textContent = m.running ? `NODE ${m.node_id ? m.node_id.slice(0, 8).toUpperCase() : '?'} · ${avg != null ? avg.toFixed(0) + ' ms' : 'kein Link'}` : 'MESH AUS';
}

""" + s[b:]

# Protokoll: Einträge aus dem Audit-Log des Geräts plus eigene Ereignisse der Seite
a, b = section("let logN = 0;", M + "ticker */")
s = s[:a] + """let lastAudit = null;
const ALERT = /rollback|fail|fehl|error|timeout|abgelehnt/i;
function renderAudit(list) {
  if (!list || !list.length) return;
  let start = 0;
  if (lastAudit) { const i = list.findIndex(e => e.t + e.action + e.detail === lastAudit); start = i >= 0 ? i + 1 : 0; }
  const fresh = lastAudit === null;
  for (const e of list.slice(start)) {
    const t = e.t ? new Date(e.t) : null;
    addLog(e.detail || '', String(e.action || '').toUpperCase(), ALERT.test(e.action + ' ' + e.detail), fresh, t && !isNaN(t) ? (Date.now() - t) / 1000 : 0);
  }
  const l = list[list.length - 1]; lastAudit = l.t + l.action + l.detail;
}

""" + s[b:]
rep("function ts(offsetSec = 0) { const d = new Date(Date.now() - offsetSec * 1000); return d.toLocaleTimeString('de-DE', { hour12: false }); }",
    "function ts(offsetSec = 0) { const d = new Date(Date.now() - offsetSec * 1000); return (offsetSec > 86400 ? d.toLocaleDateString('de-DE', { day: '2-digit', month: '2-digit' }) + ' ' : '') + d.toLocaleTimeString('de-DE', { hour12: false }); }")
rep("li.innerHTML = `<span class=\"t\">${ts(offset || 0)}</span><span class=\"who\">${who}</span><span class=\"msg\"></span>`;",
    "li.innerHTML = `<span class=\"t\">${ts(offset || 0)}</span><span class=\"who\">${esc(who)}</span><span class=\"msg\"></span>`;")

# Laufband: feste Gerätedaten
a, b = section(M + "ticker */", M + "background canvas */")
s = s[:a] + M + """ticker */
let tickerKey = '';
function buildTicker(d) {
  const items = [];
  if (d.cpu.model) items.push(esc(d.cpu.model));
  items.push(`Kernel <em>${esc(d.kernel)}</em>`, `ArcadeOS <em>${esc(d.version)}</em> · Kanal ${esc(d.channel)}`);
  for (const x of d.drives || []) items.push(`Laufwerk ${esc(x.name)} <em>${esc(x.size_gb)} GB</em>${x.removable ? ' (wechselbar)' : ''}`);
  items.push(`Daten: <em>${d.state_mode === 'persistent' ? 'bleiben erhalten' : 'nur im RAM'}</em>`);
  for (const n of d.net) items.push(`${esc(n.name)} ${esc(n.mac)} · ↓ ${bytes(n.rx_bytes)} ↑ ${bytes(n.tx_bytes)}`);
  if (d.gateway) items.push(`Router ${esc(d.gateway)}`);
  items.push(`Uhr <em>${d.time_synced ? 'gestellt (NTP)' : 'nicht gestellt'}</em>`);
  if (d.mesh.running) items.push(`Mesh-Knoten <em>${esc(d.mesh.node_id)}</em> · ${d.mesh.addrs_reached || 0}/${d.mesh.addrs_known || 0} Adressen erreicht`);
  items.push(`${d.cpu.procs} Prozesse`);
  if (d.config_pending) items.push('<em>Konfigurationsänderung wartet auf Bestätigung</em>');
  const html = items.map(t => `<span>${t}</span>`).join('');
  if (html === tickerKey) return;
  tickerKey = html;
  $('#ticker').innerHTML = html + html;
}

""" + s[b:]

# Hauptschleife: Ring dreht langsam weiter und gleitet zur neuesten Verbindung
rep("  glyphRingEl.setAttribute('transform', `rotate(${ringAngle.toFixed(2)})`);",
    """  if (!reduce) { ringTarget -= .02; ringAngle += (ringTarget - ringAngle) * .02; }
  glyphRingEl.setAttribute('transform', `rotate(${ringAngle.toFixed(2)})`);""")

# Zahlen + Start: Daten abholen statt Zufall
a, b = section(M + "live numbers */", "})();\n</script>")
s = s[:a] + M + """live data */
let fails = 0, lastT = 0, lastChange = Date.now();
function render(d) {
  $('#navHost').textContent = d.host || '?';
  $('#navSlot').textContent = d.slot || '?';
  $('#navVer').textContent = (d.version || '?').replace(/^(\\d{8})\\.\\d{4}-/, '$1 ');
  $('#cpuModel').textContent = d.cpu.cores + '×CPU';
  $('#kernel').textContent = 'LINUX ' + (d.kernel || '');
  const l = d.cpu.load || [];
  $('#loadCode').textContent = l.length ? l.map(x => x.toFixed(2)).join(' ') : '—';
  buildZpms(d); buildBars(d);
  const down = buildSectors(d);
  $('#freq').textContent = `↓ ${bytes(d.rx_bps)}/s ↑ ${bytes(d.tx_bps)}/s`;
  renderMesh(d.mesh);
  const main = d.net.find(n => n.default_route) || d.net.find(n => n.up) || d.net[0];
  const ip = main && main.ipv4[0];
  $('#sIp').textContent = ip ? ip.replace(/\\/\\d+$/, '') : 'keine';
  $('#sIpBar').style.width = ip ? '100%' : '0%';
  if (main && main.type === 'wifi' && main.wifi) {
    $('#sNet').textContent = main.up ? `WLAN ${main.wifi.signal_dbm != null ? main.wifi.signal_dbm + ' dBm' : ''}` : 'WLAN sucht';
    $('#sNetBar').style.width = (main.wifi.signal_dbm != null ? wifiPct(main.wifi.signal_dbm) : 0) + '%';
  } else if (main) {
    $('#sNet').textContent = main.up ? `Kabel ${main.speed_mbit ? main.speed_mbit + ' Mbit' : ''}` : 'kein Kabel';
    $('#sNetBar').style.width = main.up ? '100%' : '0%';
  } else { $('#sNet').textContent = 'keine Karte'; $('#sNetBar').style.width = '0%'; }
  $('#sUp').textContent = dur(d.uptime_s);
  $('#sUpBar').style.width = clamp(d.uptime_s / 864, 0, 100) + '%';
  $('#sTime').textContent = d.time_synced ? 'gestellt' : 'nicht gestellt';
  $('#sTimeBar').style.width = d.time_synced ? '100%' : '15%';
  const warn = down > 0 || (d.temp_c != null && d.temp_c >= 85) || d.state_mode !== 'persistent';
  $('#tagCore').textContent = warn ? 'WARNUNG' : 'STABIL';
  $('#life').textContent = String(d.cpu.procs);
  $('#unk').textContent = String(down);
  $('#mapC1').textContent = `${d.cpu.procs} PROZ.`;
  $('#mapC2').textContent = d.temp_c != null ? d.temp_c.toFixed(0) + ' °C' : '';
  $('#mapAlert').textContent = down ? `${down} AUS` : 'ALLE OK';
  setLifes(d.cpu.procs, down);
  buildTicker(d);
  renderAudit(d.audit);
}
// Lua-cjson schreibt leere Listen als {}: alle Listenfelder auf echte Arrays bringen
const arr = x => Array.isArray(x) ? x : [];
function normalize(d) {
  for (const k of ['disks', 'drives', 'net', 'services', 'audit']) d[k] = arr(d[k]);
  d.cpu = d.cpu || {}; d.cpu.load = arr(d.cpu.load);
  d.mem = d.mem || { total_kb: 0, used_kb: 0 };
  d.power = d.power || {};
  for (const n of d.net) n.ipv4 = arr(n.ipv4);
  d.mesh = d.mesh || { running: false }; d.mesh.links = arr(d.mesh.links);
  d.hist = d.hist || {}; for (const k of ['cpu', 'mem', 'rx', 'tx', 'temp']) d.hist[k] = arr(d.hist[k]);
  return d;
}
async function poll() {
  try {
    const r = await fetch('status.json', { cache: 'no-store' });
    if (!r.ok) throw new Error('HTTP ' + r.status);
    const d = normalize(await r.json());
    if (fails >= 3) addLog('Verbindung zum Gerät wieder da', 'MONITOR', false);
    fails = 0;
    $('#foot').classList.remove('lost');
    if (d.t !== lastT) {
      if (lastT && d.t - lastT > 30) addLog(`Sammler war ${d.t - lastT} s still`, 'MONITOR', true);
      lastT = d.t; lastChange = Date.now(); D = d; render(d);
    } else if (Date.now() - lastChange > 15000) {
      // Geraeteuhr kann falsch gehen (ohne NTP): Alter nach der Uhr des Browsers messen
      setStatus('Daten veraltet', `Sammler seit ${Math.round((Date.now() - lastChange) / 1000)} s still`);
    }
  } catch (e) {
    fails++;
    if (fails === 3) { addLog('Gerät antwortet nicht', 'MONITOR', true); $('#foot').classList.add('lost'); setStatus('Keine Verbindung', 'Gerät antwortet nicht'); }
  }
  setTimeout(poll, (D && D.interval_s ? D.interval_s : 2) * 1000);
}

/* ---------------------------------------------------------------- init */
setTheme(theme, true);
bg.resize();
requestAnimationFrame(loop);
if (reduce) requestAnimationFrame(() => { drawWave(0); drawMap(0); });
setTimeout(() => $('#app').classList.add('ready'), reduce ? 0 : 3400);
setInterval(() => { if (reduce) { drawWave(0); drawMap(0); } }, 2000);
poll();
addEventListener('resize', () => { if (reduce) requestAnimationFrame(loop); });
""" + s[b:]

# Reste, die es nicht mehr gibt
for left in ["dialLoop", "prefillLog", "nextLog", "jitter", "TEXT[", "sWorld", "sPow"]:
    if left in s:
        sys.exit(f"Rest gefunden: {left}")
open(dst, "w", encoding="utf-8").write(s)
print("ok", len(s.encode()))
