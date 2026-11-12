//! arcadeos-admin – Einrichtung der Arcade auf ArcadeOS.
//!
//! Der Betreiber hat ein Konto, dessen Schluessel nur in seinem Browser liegen (12 Woerter, wie in den
//! Arcade-Wallets). Diese Box kennt davon nur den oeffentlichen Schluessel und die Adresse. Sie
//!   * nimmt den Betreiber einmal an (Beweis: Signatur ueber eine Zufallszahl der Box),
//!   * beobachtet seine Adresse im Pepecoin-Testnet-Knoten (watch-only),
//!   * fuehrt den @name-Index aus der eigenen Chain (Regeln wie die Arcades: dogecoin-arcade tags.py,
//!     state.py _tag, tx.py determine_sender; ab Block 1.496.133),
//!   * baut die unsignierte Claim-Transaktion und sendet nur genau diese, wenn sie signiert zurueckkommt,
//!   * nimmt den signierten Besitz-Nachweis (owner.py) an und legt fuer meshd about.json ab.
//!
//! HTTP nur auf 127.0.0.1 (Caddy reicht /admin/api/ nur aus dem LAN weiter). Laeuft als Nutzer des Knotens
//! (liest dessen RPC-Cookie). Keine privaten Schluessel, kein Signieren auf der Box.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;

mod public;
mod utxo;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use ripemd::Ripemd160;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// ------------------------------------------------------------------ Pepecoin-Testnet und Arcade-Regeln
const PKH: u8 = 113; // Pepecoin-Testnet P2PKH
const P2SH: u8 = 196;
const ACTIVATION: u64 = 1_496_133; // dogecoin-arcade config.py TESTNET.activation_height
const MARKER: &[u8] = b"arcd"; // config.py MARKER (Class C: OP_RETURN marker || payload)
const ANYDATA: u16 = 200; // payload.py AnyData (Omni-Kopf: version u16 BE, type u16 BE)
const INSC: &[u8] = b"INSC"; // inscriptions.py MAGIC, VERSION 1
const KIND_CLAIM: u8 = 3;
const KIND_TRANSFER: u8 = 4;
const COIN: u64 = 100_000_000;
const MIN_FEE_PER_KB: u64 = COIN / 100; // fees.py MIN_FEE_PER_KB
const DUST: u64 = COIN / 100; // fees.py DUST_LIMIT
const SIG_MAGIC: &str = "Pepecoin Signed Message:\n";
const OWNER_MOST_DAYS: u64 = 400; // owner.py MOST_DAYS
const RESERVED: &[&str] = &[
    "admin", "administrator", "arcade", "dogecoinarcade", "support", "help", "official", "system", "root",
    "moderator", "mod", "staff", "team", "pepecoin", "dogecoin", "null", "none", "anonymous", "me", "you",
];

struct Cfg {
    listen: String,
    rpc: String,
    cookie: PathBuf,
    state: PathBuf,
    meshd_status: PathBuf,
    release: PathBuf,
}

fn env(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn log(msg: &str) {
    eprintln!("arcadeos-admin: {msg}");
}

// ------------------------------------------------------------------ Hashes, Adressen, Signaturen
fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}
fn dsha(b: &[u8]) -> [u8; 32] {
    sha256(&sha256(b))
}
fn hash160(b: &[u8]) -> [u8; 20] {
    Ripemd160::digest(sha256(b)).into()
}
fn b58check(version: u8, payload: &[u8]) -> String {
    let mut v = vec![version];
    v.extend_from_slice(payload);
    let c = dsha(&v);
    v.extend_from_slice(&c[..4]);
    bs58::encode(v).into_string()
}
fn p2pkh_script(h: &[u8; 20]) -> String {
    format!("76a914{}88ac", hex::encode(h))
}
/// Adresse aus einem Ausgabe-Skript (nur P2PKH und P2SH, wie die Arcades als Absender zulassen)
fn script_address(spk: &[u8]) -> Option<String> {
    if spk.len() == 25 && spk[0] == 0x76 && spk[1] == 0xa9 && spk[2] == 0x14 && spk[23] == 0x88 && spk[24] == 0xac {
        return Some(b58check(PKH, &spk[3..23]));
    }
    if spk.len() == 23 && spk[0] == 0xa9 && spk[1] == 0x14 && spk[22] == 0x87 {
        return Some(b58check(P2SH, &spk[2..22]));
    }
    None
}
fn varint(n: usize) -> Vec<u8> {
    if n < 0xfd {
        vec![n as u8]
    } else {
        let mut v = vec![0xfd];
        v.extend_from_slice(&(n as u16).to_le_bytes());
        v
    }
}
/// Core "verifymessage": stimmt die base64-Signatur ueber `message` zur Adresse?
fn verify_message(sig_b64: &str, message: &str, address: &str) -> bool {
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(sig_b64.trim()) else { return false };
    if raw.len() != 65 || raw[0] < 27 || raw[0] > 34 {
        return false;
    }
    let head = raw[0] - 27;
    let compressed = head >= 4;
    let Some(recid) = RecoveryId::from_byte(head & 3) else { return false };
    let Ok(mut sig) = Signature::from_slice(&raw[1..65]) else { return false };
    let mut recid = recid;
    if let Some(n) = sig.normalize_s() {
        sig = n;
        recid = RecoveryId::new(!recid.is_y_odd(), recid.is_x_reduced());
    }
    let mut m = varint(SIG_MAGIC.len());
    m.extend_from_slice(SIG_MAGIC.as_bytes());
    m.extend(varint(message.len()));
    m.extend_from_slice(message.as_bytes());
    let Ok(key) = VerifyingKey::recover_from_prehash(&dsha(&m), &sig, recid) else { return false };
    let pt = key.to_encoded_point(compressed);
    b58check(PKH, &hash160(pt.as_bytes())) == address
}

