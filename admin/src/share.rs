//! Teilen: die Chain-Knoten dieser Arcade fuer andere Arcades, ueber das Mesh (Rahmenart `cr`, das Format der
//! Arcades, dogecoin-arcade arcade/mesh/coreshare.py; der Code ist eigener).
//!
//! Standard: NICHTS wird geteilt. Der Betreiber schaltet je Chain frei (Admin-Oberflaeche, signiert mit seinem
//! Konto; gespeichert in <state>/chains.json). Angeboten wird eine Chain nur, wenn sie freigeschaltet ist UND ihr
//! Knoten laeuft und synchron ist.
//!
//! Was andere fragen duerfen, beantwortet allein der Share-Port des Knotens (chaind CHAIND_RPC_SHARE, nur
//! 127.0.0.1, ohne Cookie, nur Lesen der Chain + Weitergeben bereits signierter Transaktionen; die Sperre sitzt
//! im Knoten selbst). Dieser Dienst kennt keine Cookies anderer Knoten und keine Schluessel. Zusaetzlich prueft er
//! selbst gegen SERVED.
//!
//! Grenzen wie die Arcades: hoechstens AT_ONCE_PER_PEER Anfragen einer Arcade zugleich, CORE_AT_ONCE fuer alle
//! zusammen (der Knoten arbeitet fuer uns selbst weiter), MAX_CALLS Aufrufe je Anfrage, MAX_ASKED Bytes je
//! Anfrage, RATE Bytes/s Antwort je Arcade, Antwort hoechstens MAX_ANSWER. Antworten unkomprimiert (ohne `z`:
//! so liest sie jede Arcade).
//!
//! Auf dem Mesh (ueber meshd, /run/meshd/ext.sock):
//!   offer {nets: {net: {h, b, l}}}         beim Verbindungsaufbau und wenn sich das Angebot aendert
//!   q {id, net, seq, end} + Aufrufe        -> a {id, seq, end[, err]} + Antworten (Stuecke zu CHUNK)
//!   sub / tx / gone (Mempool-Strom)        noch nicht: wird ignoriert

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::{log, now, App};

pub struct Chain {
    pub net: &'static str,
    pub name: &'static str,
    /// der Share-Port des Knotens (chaind CHAIND_RPC_SHARE)
    pub rpc: &'static str,
}

/// Die Chains, deren Knoten auf ArcadeOS laufen koennen, mit dem Namen, den die Arcades im Mesh benutzen
pub const CHAINS: &[Chain] = &[
    Chain { net: "test", name: "Pepecoin Testnet", rpc: "127.0.0.1:44874" },
    Chain { net: "ltc", name: "Litecoin", rpc: "127.0.0.1:9334" },
    Chain { net: "ltctest", name: "Litecoin Testnet", rpc: "127.0.0.1:19334" },
    Chain { net: "kot", name: "Kotia", rpc: "127.0.0.1:44884" },
];

/// Was beantwortet wird (wie die Arcades, ohne deren Eigenheiten verifymessage/arcade_*/Mining)
const SERVED: &[&str] = &[
    "getblockchaininfo", "getblockcount", "getbestblockhash", "getblockhash", "getblockheader", "getblock",
    "getrawtransaction", "getrawmempool", "getmempoolentry", "getmempoolinfo", "gettxout", "getdifficulty",
    "sendrawtransaction", "estimatefee", "estimatesmartfee", "getnetworkinfo",
];

const CHUNK: usize = 48 * 1024;
const RATE: f64 = 512.0 * 1024.0;
const AT_ONCE_PER_PEER: usize = 4;
const CORE_AT_ONCE: usize = 2;
const MAX_CALLS: usize = 500;
const MAX_ASKED: usize = 8 * 1024 * 1024;
const MAX_ANSWER: usize = 64 * 1024 * 1024;
const OFFER_EVERY: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Peer {
    rtt_ms: Option<f64>,
    busy: usize,
    /// Zeitpunkt, ab dem wieder gesendet werden darf (Ratenbegrenzung)
    free_at: Option<Instant>,
    served: u64,
}

#[derive(Default)]
struct St {
    /// freigeschaltet vom Betreiber: net -> an
    share: BTreeMap<String, bool>,
    peers: HashMap<String, Peer>,
    asked: HashMap<(String, i64), Vec<u8>>,
    /// was zuletzt angeboten wurde (net -> {h, b, l})
    offered: Map<String, Value>,
    /// je Chain: Stand des Knotens beim letzten Blick (fuer die Admin-Oberflaeche)
    node: BTreeMap<String, Value>,
    linked: bool,
    served_total: u64,
    refused_total: u64,
}

fn st() -> &'static Mutex<St> {
    static S: OnceLock<Mutex<St>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn core_slots() -> &'static (Mutex<usize>, Condvar) {
    static C: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    C.get_or_init(|| (Mutex::new(0), Condvar::new()))
}

type W = Arc<Mutex<Option<UnixStream>>>;

fn writer() -> &'static W {
    static W_: OnceLock<W> = OnceLock::new();
    W_.get_or_init(Default::default)
}

// ------------------------------------------------------------------ Einstellungen

fn settings_path(app: &App) -> std::path::PathBuf {
    app.cfg.state.join("chains.json")
}

/// chains.json: {"share": {net: bool}, "borrow": {...}} - fehlt sie, ist alles aus
pub fn load(app: &App) {
    let v: Value = std::fs::read_to_string(settings_path(app)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    let mut s = st().lock().unwrap();
    s.share = CHAINS.iter().map(|c| (c.net.to_string(), v["share"][c.net].as_bool() == Some(true))).collect();
    drop(s);
    crate::borrow::set_settings(crate::borrow::settings_from(&v));
}

pub fn shared_settings() -> BTreeMap<String, bool> {
    st().lock().unwrap().share.clone()
}

/// Neue Freigaben (nur bekannte Chains), gespeichert; das Angebot folgt beim naechsten Blick (hoechstens OFFER_EVERY)
pub fn set_settings(app: &App, share: &Map<String, Value>, borrow: &Map<String, Value>) -> Result<(), String> {
    let path = settings_path(app);
    let mut v: Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    if !v.is_object() {
        v = json!({});
    }
    let mut new = Map::new();
    for c in CHAINS {
        new.insert(c.net.into(), json!(share.get(c.net).and_then(Value::as_bool).unwrap_or(false)));
    }
    v["share"] = Value::Object(new);
    let mut b = Map::new();
    for l in crate::borrow::lendable() {
        b.insert(l.net.into(), json!(borrow.get(l.net).and_then(Value::as_bool).unwrap_or(false)));
    }
    v["borrow"] = Value::Object(b);
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap()).map_err(|e| e.to_string())?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    load(app);
    wake();
    Ok(())
}

fn wake() {
    let (m, c) = offer_wake();
    *m.lock().unwrap() = true;
    c.notify_all();
}

fn offer_wake() -> &'static (Mutex<bool>, Condvar) {
    static C: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
    C.get_or_init(|| (Mutex::new(false), Condvar::new()))
}

/// Fuer die Admin-Oberflaeche: je Chain freigeschaltet, Stand des Knotens, angeboten, beantwortet
pub fn status() -> Value {
    let s = st().lock().unwrap();
    let chains: Vec<Value> = CHAINS
        .iter()
        .map(|c| {
            json!({"net": c.net, "name": c.name, "share": s.share.get(c.net).copied().unwrap_or(false),
                   "node": s.node.get(c.net).cloned().unwrap_or(json!({"running": false})),
                   "offered": s.offered.contains_key(c.net)})
        })
        .collect();
    let borrowers: Vec<Value> = s.peers.iter().filter(|(_, p)| p.served > 0)
        .map(|(id, p)| json!({"peer": &id[..16.min(id.len())], "answers": p.served})).collect();
    let linked = s.linked;
    let (answers, refused) = (s.served_total, s.refused_total);
    drop(s);
    json!({"chains": chains, "mesh": linked, "answers": answers, "refused": refused, "borrowers": borrowers,
           "borrow": crate::borrow::status()})
}

// ------------------------------------------------------------------ meshd-Schnittstelle

fn encode(h: &Value, blob: &[u8]) -> Vec<u8> {
    let head = serde_json::to_vec(h).unwrap();
    let mut v = Vec::with_capacity(6 + head.len() + blob.len());
    v.extend_from_slice(&((2 + head.len() + blob.len()) as u32).to_be_bytes());
    v.extend_from_slice(&(head.len() as u16).to_be_bytes());
    v.extend_from_slice(&head);
    v.extend_from_slice(blob);
    v
}