// ------------------------------------------------------------------ @name-Regeln (tags.py)
fn validate_tag(input: &str) -> Result<String, String> {
    let t = input.trim().trim_start_matches('@').to_lowercase();
    if t.len() < 2 {
        return Err("Ein @name braucht mindestens 2 Zeichen.".into());
    }
    if t.len() > 24 {
        return Err("Ein @name hat hoechstens 24 Zeichen.".into());
    }
    if !t.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_') {
        return Err("Nur Kleinbuchstaben, Ziffern und _ (kein Bindestrich, kein Punkt).".into());
    }
    if RESERVED.contains(&t.as_str()) {
        return Err(format!("@{t} ist reserviert (wirkt offiziell)."));
    }
    Ok(t)
}
/// OP_RETURN-Daten eines Claims: marker || Omni(version 0, type 200) || INSC 1 kind len tag
fn tag_data(tag: &str, kind: u8) -> Vec<u8> {
    let mut d = MARKER.to_vec();
    d.extend_from_slice(&0u16.to_be_bytes());
    d.extend_from_slice(&ANYDATA.to_be_bytes());
    d.extend_from_slice(INSC);
    d.extend_from_slice(&[1, kind, tag.len() as u8]);
    d.extend_from_slice(tag.as_bytes());
    d
}
/// (kind, tag) aus den OP_RETURN-Daten, wenn es ein gueltiger Tag-Eintrag ist
fn parse_tag_data(d: &[u8]) -> Option<(u8, String)> {
    let p = d.strip_prefix(MARKER)?;
    if p.len() < 4 + 7 || u16::from_be_bytes([p[2], p[3]]) != ANYDATA {
        return None;
    }
    let a = &p[4..];
    if &a[..4] != INSC || a[4] != 1 || !(a[5] == KIND_CLAIM || a[5] == KIND_TRANSFER) {
        return None;
    }
    let len = a[6] as usize;
    let raw = a.get(7..7 + len)?;
    let tag = std::str::from_utf8(raw).ok()?;
    match validate_tag(tag) {
        Ok(t) if t == tag => Some((a[5], t)),
        _ => None,
    }
}
/// Daten des ersten Pushes nach OP_RETURN
fn op_return_data(spk: &[u8]) -> Option<Vec<u8>> {
    if spk.first() != Some(&0x6a) || spk.len() < 2 {
        return None;
    }
    let (n, at) = match spk[1] {
        n @ 1..=75 => (n as usize, 2),
        0x4c => (*spk.get(2)? as usize, 3),
        _ => return None,
    };
    spk.get(at..at + n).map(|s| s.to_vec())
}

// ------------------------------------------------------------------ RPC zum Pepecoin-Knoten
fn rpc(cfg: &Cfg, method: &str, params: Value) -> Result<Value, String> {
    let v = rpc_post(cfg, &json!({"jsonrpc": "1.0", "id": "admin", "method": method, "params": params}).to_string())?;
    if !v["error"].is_null() {
        return Err(format!("{}", v["error"]["message"].as_str().unwrap_or("RPC-Fehler")));
    }
    Ok(v["result"].clone())
}