fn read_local(r: &mut UnixStream) -> std::io::Result<(Value, Vec<u8>)> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    if !(2..=80 * 1024).contains(&n) {
        return Err(bad("Rahmengroesse"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    let hl = u16::from_be_bytes([b[0], b[1]]) as usize;
    if 2 + hl > n {
        return Err(bad("Kopflaenge"));
    }
    let h: Value = serde_json::from_slice(&b[2..2 + hl]).map_err(|_| bad("Kopf"))?;
    Ok((h, b[2 + hl..].to_vec()))
}

/// Einen cr-Rahmen an eine Arcade
pub fn send(peer: &str, h: Value, blob: &[u8]) -> bool {
    let w = writer().lock().unwrap();
    let Some(s) = w.as_ref() else { return false };
    let mut s: &UnixStream = s;
    s.write_all(&encode(&json!({"op": "send", "peer": peer, "h": h}), blob)).is_ok()
}

// ------------------------------------------------------------------ Knoten fragen (nur der Share-Port)

fn post(addr: &str, body: &[u8], wait: Duration) -> Result<Vec<u8>, String> {
    let sa = addr.parse().map_err(|_| "Adresse".to_string())?;
    let mut s = TcpStream::connect_timeout(&sa, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(wait)).ok();
    s.set_write_timeout(Some(Duration::from_secs(30))).ok();
    write!(s, "POST / HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
    s.write_all(body).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    s.take(MAX_ANSWER as u64 + 4096).read_to_end(&mut out).map_err(|e| e.to_string())?;
    let at = out.windows(4).position(|w| w == b"\r\n\r\n").ok_or("keine HTTP-Antwort")? + 4;
    Ok(out[at..].to_vec())
}

/// Aufrufe [(method, params)] am Share-Port: je Aufruf {"r": ...} oder {"e": [code, message]}
fn ask_node(chain: &Chain, calls: &[(String, Value)]) -> Result<Vec<Value>, String> {
    let batch: Vec<Value> = calls.iter().enumerate().map(|(i, (m, p))| json!({"jsonrpc": "1.0", "id": i, "method": m, "params": p})).collect();
    let raw = post(chain.rpc, Value::Array(batch).to_string().as_bytes(), Duration::from_secs(60))?;
    let got: Value = serde_json::from_slice(&raw).map_err(|_| "Antwort des Knotens ist kein JSON".to_string())?;
    let arr = got.as_array().ok_or("Antwort des Knotens ist keine Liste")?;
    let mut by_id: HashMap<u64, &Value> = HashMap::new();
    for a in arr {
        if let Some(i) = a["id"].as_u64() {
            by_id.insert(i, a);
        }
    }
    Ok((0..calls.len() as u64)
        .map(|i| match by_id.get(&i) {
            Some(a) if a["error"].is_null() => json!({"r": a["result"]}),
            Some(a) => json!({"e": [a["error"]["code"], a["error"]["message"]]}),
            None => json!({"e": [-32603, "no answer"]}),
        })
        .collect())
}

/// Stand des Knotens: Some({h, b}) wenn er laeuft und synchron ist
fn node_tip(chain: &Chain) -> (Value, Option<Value>) {
    match ask_node(chain, &[("getblockchaininfo".into(), json!([]))]) {
        Ok(a) => {
            let i = &a[0]["r"];
            let (blocks, headers) = (i["blocks"].as_u64().unwrap_or(0), i["headers"].as_u64().unwrap_or(0));
            let synced = i["initialblockdownload"] == false && blocks + 2 >= headers && blocks > 0;
            let info = json!({"running": true, "blocks": blocks, "headers": headers, "synced": synced});
            (info, synced.then(|| json!({"h": blocks, "b": i["bestblockhash"], "l": 1})))
        }
        Err(e) => (json!({"running": false, "error": e}), None),
    }
}

// ------------------------------------------------------------------ Ablauf

pub fn run(app: Arc<App>) {
    load(&app);
    std::thread::spawn(offer_loop);
    loop {
        let path = std::env::var("ADMIN_MESHD_EXT").unwrap_or_else(|_| "/run/meshd/ext.sock".into());
        match UnixStream::connect(&path) {
            Ok(mut s) => {
                let _ = s.write_all(&encode(&json!({"op": "hello", "kinds": ["cr"]}), b""));
                *writer().lock().unwrap() = s.try_clone().ok();
                st().lock().unwrap().linked = true;
                log("teilen: mit meshd verbunden");
                let r = session(&mut s);
                *writer().lock().unwrap() = None;
                {
                    let mut g = st().lock().unwrap();
                    g.linked = false;
                    g.peers.clear();
                    g.asked.clear();
                }
                log(&format!("teilen: meshd getrennt ({})", r.err().map(|e| e.to_string()).unwrap_or_default()));
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

fn session(s: &mut UnixStream) -> std::io::Result<()> {
    loop {
        let (h, blob) = read_local(s)?;
        match h["op"].as_str() {
            Some("up") => {
                let Some(peer) = h["peer"].as_str() else { continue };
                let offer = {
                    let mut g = st().lock().unwrap();
                    g.peers.entry(peer.to_string()).or_default();
                    g.offered.clone()
                };
                // nur wenn etwas geteilt wird: ohne Freigabe sagt diese Arcade nichts
                if !offer.is_empty() {
                    send(peer, json!({"t": "cr", "op": "offer", "nets": offer}), b"");
                }
            }
            Some("down") => {
                if let Some(peer) = h["peer"].as_str() {
                    let mut g = st().lock().unwrap();
                    g.peers.remove(peer);
                    g.asked.retain(|k, _| k.0 != peer);
                    drop(g);
                    crate::borrow::on_down(peer);
                }
            }
            Some("links") => {
                let mut g = st().lock().unwrap();
                for l in h["links"].as_array().into_iter().flatten() {
                    if let Some(p) = l["peer"].as_str() {
                        g.peers.entry(p.to_string()).or_default().rtt_ms = l["rtt_ms"].as_f64();
                        crate::borrow::on_rtt(p, l["rtt_ms"].as_f64());
                    }
                }
            }
            Some("frame") => {
                let (Some(peer), f) = (h["peer"].as_str(), &h["h"]) else { continue };
                match f["op"].as_str() {
                    Some("q") => on_request(peer, f, blob),
                    // Leihen (borrow.rs): Angebote merken, Antworten auf eigene Fragen zuordnen
                    Some("offer") => crate::borrow::on_offer(peer, &f["nets"]),
                    Some("a") => crate::borrow::on_answer(peer, f, &blob),
                    // sub/tx/gone (Mempool-Strom): noch nicht
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn answer_err(peer: &str, id: i64, msg: &str) {
    st().lock().unwrap().refused_total += 1;
    send(peer, json!({"t": "cr", "op": "a", "id": id, "seq": 0, "end": true, "err": msg}), b"");
}

fn on_request(peer: &str, f: &Value, blob: Vec<u8>) {
    let Some(id) = f["id"].as_i64() else { return };
    let net = f["net"].as_str().unwrap_or("").to_string();
    let mut g = st().lock().unwrap();
    if !g.offered.contains_key(&net) {
        drop(g);
        return answer_err(peer, id, "this arcade does not share that chain");
    }
    let key = (peer.to_string(), id);
    let parts = g.asked.entry(key.clone()).or_default();
    parts.extend_from_slice(&blob);
    if parts.len() > MAX_ASKED {
        g.asked.remove(&key);
        drop(g);
        return answer_err(peer, id, "too large");
    }
    if f["end"] == false {
        return;
    }
    let body = g.asked.remove(&key).unwrap_or_default();
    let p = g.peers.entry(peer.to_string()).or_default();
    if p.busy >= AT_ONCE_PER_PEER {
        drop(g);
        return answer_err(peer, id, "busy");
    }
    let calls: Option<Vec<(String, Value)>> = serde_json::from_slice::<Value>(&body).ok().and_then(|v| {
        let a = v.as_array()?;
        if a.len() > MAX_CALLS {
            return None;
        }
        a.iter().map(|c| Some((c.get(0)?.as_str()?.to_string(), c.get(1).cloned().filter(Value::is_array).unwrap_or(json!([]))))).collect()
    });
    let Some(calls) = calls else {
        drop(g);
        return answer_err(peer, id, "not a batch");
    };
    p.busy += 1;
    drop(g);
    let peer = peer.to_string();
    std::thread::spawn(move || {
        answer(&peer, id, &net, calls);
        let mut g = st().lock().unwrap();
        if let Some(p) = g.peers.get_mut(&peer) {
            p.busy = p.busy.saturating_sub(1);
        }
    });
}

fn answer(peer: &str, id: i64, net: &str, calls: Vec<(String, Value)>) {
    let Some(chain) = CHAINS.iter().find(|c| c.net == net) else { return answer_err(peer, id, "unknown chain") };
    // nur SERVED; der Rest bekommt eine Absage an seiner Stelle
    let asked: Vec<(String, Value)> = calls.iter().filter(|(m, _)| SERVED.contains(&m.as_str())).cloned().collect();
    // hoechstens CORE_AT_ONCE Anfragen fremder Arcades zugleich am Knoten
    {
        let (m, c) = core_slots();
        let mut n = m.lock().unwrap();
        while *n >= CORE_AT_ONCE {
            n = c.wait(n).unwrap();
        }
        *n += 1;
    }
    let got = if asked.is_empty() { Ok(Vec::new()) } else { ask_node(chain, &asked) };
    {
        let (m, c) = core_slots();
        *m.lock().unwrap() -= 1;
        c.notify_one();
    }
    let got = match got {
        Ok(g) => g,
        Err(e) => return answer_err(peer, id, &format!("node: {}", e.chars().take(150).collect::<String>())),
    };
    let mut it = got.into_iter();
    let answers: Vec<Value> = calls
        .iter()
        .map(|(m, _)| if SERVED.contains(&m.as_str()) { it.next().unwrap_or(json!({"e": [-32603, "no answer"]})) } else { json!({"e": [-32601, format!("a shared Core does not answer {m}")]}) })
        .collect();
    let data = Value::Array(answers).to_string().into_bytes();
    if data.len() > MAX_ANSWER {
        return answer_err(peer, id, "answer too large");
    }
    let parts: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(CHUNK).collect() };
    let n = parts.len();
    for (seq, part) in parts.into_iter().enumerate() {
        // Ratenbegrenzung je Arcade
        let wait = {
            let mut g = st().lock().unwrap();
            let p = g.peers.entry(peer.to_string()).or_default();
            let start = p.free_at.filter(|t| *t > Instant::now()).unwrap_or_else(Instant::now);
            p.free_at = Some(start + Duration::from_secs_f64(part.len() as f64 / RATE));
            start.saturating_duration_since(Instant::now())
        };
        std::thread::sleep(wait);
        if !send(peer, json!({"t": "cr", "op": "a", "id": id, "seq": seq, "end": seq + 1 == n}), part) {
            return;
        }
    }
    let mut g = st().lock().unwrap();
    g.served_total += 1;
    if let Some(p) = g.peers.get_mut(peer) {
        p.served += 1;
    }
}

/// Alle OFFER_EVERY (oder sofort nach einer Aenderung der Freigaben): die freigeschalteten Knoten ansehen, das
/// Angebot bilden und, wenn es sich geaendert hat, allen verbundenen Arcades sagen. Wird nichts (mehr) geteilt,
/// geht einmal ein leeres Angebot hinaus - und danach nie wieder etwas.
fn offer_loop() {
    let mut said_hour = now();
    loop {
        let share = shared_settings();
        let mut offer = Map::new();
        let mut info = BTreeMap::new();
        for c in CHAINS {
            let (i, tip) = node_tip(c);
            info.insert(c.net.to_string(), i);
            if share.get(c.net) == Some(&true) {
                if let Some(t) = tip {
                    offer.insert(c.net.to_string(), t);
                }
            }
        }
        let (changed, peers) = {
            let mut g = st().lock().unwrap();
            g.node = info;
            let changed = g.offered != offer;
            if changed {
                g.offered = offer.clone();
            }
            (changed, g.peers.keys().cloned().collect::<Vec<_>>())
        };
        if changed {
            let nets: Vec<&String> = offer.keys().collect();
            log(&format!("teilen: Angebot jetzt {:?} an {} Arcades", nets, peers.len()));
            for p in &peers {
                send(p, json!({"t": "cr", "op": "offer", "nets": offer}), b"");
            }
        }
        if now() > said_hour + 3600 {
            said_hour = now();
            let g = st().lock().unwrap();
            if !g.offered.is_empty() {
                log(&format!("teilen: {} Antworten bisher, {} abgelehnt", g.served_total, g.refused_total));
            }
        }
        let (m, c) = offer_wake();
        let woke = m.lock().unwrap();
        let (mut woke, _) = c.wait_timeout(woke, OFFER_EVERY).unwrap();
        *woke = false;
    }
}