/// Rohe JSON-RPC-Anfrage (einzeln oder als Batch-Liste)
fn rpc_post(cfg: &Cfg, body: &str) -> Result<Value, String> {
    let cookie = std::fs::read_to_string(&cfg.cookie).map_err(|e| format!("RPC-Cookie: {e} (laeuft pepecoind?)"))?;
    let auth = base64::engine::general_purpose::STANDARD.encode(cookie.trim());
    let mut s = TcpStream::connect(&cfg.rpc).map_err(|e| format!("RPC: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(120))).ok();
    write!(
        s,
        "POST / HTTP/1.1\r\nHost: {}\r\nAuthorization: Basic {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        cfg.rpc,
        body.len()
    )
    .map_err(|e| format!("RPC: {e}"))?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(|e| format!("RPC: {e}"))?;
    let at = resp.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    serde_json::from_slice(&resp[at..]).map_err(|_| format!("RPC: keine Antwort ({})", String::from_utf8_lossy(&resp[..at.min(80)]).lines().next().unwrap_or("")))
}

// ------------------------------------------------------------------ Zustand
#[derive(Default)]
struct Index {
    height: u64,
    hash: String,
    tags: BTreeMap<String, (String, String, u64)>, // tag -> (Adresse, txid, Hoehe)
    by_addr: HashMap<String, String>,
}

impl Index {
    fn load(path: &PathBuf) -> Index {
        let mut ix = Index { height: ACTIVATION - 1, ..Default::default() };
        if let Ok(v) = std::fs::read_to_string(path).map(|s| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null)) {
            if let (Some(h), Some(hash), Some(t)) = (v["height"].as_u64(), v["hash"].as_str(), v["tags"].as_object()) {
                ix.height = h;
                ix.hash = hash.to_string();
                for (tag, e) in t {
                    let (a, tx, ht) = (e[0].as_str().unwrap_or(""), e[1].as_str().unwrap_or(""), e[2].as_u64().unwrap_or(0));
                    ix.tags.insert(tag.clone(), (a.into(), tx.into(), ht));
                    ix.by_addr.insert(a.into(), tag.clone());
                }
            }
        }
        ix
    }
    fn save(&self, path: &PathBuf) {
        let tags: serde_json::Map<String, Value> =
            self.tags.iter().map(|(t, (a, tx, h))| (t.clone(), json!([a, tx, h]))).collect();
        let tmp = path.with_extension("new");
        if std::fs::write(&tmp, json!({"height": self.height, "hash": self.hash, "tags": tags}).to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
    /// Regeln wie dogecoin-arcade state.py _tag
    fn apply(&mut self, kind: u8, tag: &str, sender: &str, reference: Option<&str>, txid: &str, height: u64) {
        let held = self.tags.get(tag).map(|e| e.0.clone());
        if kind == KIND_TRANSFER {
            let Some(to) = reference else { return };
            if held.as_deref() != Some(sender) || to == sender || self.by_addr.contains_key(to) {
                return;
            }
            self.by_addr.remove(sender);
            self.tags.insert(tag.into(), (to.into(), txid.into(), height));
            self.by_addr.insert(to.into(), tag.into());
            return;
        }
        if held.is_some() {
            return; // vergeben (oder schon die eigene)
        }
        if let Some(old) = self.by_addr.remove(sender) {
            self.tags.remove(&old); // neuer Name gibt den alten frei
        }
        self.tags.insert(tag.into(), (sender.into(), txid.into(), height));
        self.by_addr.insert(sender.into(), tag.into());
    }
}

struct App {
    cfg: Cfg,
    index: Mutex<Index>,
    admin: Mutex<Value>, // admin.json: operator, name, pending
    nonces: Mutex<HashMap<String, u64>>,
    offers: Mutex<HashMap<String, (String, Value, String)>>, // offer id -> (tag, unsigned tx, Adresse)
    utxo: Mutex<utxo::Utxo>,
}

impl App {
    fn admin_path(&self) -> PathBuf {
        self.cfg.state.join("admin.json")
    }
    fn save_admin(&self, v: &Value) {
        let p = self.admin_path();
        let tmp = p.with_extension("new");
        if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
    fn node_id(&self) -> Option<String> {
        let s = std::fs::read_to_string(&self.cfg.meshd_status).ok()?;
        let v: Value = serde_json::from_str(&s).ok()?;
        v["node_id"].as_str().filter(|x| x.len() == 64).map(String::from)
    }
    fn version(&self) -> String {
        let r = std::fs::read_to_string(&self.cfg.release).unwrap_or_default();
        let v = r.lines().find_map(|l| l.strip_prefix("VERSION=")).unwrap_or("?");
        format!("ArcadeOS {v}")
    }
    fn operator_addr(&self) -> Option<String> {
        self.admin.lock().unwrap()["operator"]["address"].as_str().map(String::from)
    }
    /// about.json fuer meshd (op/me-Rahmen): @name nur, wenn er dem Betreiber laut Chain gehoert
    fn write_about(&self) {
        let admin = self.admin.lock().unwrap().clone();
        let Some(addr) = admin["operator"]["address"].as_str() else { return };
        let tag = self.index.lock().unwrap().by_addr.get(addr).cloned().unwrap_or_default();
        let mut about = json!({"tag": tag, "name": admin["name"], "version": self.version()});
        if !admin["owner"].is_null() {
            about["owner"] = admin["owner"].clone();
        }
        let p = self.cfg.state.join("about.json");
        let tmp = p.with_extension("new");
        if std::fs::write(&tmp, about.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

// ------------------------------------------------------------------ @name-Index aus der Chain
fn index_block(app: &App, height: u64) -> Result<String, String> {
    let cfg = &app.cfg;
    let hash = rpc(cfg, "getblockhash", json!([height]))?.as_str().unwrap_or("").to_string();
    let block = rpc(cfg, "getblock", json!([hash, true]))?;
    let txids: Vec<String> = block["tx"].as_array().cloned().unwrap_or_default().iter().filter_map(|t| t.as_str().map(String::from)).collect();
    for txid in txids.iter().skip(1) {
        let tx = rpc(cfg, "getrawtransaction", json!([txid, 1]))?;
        let vouts = tx["vout"].as_array().cloned().unwrap_or_default();
        let mut found = None;
        for o in &vouts {
            let spk = hex::decode(o["scriptPubKey"]["hex"].as_str().unwrap_or("")).unwrap_or_default();
            if let Some(d) = op_return_data(&spk) {
                if d.starts_with(MARKER) {
                    found = parse_tag_data(&d);
                    break;
                }
            }
        }
        let Some((kind, tag)) = found else { continue };
        // Absender (Class C): Besitzer des ERSTEN Inputs (tx.py determine_sender)
        let vin0 = &tx["vin"][0];
        let prev = rpc(cfg, "getrawtransaction", json!([vin0["txid"], 1]))?;
        let n = vin0["vout"].as_u64().unwrap_or(0) as usize;
        let spk = hex::decode(prev["vout"][n]["scriptPubKey"]["hex"].as_str().unwrap_or("")).unwrap_or_default();
        let Some(sender) = script_address(&spk) else { continue };
        // Empfaenger einer Uebergabe (tx.py determine_reference)
        let addrs: Vec<String> = vouts
            .iter()
            .filter_map(|o| script_address(&hex::decode(o["scriptPubKey"]["hex"].as_str().unwrap_or("")).unwrap_or_default()))
            .collect();
        let reference = match addrs.len() {
            0 => None,
            1 => Some(addrs[0].clone()),
            _ => {
                let mut r = None;
                let mut change_removed = false;
                for a in &addrs {
                    if *a == sender && !change_removed {
                        change_removed = true;
                        continue;
                    }
                    r = Some(a.clone());
                }
                r
            }
        };
        app.index.lock().unwrap().apply(kind, &tag, &sender, reference.as_deref(), txid, height);
        log(&format!("Block {height}: @{tag} {} {sender}", if kind == KIND_CLAIM { "beansprucht von" } else { "uebergeben von" }));
    }
    Ok(hash)
}

fn indexer(app: Arc<App>) {
    let path = app.cfg.state.join("tags.json");
    loop {
        let step = (|| -> Result<(), String> {
            let tip = rpc(&app.cfg, "getblockcount", json!([]))?.as_u64().unwrap_or(0);
            // Umbau der Chain? Dann von vorn (der Index ist klein)
            let (h, stored) = {
                let ix = app.index.lock().unwrap();
                (ix.height, ix.hash.clone())
            };
            if h >= ACTIVATION && !stored.is_empty() {
                let now_hash = rpc(&app.cfg, "getblockhash", json!([h])).ok().and_then(|v| v.as_str().map(String::from));
                if now_hash.as_deref() != Some(stored.as_str()) {
                    log("Chain hat sich umgebaut: Namens-Index neu");
                    *app.index.lock().unwrap() = Index { height: ACTIVATION - 1, ..Default::default() };
                }
            }
            let mut height = app.index.lock().unwrap().height;
            let mut changed = false;
            while height < tip {
                let hash = index_block(&app, height + 1)?;
                height += 1;
                {
                    let mut ix = app.index.lock().unwrap();
                    ix.height = height;
                    ix.hash = hash;
                }
                changed = true;
                if height % 2000 == 0 {
                    app.index.lock().unwrap().save(&path);
                }
            }
            if changed {
                app.index.lock().unwrap().save(&path);
                app.write_about();
            }
            Ok(())
        })();
        if let Err(e) = step {
            log(&format!("Index: {e}"));
        }
        std::thread::sleep(Duration::from_secs(20));
    }
}

/// Claims im Mempool fuer diesen Namen (noch nicht in einem Block)
fn pending_claims(app: &App, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(ids) = rpc(&app.cfg, "getrawmempool", json!([])) else { return out };
    for id in ids.as_array().cloned().unwrap_or_default().iter().take(300) {
        let Ok(tx) = rpc(&app.cfg, "getrawtransaction", json!([id, 1])) else { continue };
        for o in tx["vout"].as_array().cloned().unwrap_or_default() {
            let spk = hex::decode(o["scriptPubKey"]["hex"].as_str().unwrap_or("")).unwrap_or_default();
            if let Some((KIND_CLAIM, t)) = op_return_data(&spk).and_then(|d| parse_tag_data(&d)) {
                if t == tag {
                    out.push(id.as_str().unwrap_or("").to_string());
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ HTTP
type Resp = (u16, Value);

fn err(code: u16, msg: impl Into<String>) -> Resp {
    (code, json!({"error": msg.into()}))
}

fn status(app: &App) -> Resp {
    let admin = app.admin.lock().unwrap().clone();
    let chain = rpc(&app.cfg, "getblockchaininfo", json!([])).ok();
    let (ix_height, tag) = {
        let ix = app.index.lock().unwrap();
        let tag = admin["operator"]["address"].as_str().and_then(|a| ix.by_addr.get(a).cloned());
        (ix.height, tag)
    };
    (200, json!({
        "node_id": app.node_id(),
        "version": app.version(),
        "chain": chain.map(|c| json!({"chain": c["chain"], "blocks": c["blocks"], "headers": c["headers"],
                                       "ibd": c["initialblockdownload"]})),
        "names_height": ix_height,
        "names_ready": chain_tip(app).map(|t| ix_height + 1 >= t).unwrap_or(false),
        "operator": admin["operator"],
        "name": admin["name"],
        "tag": tag,
        "pending": admin["pending"],
        "owner": admin["owner"],
    }))
}

fn chain_tip(app: &App) -> Option<u64> {
    rpc(&app.cfg, "getblockcount", json!([])).ok()?.as_u64()
}

fn challenge(app: &App) -> Resp {
    let Some(node) = app.node_id() else { return err(503, "meshd laeuft nicht (keine Knoten-Kennung)") };
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).ok();
    let nonce = hex::encode(b);
    let mut n = app.nonces.lock().unwrap();
    n.retain(|_, t| now() < *t);
    n.insert(nonce.clone(), now() + 600);
    (200, json!({"nonce": nonce, "message": format!("arcadeos-admin\n{node}\n{nonce}")}))
}

fn setup(app: &App, b: &Value) -> Resp {
    if !app.admin.lock().unwrap()["operator"].is_null() {
        return err(409, "Diese Arcade hat schon einen Betreiber.");
    }
    let (Some(pubkey), Some(address), Some(nonce), Some(sig)) =
        (b["pubkey"].as_str(), b["address"].as_str(), b["nonce"].as_str(), b["sig"].as_str())
    else {
        return err(400, "pubkey, address, nonce, sig fehlen");
    };
    let Ok(pk) = hex::decode(pubkey) else { return err(400, "pubkey ist kein hex") };
    if pk.len() != 33 || b58check(PKH, &hash160(&pk)) != address {
        return err(400, "Adresse passt nicht zum Schluessel");
    }
    if app.nonces.lock().unwrap().remove(nonce).is_none() {
        return err(400, "Zufallszahl unbekannt oder abgelaufen, bitte neu laden");
    }
    let Some(node) = app.node_id() else { return err(503, "meshd laeuft nicht") };
    if !verify_message(sig, &format!("arcadeos-admin\n{node}\n{nonce}"), address) {
        return err(400, "Signatur passt nicht");
    }
    let mut admin = app.admin.lock().unwrap();
    admin["operator"] = json!({"pubkey": pubkey, "address": address, "since": now()});
    let name = b["name"].as_str().map(clean_name).unwrap_or_default();
    admin["name"] = json!(name);
    app.save_admin(&admin);
    drop(admin);
    app.write_about();
    log(&format!("Betreiber eingerichtet: {address}"));
    (200, json!({"ok": true}))
}

fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(60).collect::<String>().trim().to_string()
}

fn utxos(app: &App, addr: &str) -> Result<Vec<Value>, String> {
    utxo::coins(app, addr)
}

fn sats(v: &Value) -> u64 {
    // Betrag in PEP (Gleitkomma von Core) -> Satoshi, gerundet
    (v.as_f64().unwrap_or(0.0) * COIN as f64).round() as u64
}

fn balance(app: &App) -> Resp {
    let Some(addr) = app.operator_addr() else { return err(409, "noch kein Betreiber") };
    match utxos(app, &addr) {
        Ok(u) => {
            let conf: u64 = u.iter().filter(|x| x["confirmations"].as_u64().unwrap_or(0) > 0).map(|x| x["sats"].as_u64().unwrap_or(0)).sum();
            let unconf: u64 = u.iter().filter(|x| x["confirmations"].as_u64().unwrap_or(0) == 0).map(|x| x["sats"].as_u64().unwrap_or(0)).sum();
            (200, json!({"address": addr, "confirmed": conf, "unconfirmed": unconf, "coins": u.len()}))
        }
        Err(e) => err(502, e),
    }
}

fn name_check(app: &App, q: &str) -> Resp {
    let me = app.operator_addr();
    (200, name_check_for(app, q, me.as_deref()))
}

/// Ist der @name frei? `me`: die Adresse, fuer die gefragt wird ("gehoert schon dir")
fn name_check_for(app: &App, q: &str, me: Option<&str>) -> Value {
    let tag = match validate_tag(q) {
        Ok(t) => t,
        Err(e) => return json!({"ok": false, "reason": e}),
    };
    let held = app.index.lock().unwrap().tags.get(&tag).cloned();
    if let Some((a, tx, h)) = held {
        let mine = me == Some(a.as_str());
        return json!({"ok": mine, "tag": tag, "taken": !mine, "mine": mine, "by": a, "txid": tx, "height": h,
            "reason": if mine { "gehoert schon dir" } else { "vergeben" }});
    }
    let pending = pending_claims(app, &tag);
    if !pending.is_empty() {
        return json!({"ok": false, "tag": tag, "pending": pending, "reason": "wird gerade beansprucht (unbestaetigt)"});
    }
    let ready = chain_tip(app).map(|t| app.index.lock().unwrap().height + 1 >= t).unwrap_or(false);
    json!({"ok": ready, "tag": tag, "free": true, "reason": if ready { "frei" } else { "Namens-Index noch nicht fertig" }})
}

fn claim(app: &App, b: &Value) -> Resp {
    let Some(addr) = app.operator_addr() else { return err(409, "noch kein Betreiber") };
    match build_claim(app, &addr, b["tag"].as_str().unwrap_or("")) {
        Ok(v) => (200, v),
        Err((c, e)) => err(c, e),
    }
}

/// Unsignierte Claim-Transaktion fuer `addr`: Inputs nur von dieser Adresse (Absender = erster Input)
fn build_claim(app: &App, addr: &str, want: &str) -> Result<Value, (u16, String)> {
    let check = name_check_for(app, want, Some(addr));
    if check["ok"] != true || check["mine"] == true {
        return Err((400, check["reason"].as_str().unwrap_or("Name geht nicht").to_string()));
    }
    let tag = check["tag"].as_str().unwrap().to_string();
    let mut coins = utxos(app, addr).map_err(|e| (502, e))?;
    coins.sort_by_key(|c| std::cmp::Reverse(c["sats"].as_u64().unwrap_or(0)));
    let data = tag_data(&tag, KIND_CLAIM);
    let op_return = format!("6a{:02x}{}", data.len(), hex::encode(&data));
    let a = bs58::decode(addr).into_vec().unwrap_or_default();
    if a.len() != 25 {
        return Err((400, "Adresse unlesbar".into()));
    }
    let mut h = [0u8; 20];
    h.copy_from_slice(&a[1..21]);
    let mine = p2pkh_script(&h);
    let mut inputs = Vec::new();
    let mut total = 0u64;
    let mut fee = 0u64;
    for c in &coins {
        if c["scriptPubKey"].as_str() != Some(mine.as_str()) {
            continue;
        }
        inputs.push(json!({"txid": c["txid"], "vout": c["vout"], "script": mine, "amount": c["sats"].as_u64().unwrap_or(0)}));
        total += c["sats"].as_u64().unwrap_or(0);
        let size = 10 + inputs.len() as u64 * 148 + (9 + 2 + data.len() as u64) + 34;
        fee = size.div_ceil(1000) * MIN_FEE_PER_KB;
        if total >= fee + DUST {
            break;
        }
    }
    if inputs.is_empty() || total < fee {
        return Err((400, format!("Zu wenig Testnet-PEP auf {addr}: noetig sind mindestens {} PEP fuer die Gebuehr.", fmt_pep(fee.max(MIN_FEE_PER_KB)))));
    }
    let change = total - fee;
    let mut outputs = vec![json!({"value": 0, "script": op_return})];
    if change >= DUST {
        outputs.push(json!({"value": change, "script": mine}));
    }
    let tx = json!({"version": 1, "locktime": 0, "inputs": inputs, "outputs": outputs, "fee": total - outputs.iter().map(|o| o["value"].as_u64().unwrap_or(0)).sum::<u64>()});
    let mut id = [0u8; 8];
    getrandom::getrandom(&mut id).ok();
    let id = hex::encode(id);
    let mut offers = app.offers.lock().unwrap();
    if offers.len() > 200 {
        offers.clear();
    }
    offers.insert(id.clone(), (tag.clone(), tx.clone(), addr.to_string()));
    Ok(json!({"offer": id, "tag": tag, "tx": tx}))
}

fn fmt_pep(s: u64) -> String {
    format!("{}.{:08}", s / COIN, s % COIN)
}

fn send(app: &App, b: &Value) -> Resp {
    let Some(addr) = app.operator_addr() else { return err(409, "noch kein Betreiber") };
    match send_offer(app, &addr, b) {
        Ok((txid, tag)) => {
            let mut admin = app.admin.lock().unwrap();
            admin["pending"] = json!({"tag": tag, "txid": txid, "at": now()});
            app.save_admin(&admin);
            (200, json!({"txid": txid, "tag": tag}))
        }
        Err((c, e)) => err(c, e),
    }
}

/// Sendet nur genau die fuer `addr` angebotene Transaktion (gleiche Inputs, gleiche Outputs)
fn send_offer(app: &App, addr: &str, b: &Value) -> Result<(String, String), (u16, String)> {
    let (Some(id), Some(hexs)) = (b["offer"].as_str(), b["hex"].as_str()) else { return Err((400, "offer und hex fehlen".into())) };
    let Some((tag, tx, owner)) = app.offers.lock().unwrap().get(id).cloned() else { return Err((404, "Angebot unbekannt (neu bauen)".into())) };
    if owner != addr {
        return Err((403, "Dieses Angebot gehoert zu einem anderen Konto.".into()));
    }
    let dec = rpc(&app.cfg, "decoderawtransaction", json!([hexs])).map_err(|e| (400, format!("Transaktion unlesbar: {e}")))?;
    let vin = dec["vin"].as_array().cloned().unwrap_or_default();
    let vout = dec["vout"].as_array().cloned().unwrap_or_default();
    let want_in = tx["inputs"].as_array().unwrap();
    let want_out = tx["outputs"].as_array().unwrap();
    let same_in = vin.len() == want_in.len()
        && vin.iter().zip(want_in).all(|(a, w)| a["txid"] == w["txid"] && a["vout"] == w["vout"]);
    let same_out = vout.len() == want_out.len()
        && vout.iter().zip(want_out).all(|(a, w)| {
            sats(&a["value"]) == w["value"].as_u64().unwrap_or(u64::MAX) && a["scriptPubKey"]["hex"] == w["script"]
        });
    if !same_in || !same_out {
        return Err((400, "Das ist nicht die angebotene Transaktion - nichts gesendet.".into()));
    }
    let txid = rpc(&app.cfg, "sendrawtransaction", json!([hexs])).map_err(|e| (502, format!("pepecoind hat abgelehnt: {e}")))?;
    app.offers.lock().unwrap().remove(id);
    let txid = txid.as_str().unwrap_or("").to_string();
    log(&format!("Claim @{tag} gesendet: {txid}"));
    Ok((txid, tag))
}

fn owner(app: &App, b: &Value) -> Resp {
    let Some(addr) = app.operator_addr() else { return err(409, "noch kein Betreiber") };
    let Some(node) = app.node_id() else { return err(503, "meshd laeuft nicht") };
    let (Some(expires), Some(sig)) = (b["expires"].as_u64(), b["sig"].as_str()) else { return err(400, "expires und sig fehlen") };
    if expires <= now() || expires > now() + OWNER_MOST_DAYS * 86400 {
        return err(400, format!("Ablauf muss in der Zukunft liegen, hoechstens {OWNER_MOST_DAYS} Tage"));
    }
    let words = format!("arcade-owner\ntest\n{node}\n{addr}\n{expires}");
    if !verify_message(sig, &words, &addr) {
        return err(400, "Signatur passt nicht zum Betreiber");
    }
    let mut admin = app.admin.lock().unwrap();
    admin["owner"] = json!({"a": addr, "x": expires, "s": sig});
    if let Some(n) = b["name"].as_str() {
        admin["name"] = json!(clean_name(n));
    }
    app.save_admin(&admin);
    drop(admin);
    app.write_about();
    log("Besitz-Nachweis gespeichert");
    (200, json!({"ok": true}))
}

fn route(app: &App, method: &str, url: &str, body: &Value) -> Resp {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let path = path.trim_start_matches("/admin/api");
    match (method, path) {
        ("GET", "/status") => status(app),
        ("GET", "/challenge") => challenge(app),
        ("POST", "/setup") => setup(app, body),
        ("GET", "/balance") => balance(app),
        ("GET", "/name") => {
            let q = query.split('&').find_map(|kv| kv.strip_prefix("tag=")).unwrap_or("");
            name_check(app, &q.replace("%40", "@"))
        }
        ("POST", "/claim") => claim(app, body),
        ("POST", "/send") => send(app, body),
        ("POST", "/owner") => owner(app, body),
        _ => err(404, "unbekannt"),
    }
}

fn main() {
    let cfg = Cfg {
        listen: env("ADMIN_LISTEN", "127.0.0.1:8090"),
        rpc: env("ADMIN_RPC", "127.0.0.1:44873"),
        cookie: env("ADMIN_COOKIE", "/media/hdd/arcadeos/pepecoin-testnet/testnet3/.cookie").into(),
        state: env("ADMIN_STATE", "/state/arcade").into(),
        meshd_status: env("ADMIN_MESHD", "/run/meshd.json").into(),
        release: env("ADMIN_RELEASE", "/etc/arcadeos-release").into(),
    };
    if std::env::args().nth(1).as_deref() == Some("--selftest") {
        return selftest();
    }
    std::fs::create_dir_all(&cfg.state).ok();
    let admin = std::fs::read_to_string(cfg.state.join("admin.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({"operator": null, "name": "", "pending": null, "owner": null}));
    let index = Index::load(&cfg.state.join("tags.json"));
    let app = Arc::new(App { cfg, index: Mutex::new(index), admin: Mutex::new(admin), nonces: Default::default(), offers: Default::default(), utxo: Default::default() });
    app.write_about();
    let a = app.clone();
    std::thread::spawn(move || indexer(a));
    let a = app.clone();
    std::thread::spawn(move || utxo::run(a));
    // Oeffentlicher Teil (Konten der Nutzer), eigener Port; Caddy reicht ihn ins Internet
    let a = app.clone();
    let public_listen = env("ADMIN_PUBLIC_LISTEN", "127.0.0.1:8091");
    if public_listen != "off" {
        std::thread::spawn(move || public::serve(a, public_listen));
    }
    let server = match tiny_http::Server::http(&app.cfg.listen) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("kann nicht auf {} lauschen: {e}", app.cfg.listen));
            std::process::exit(1);
        }
    };
    log(&format!("lauscht auf {}", app.cfg.listen));
    for mut req in server.incoming_requests() {
        let mut body = String::new();
        let _ = req.as_reader().take(64 * 1024).read_to_string(&mut body);
        let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let (code, v) = route(&app, req.method().as_str(), &req.url().to_string(), &body);
        let resp = tiny_http::Response::from_string(v.to_string())
            .with_status_code(code)
            .with_header("Content-Type: application/json; charset=utf-8".parse::<tiny_http::Header>().unwrap())
            .with_header("Cache-Control: no-store".parse::<tiny_http::Header>().unwrap());
        let _ = req.respond(resp);
    }
}

/// Selbsttest gegen die Werte, die Browser-Code und pepecoind bestaetigt haben
fn selftest() {
    let pk = hex::decode("02a7451395735369f2ecdfc829c0f774e88ef1303dfe5b2f04dbaab30a535dfdd6").unwrap();
    let addr = b58check(PKH, &hash160(&pk));
    assert_eq!(addr, "nZVmfmUtKPmskB9Ds4P9GUJy9eYFqPKHqH");
    let msg = format!("arcade-owner\ntest\n44872a94f3b2ed417c01089d156e31ad4c15888624c56f6c294954b94e8c6cdc\n{addr}\n1800000000");
    let sig = "IJZ1UWHPqyQapX3kfh13ywN0LnwUUKaZaYcAA+wgJvVIbwe/DkqJ92wBCKNAqGydfUebdHF8BWEWOqWigtPN5UA=";
    assert!(verify_message(sig, &msg, &addr));
    assert!(!verify_message(sig, &(msg.clone() + "x"), &addr));
    assert_eq!(hex::encode(tag_data("arcados_box", KIND_CLAIM)), "61726364000000c8494e534301030b61726361646f735f626f78");
    assert_eq!(parse_tag_data(&tag_data("arcados_box", KIND_CLAIM)), Some((KIND_CLAIM, "arcados_box".into())));
    assert!(validate_tag("Admin").is_err() && validate_tag("a-b").is_err() && validate_tag("x").is_err());
    assert_eq!(validate_tag("@LiteWorlds").unwrap(), "liteworlds");
    let mut ix = Index::default();
    ix.apply(KIND_CLAIM, "bob", "A", None, "t1", 1);
    ix.apply(KIND_CLAIM, "bob", "B", None, "t2", 2); // vergeben
    ix.apply(KIND_CLAIM, "bobby", "A", None, "t3", 3); // gibt bob frei
    assert_eq!(ix.tags.get("bobby").unwrap().0, "A");
    assert!(!ix.tags.contains_key("bob"));
    ix.apply(KIND_TRANSFER, "bobby", "A", Some("C"), "t4", 4);
    assert_eq!(ix.by_addr.get("C").map(String::as_str), Some("bobby"));
    println!("selftest ok");
}
