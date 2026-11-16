//! Leihen: eine Chain, deren Knoten hier nicht laeuft, bei einer Arcade fragen, die sie teilt (Rahmenart `cr`,
//! Format der Arcades; eigener Code). Standard: AUS. Der Betreiber schaltet je Chain frei (chains.json "borrow").
//!
//! Wer gefragt wird: unter den verbundenen Arcades, die die Chain anbieten, die mit der kuerzesten Antwortzeit;
//! wer zuletzt FAILING-mal versagt hat, kommt hinter alle, die antworten; bei einer falschen Antwort zaehlt es
//! zehnfach. Jede gute Antwort zieht einen Fehler ab.
//!
//! Was geprueft wird (die Antwort kommt von fremden Knoten):
//!   - Bloecke nur roh (getblock h 0): Kennung = angefragter Hash, Merkle-Wurzel und Form (check_block_body),
//!     Arbeit: scrypt/sha256d des Kopfes oder bei Merge-Mining der Elternblock (AuxPoW). Ohne pruefbare Arbeit
//!     (Proof of Stake, DigiBytes Algorithmen) zaehlen Hash, Merkle-Wurzel und Form.
//!   - Transaktionen nur roh (getrawtransaction t 0): ihr Hash = angefragte Kennung.
//!   - Koepfe roh (getblockheader h false): Hash = angefragt (und die Arbeit, wo der Kopf sie allein traegt).
//!   - Alles andere (Hoehe, Block an einer Hoehe, Mempool, unverbrauchte Coins, Gebuehren) ist das Wort des
//!     Teilers. Ausfuehrliche Formen (getblock h 1/2, getrawtransaction t 1) werden nicht gefragt: sie liessen sich
//!     nicht pruefen.
//! Senden (sendrawtransaction) geht nur mit einer bereits signierten Transaktion; Schluessel kennt dieser Dienst
//! nicht.

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use arcade_chain::params::ChainParams;
use arcade_chain::pow::{meets_target, pow_hash, try_pow_hash, PowAlgo};
use arcade_chain::{Block, Hash256, Header, Tx};
use serde_json::{json, Value};

/// Chains, die geliehen werden koennen: Name im Mesh, Anzeige, Regeln (fuer die Pruefung)
pub struct Lendable {
    pub net: &'static str,
    pub name: &'static str,
    pub params: &'static ChainParams,
}

pub fn lendable() -> &'static [Lendable] {
    use arcade_chain::params::*;
    static L: OnceLock<Vec<Lendable>> = OnceLock::new();
    L.get_or_init(|| {
        vec![
            Lendable { net: "main", name: "Pepecoin", params: &PEPECOIN_MAIN },
            Lendable { net: "doge", name: "Dogecoin", params: &DOGECOIN_MAIN },
            Lendable { net: "ltc", name: "Litecoin", params: &LITECOIN_MAIN },
            Lendable { net: "ltctest", name: "Litecoin Testnet", params: &LITECOIN_TEST },
            Lendable { net: "kot", name: "Kotia", params: &KOTIA_MAIN },
            Lendable { net: "test", name: "Pepecoin Testnet", params: &PEPECOIN_TEST },
            Lendable { net: "dgb", name: "DigiByte", params: &DIGIBYTE_MAIN },
            Lendable { net: "bel", name: "Bellscoin", params: &BELLSCOIN_MAIN },
            Lendable { net: "lky", name: "Luckycoin", params: &LUCKYCOIN_MAIN },
            Lendable { net: "wjk", name: "Wojakcoin", params: &WOJAKCOIN_MAIN },
            Lendable { net: "flop", name: "Flopcoin", params: &FLOPCOIN_MAIN },
        ]
    })
}

const FAILING: u32 = 3;
const LIED: u32 = 10;
const TIMEOUT: Duration = Duration::from_secs(60);
const CHUNK: usize = 48 * 1024;
const MAX_ANSWER: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct St {
    /// freigeschaltet: net -> an
    borrow: BTreeMap<String, bool>,
    /// Angebote: peer -> net -> {h, b, ...}
    offers: HashMap<String, serde_json::Map<String, Value>>,
    failures: HashMap<String, u32>,
    rtt: HashMap<String, f64>,
    /// offene Anfragen: id -> (peer, gesammelte Antwort, wohin)
    waiting: HashMap<i64, (String, Vec<u8>, mpsc::Sender<Result<Vec<u8>, String>>)>,
    next_id: i64,
    asked: u64,
    refused: u64,
    /// Mempool, wie die Teiler ihn schicken (tx/gone): net -> Pool
    pool: HashMap<String, Pool>,
    pushed_bad: u64,
    /// gepruefte Koepfe: Hash -> bits (fuer die Schwierigkeits-Regel beim naechsten Block)
    headers: HashMap<Hash256, u32>,
    /// Vergleich der Teiler je Chain (tips_loop)
    tips: BTreeMap<String, Value>,
}

/// Transaktionen, die Teiler gepusht haben: geprueft (sie parsen; die Kennung ist ihr Hash), je Teiler gemerkt,
/// hoechstens POOL_KEEP je Chain (die aeltesten gehen zuerst)
#[derive(Default)]
struct Pool {
    order: std::collections::VecDeque<Hash256>,
    txs: HashMap<Hash256, (Vec<u8>, std::collections::HashSet<String>)>,
}

const POOL_KEEP: usize = 20_000;
const HEADERS_KEEP: usize = 20_000;
const TIPS_EVERY: Duration = Duration::from_secs(120);

fn st() -> &'static Mutex<St> {
    static S: OnceLock<Mutex<St>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(St { next_id: 1, ..Default::default() }))
}

pub fn set_settings(borrow: BTreeMap<String, bool>) {
    let peers: Vec<String> = {
        let mut g = st().lock().unwrap();
        g.borrow = borrow;
        // was nicht (mehr) geliehen wird, braucht keinen Mempool und keinen Vergleich
        let on: Vec<String> = g.borrow.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect();
        g.pool.retain(|n, _| on.contains(n));
        g.tips.retain(|n, _| on.contains(n));
        g.offers.keys().cloned().collect()
    };
    for p in peers {
        subscribe(&p);
    }
}

/// Bei einem Teiler den Mempool-Strom der Chains bestellen, die wir leihen und die er anbietet
fn subscribe(peer: &str) {
    let nets: Vec<String> = {
        let g = st().lock().unwrap();
        let Some(o) = g.offers.get(peer) else { return };
        g.borrow.iter().filter(|(n, on)| **on && o.contains_key(*n)).map(|(n, _)| n.clone()).collect()
    };
    if !nets.is_empty() {
        crate::share::send(peer, json!({"t": "cr", "op": "sub", "nets": nets}), b"");
    }
}

/// Ein Teiler hat eine neue Mempool-Transaktion geschickt: nur fuer geliehene Chains, nur wenn sie parst
pub fn on_pushed(peer: &str, f: &Value, blob: &[u8]) {
    let net = f["net"].as_str().unwrap_or("");
    let Some(l) = lendable().iter().find(|l| l.net == net) else { return };
    let mut g = st().lock().unwrap();
    if g.borrow.get(net) != Some(&true) {
        return;
    }
    let Ok(t) = Tx::parse_with(blob, &l.params.format) else {
        g.pushed_bad += 1;
        return;
    };
    let pool = g.pool.entry(net.to_string()).or_default();
    match pool.txs.get_mut(&t.txid) {
        Some(e) => {
            e.1.insert(peer.to_string());
        }
        None => {
            pool.txs.insert(t.txid, (blob.to_vec(), [peer.to_string()].into()));
            pool.order.push_back(t.txid);
            while pool.order.len() > POOL_KEEP {
                if let Some(old) = pool.order.pop_front() {
                    pool.txs.remove(&old);
                }
            }
        }
    }
}

/// Ein Teiler sagt, dass Transaktionen seinen Mempool verlassen haben (in einem Block oder verworfen)
pub fn on_gone(peer: &str, f: &Value) {
    let net = f["net"].as_str().unwrap_or("");
    let mut g = st().lock().unwrap();
    let Some(pool) = g.pool.get_mut(net) else { return };
    for id in f["ids"].as_array().into_iter().flatten().take(5000).filter_map(Value::as_str) {
        let Ok(h) = Hash256::from_hex(id) else { continue };
        let empty = pool.txs.get_mut(&h).map(|e| {
            e.1.remove(peer);
            e.1.is_empty()
        });
        if empty == Some(true) {
            pool.txs.remove(&h);
            pool.order.retain(|x| *x != h);
        }
    }
}

/// Der gepushte Mempool einer geliehenen Chain: Kennungen (neueste zuletzt)
pub fn pool(net: &str) -> Vec<String> {
    let g = st().lock().unwrap();
    g.pool.get(net).map(|p| p.order.iter().map(|h| h.to_hex()).collect()).unwrap_or_default()
}

/// Eine gepushte Transaktion, roh (schon geprueft)
pub fn pool_tx(net: &str, txid: &Hash256) -> Option<Vec<u8>> {
    st().lock().unwrap().pool.get(net)?.txs.get(txid).map(|e| e.0.clone())
}

pub fn settings_from(v: &Value) -> BTreeMap<String, bool> {
    lendable().iter().map(|l| (l.net.to_string(), v["borrow"][l.net].as_bool() == Some(true))).collect()
}

// ------------------------------------------------------------------ was meshd meldet (aus share.rs)

pub fn on_offer(peer: &str, nets: &Value) {
    {
        let mut g = st().lock().unwrap();
        match nets.as_object() {
            Some(m) if !m.is_empty() => {
                g.offers.insert(peer.to_string(), m.iter().filter(|(_, v)| v.is_object()).map(|(k, v)| (k.clone(), v.clone())).collect());
            }
            _ => {
                g.offers.remove(peer);
                return;
            }
        }
    }
    // wie die Arcades: bei jedem Angebot (wieder) bestellen, was wir leihen (nichts, solange Leihen aus ist)
    subscribe(peer);
}

pub fn on_answer(peer: &str, f: &Value, blob: &[u8]) {
    let Some(id) = f["id"].as_i64() else { return };
    let mut g = st().lock().unwrap();
    let Some((who, parts, _)) = g.waiting.get_mut(&id) else { return };
    if who != peer {
        return; // nur der Gefragte darf antworten
    }
    if let Some(e) = f["err"].as_str() {
        let (_, _, tx) = g.waiting.remove(&id).unwrap();
        let _ = tx.send(Err(e.chars().take(200).collect()));
        return;
    }
    if f.get("z").and_then(Value::as_i64) == Some(1) {
        let (_, _, tx) = g.waiting.remove(&id).unwrap();
        let _ = tx.send(Err("compressed answer (not asked for)".into()));
        return;
    }
    parts.extend_from_slice(blob);
    if parts.len() > MAX_ANSWER {
        let (_, _, tx) = g.waiting.remove(&id).unwrap();
        let _ = tx.send(Err("answer too large".into()));
        return;
    }
    if f["end"] == true {
        let (_, body, tx) = g.waiting.remove(&id).unwrap();
        let _ = tx.send(Ok(body));
    }
}

pub fn on_down(peer: &str) {
    let mut g = st().lock().unwrap();
    g.offers.remove(peer);
    let gone: Vec<i64> = g.waiting.iter().filter(|(_, w)| w.0 == peer).map(|(k, _)| *k).collect();
    for id in gone {
        if let Some((_, _, tx)) = g.waiting.remove(&id) {
            let _ = tx.send(Err("the sharing arcade went away".into()));
        }
    }
}

pub fn on_rtt(peer: &str, rtt_ms: Option<f64>) {
    if let Some(r) = rtt_ms {
        st().lock().unwrap().rtt.insert(peer.to_string(), r);
    }
}

/// Die Teiler einer Chain in der Reihenfolge, in der sie gefragt werden
fn sharers(net: &str) -> Vec<String> {
    let g = st().lock().unwrap();
    let mut v: Vec<String> = g.offers.iter().filter(|(_, n)| n.contains_key(net)).map(|(p, _)| p.clone()).collect();
    v.sort_by(|a, b| {
        let key = |p: &String| {
            let f = g.failures.get(p).copied().unwrap_or(0);
            let failing = f >= FAILING;
            (failing, if failing { f } else { 0 }, (g.rtt.get(p).copied().unwrap_or(9e9) * 1000.0) as u64, f)
        };
        key(a).cmp(&key(b))
    });
    v
}

fn fail(peer: &str, n: u32) {
    *st().lock().unwrap().failures.entry(peer.to_string()).or_default() += n;
}

// ------------------------------------------------------------------ fragen

fn ask_once(peer: &str, net: &str, calls: &[(String, Value)]) -> Result<Vec<Value>, String> {
    let (tx, rx) = mpsc::channel();
    let id = {
        let mut g = st().lock().unwrap();
        let id = g.next_id;
        g.next_id += 1;
        g.waiting.insert(id, (peer.to_string(), Vec::new(), tx));
        g.asked += 1;
        id
    };
    let data = Value::Array(calls.iter().map(|(m, p)| json!([m, p])).collect()).to_string().into_bytes();
    let parts: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(CHUNK).collect() };
    let n = parts.len();
    for (seq, part) in parts.into_iter().enumerate() {
        if !crate::share::send(peer, json!({"t": "cr", "op": "q", "id": id, "net": net, "seq": seq, "end": seq + 1 == n}), part) {
            st().lock().unwrap().waiting.remove(&id);
            return Err("meshd not reachable".into());
        }
    }
    let got = rx.recv_timeout(TIMEOUT);
    st().lock().unwrap().waiting.remove(&id);
    let body = got.map_err(|_| "no answer".to_string())??;
    let v: Value = serde_json::from_slice(&body).map_err(|_| "answer is not JSON".to_string())?;
    let a = v.as_array().ok_or("answer is not a list")?;
    if a.len() != calls.len() {
        return Err("an answer of the wrong length".into());
    }
    Ok(a.clone())
}

/// Was ein Aufruf sein darf: nur pruefbare (rohe) Formen der Lesungen, die Woerter des Teilers, Senden
fn allowed(m: &str, p: &Value) -> Result<(), String> {
    let raw = |i: usize| matches!(p.get(i), None | Some(Value::Bool(false))) || p.get(i).and_then(Value::as_u64) == Some(0);
    match m {
        "getblock" | "getrawtransaction" if raw(1) => Ok(()),
        "getblockheader" if p.get(1) == Some(&Value::Bool(false)) => Ok(()),
        "getblock" | "getrawtransaction" | "getblockheader" => Err(format!("{m}: only the raw form is borrowed (it can be checked)")),
        "getblockchaininfo" | "getblockcount" | "getbestblockhash" | "getblockhash" | "getrawmempool" | "getmempoolentry"
        | "getmempoolinfo" | "gettxout" | "getdifficulty" | "estimatefee" | "estimatesmartfee" | "getnetworkinfo" | "sendrawtransaction" => Ok(()),
        _ => Err(format!("{m} is not borrowed")),
    }
}

fn work_ok(p: &ChainParams, h: &Header, b: Option<&Block>) -> Result<(), String> {
    if let Some(a) = b.and_then(|b| b.auxpow.as_ref()) {
        let chain_id = p.auxpow_chain_id.unwrap_or(h.chain_id());
        a.check(p.block_id(h), chain_id, p.auxpow_strict_chain_id).map_err(|x| x.to_string())?;
        if !meets_target(&pow_hash(PowAlgo::Scrypt, &a.parent.encode()), h.bits) {
            return Err("AuxPoW parent misses its target".into());
        }
        return Ok(());
    }
    if p.format.has_auxpow(h) {
        return Ok(()); // ein Kopf allein traegt seine AuxPoW nicht: Hash zaehlt
    }
    match p.pow.algo(h.version, u32::MAX) {
        Some(Some(algo)) => match try_pow_hash(algo, &h.encode()) {
            Some(ph) if meets_target(&ph, h.bits) => Ok(()),
            Some(_) if matches!(p.pow, arcade_chain::pow::PowRule::PowThenStake { .. }) => Ok(()),
            Some(_) => Err("proof of work misses its target".into()),
            None => Ok(()), // Algorithmus nicht gebaut (DigiByte): Hash und Merkle zaehlen
        },
        _ => Ok(()),
    }
}

/// Eine Antwort pruefen, soweit sie sich aus den Bytes pruefen laesst
fn check(p: &ChainParams, m: &str, params: &Value, a: &Value) -> Result<(), String> {
    let Some(r) = a.get("r") else { return Ok(()) }; // ein Fehler des Knotens: nichts zu pruefen
    match m {
        "getblock" => {
            let want = params.get(0).and_then(Value::as_str).and_then(|s| Hash256::from_hex(s).ok()).ok_or("hash")?;
            let raw = r.as_str().and_then(|s| hex::decode(s).ok()).ok_or("block is not hex")?;
            let b = Block::parse_with(&raw, &p.format).map_err(|e| format!("block does not parse: {e}"))?;
            if b.id != want {
                return Err("block is not the one asked for".into());
            }
            arcade_chain::params::check_block_body(p, &b).map_err(|e| format!("block body: {e}"))?;
            // gestakter Block (Kotia): die Block-Signatur mit dem Schluessel aus der Coinstake-Ausgabe
            if p.format.block_sig && b.is_proof_of_stake() {
                use k256::ecdsa::signature::hazmat::PrehashVerifier;
                let out = b.txs[1].outputs.get(1).ok_or("coinstake without output 1")?;
                let key = arcade_chain::kotia_pos::block_signing_key(&out.script_pubkey).ok_or("no signing key in the coinstake")?;
                let vk = k256::ecdsa::VerifyingKey::from_sec1_bytes(key).map_err(|_| "signing key unreadable")?;
                let s = k256::ecdsa::Signature::from_der(b.sig.as_deref().unwrap_or(&[])).map_err(|_| "block signature unreadable")?;
                let s = s.normalize_s().unwrap_or(s);
                vk.verify_prehash(&b.id.0, &s).map_err(|_| "block signature does not verify")?;
            }
            work_ok(p, &b.header, Some(&b))
        }
        "getrawtransaction" => {
            let want = params.get(0).and_then(Value::as_str).and_then(|s| Hash256::from_hex(s).ok()).ok_or("txid")?;
            let raw = r.as_str().and_then(|s| hex::decode(s).ok()).ok_or("tx is not hex")?;
            let t = Tx::parse_with(&raw, &p.format).map_err(|_| "tx does not parse".to_string())?;
            (t.txid == want).then_some(()).ok_or_else(|| "tx is not the one asked for".into())
        }
        "getblockheader" => {
            let want = params.get(0).and_then(Value::as_str).and_then(|s| Hash256::from_hex(s).ok()).ok_or("hash")?;
            let raw = r.as_str().and_then(|s| hex::decode(s).ok()).ok_or("header is not hex")?;
            let h80: [u8; 80] = raw.get(..80).and_then(|x| x.try_into().ok()).ok_or("header too short")?;
            let h = Header::parse(&h80);
            if p.block_id(&h) != want {
                return Err("header is not the one asked for".into());
            }
            work_ok(p, &h, None)
        }
        _ => Ok(()),
    }
}

/// Aufrufe [(method, params)] auf einer geliehenen Chain: (wer geantwortet hat, je Aufruf {"r"} oder {"e"}).
/// Nur wenn der Betreiber das Leihen dieser Chain freigeschaltet hat.
pub fn call(net: &str, calls: &[(String, Value)]) -> Result<(String, Vec<Value>), String> {
    let l = lendable().iter().find(|l| l.net == net).ok_or("unknown chain")?;
    if st().lock().unwrap().borrow.get(net) != Some(&true) {
        return Err(format!("borrowing {} is off (Admin: Chains)", l.name));
    }
    // eine schon gepushte (gepruefte) Transaktion roh: ohne Frage an einen Teiler
    if let [(m, p)] = calls {
        if m == "getrawtransaction" && allowed(m, p).is_ok() {
            if let Some(raw) = p.get(0).and_then(Value::as_str).and_then(|s| Hash256::from_hex(s).ok()).and_then(|h| pool_tx(net, &h)) {
                return Ok(("local".repeat(4), vec![json!({"r": hex::encode(raw)})]));
            }
        }
    }
    // der gepushte Mempool (nur hier, von den Teilern gesammelt): arcade_pool
    if calls.len() == 1 && calls[0].0 == "arcade_pool" {
        return Ok(("local".repeat(4), vec![json!({"r": pool(net)})]));
    }
    for (m, p) in calls {
        allowed(m, p)?;
    }
    let mut tried = Vec::new();
    for peer in sharers(net) {
        match ask_once(&peer, net, calls) {
            Ok(answers) => {
                let mut bad = calls.iter().zip(&answers).find_map(|((m, p), a)| check(l.params, m, p, a).err().map(|e| format!("{m}: {e}")));
                // Bloecke und Koepfe: die Schwierigkeit darf zum Block darunter nicht mehr als 4x fallen
                if bad.is_none() {
                    bad = calls.iter().zip(&answers).find_map(|((m, _), a)| {
                        let raw = a["r"].as_str().filter(|_| m == "getblock" || m == "getblockheader")?;
                        link_ok(&peer, net, l.params, raw).err().map(|e| format!("{m}: {e}"))
                    });
                }
                if let Some(e) = bad {
                    fail(&peer, LIED);
                    st().lock().unwrap().refused += 1;
                    crate::log(&format!("leihen: falsche Antwort von {} auf {net}: {e}", &peer[..16]));
                    tried.push(format!("{}: {e}", &peer[..12]));
                    continue;
                }
                let mut g = st().lock().unwrap();
                let f = g.failures.entry(peer.clone()).or_default();
                *f = f.saturating_sub(1);
                return Ok((peer, answers));
            }
            Err(e) => {
                fail(&peer, 1);
                tried.push(format!("{}: {e}", &peer[..12.min(peer.len())]));
            }
        }
    }
    Err(if tried.is_empty() { format!("no linked arcade shares {}", l.name) } else { format!("no arcade sharing {} answered ({})", l.name, tried.join("; ")) })
}

/// Die Schwierigkeits-Regel der Arcades (MAX_EASIER, arcade-chain too_easy: mit den Ausnahmen fuer Testnetze und
/// Chains, deren Arbeit nicht geprueft wird): ein Block (oder Kopf) gegen den Kopf darunter. Den Kopf darunter
/// fragt sie beim selben Teiler (roh, geprueft), wenn er nicht schon bekannt ist.
fn link_ok(peer: &str, net: &str, p: &'static ChainParams, raw_hex: &str) -> Result<(), String> {
    let raw = hex::decode(raw_hex).map_err(|_| "not hex")?;
    let h80: [u8; 80] = raw.get(..80).and_then(|x| x.try_into().ok()).ok_or("too short")?;
    let h = Header::parse(&h80);
    let id = p.block_id(&h);
    let remember = |id: Hash256, bits: u32| {
        let mut g = st().lock().unwrap();
        if g.headers.len() >= HEADERS_KEEP {
            g.headers.clear();
        }
        g.headers.insert(id, bits);
    };
    if !p.max_easier || h.prev == Hash256::ZERO {
        remember(id, h.bits);
        return Ok(());
    }
    let known = st().lock().unwrap().headers.get(&h.prev).copied();
    let parent_bits = match known {
        Some(b) => b,
        None => {
            let ask = json!([h.prev.to_hex(), false]);
            let a = ask_once(peer, net, &[("getblockheader".into(), ask.clone())])?;
            check(p, "getblockheader", &ask, &a[0])?;
            let r = a[0]["r"].as_str().ok_or("no header below")?;
            let pr = hex::decode(r).map_err(|_| "not hex")?;
            let ph: [u8; 80] = pr.get(..80).and_then(|x| x.try_into().ok()).ok_or("too short")?;
            let b = Header::parse(&ph).bits;
            remember(h.prev, b);
            b
        }
    };
    if arcade_chain::params::too_easy(p, h.bits, parent_bits) {
        return Err("its difficulty fell more than 4x in one block".into());
    }
    remember(id, h.bits);
    Ok(())
}

/// Alle TIPS_EVERY, nur fuer geliehene Chains mit mindestens zwei Teilern: alle nach dem Block an derselben Hoehe
/// fragen (etwas unter dem niedrigsten Angebot, damit alle ihn haben) und vergleichen. Abweichungen stehen in der
/// Admin-Oberflaeche und im Protokoll; wer recht hat, laesst sich daraus nicht sagen.
pub fn tips_loop() {
    loop {
        std::thread::sleep(TIPS_EVERY);
        let nets: Vec<String> = st().lock().unwrap().borrow.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect();
        for net in nets {
            let peers = sharers(&net);
            if peers.len() < 2 {
                st().lock().unwrap().tips.remove(&net);
                continue;
            }
            let low = {
                let g = st().lock().unwrap();
                peers.iter().filter_map(|p| g.offers.get(p)?.get(&net)?["h"].as_u64()).min().unwrap_or(0)
            };
            if low < 10 {
                continue;
            }
            let height = low - 3;
            let mut by: BTreeMap<String, String> = BTreeMap::new();
            for p in peers.iter().take(6) {
                if let Ok(a) = ask_once(p, &net, &[("getblockhash".into(), json!([height]))]) {
                    if let Some(h) = a[0]["r"].as_str() {
                        by.insert(p[..16].to_string(), h.to_string());
                    }
                }
            }
            let distinct: std::collections::BTreeSet<&String> = by.values().collect();
            let agree = distinct.len() <= 1;
            if !agree {
                crate::log(&format!("leihen: Teiler von {net} sind sich bei Hoehe {height} nicht einig: {by:?}"));
            }
            st().lock().unwrap().tips.insert(net.clone(), json!({"height": height, "agree": agree, "hashes": by, "at": crate::now()}));
        }
    }
}

/// Fuer die Admin-Oberflaeche
pub fn status() -> Value {
    let order: Vec<(&str, Vec<String>)> = lendable().iter().map(|l| (l.net, sharers(l.net))).collect();
    let payees: Vec<Option<(String, String)>> = lendable().iter().map(|l| payee(l.net)).collect();
    let g = st().lock().unwrap();
    let chains: Vec<Value> = lendable()
        .iter()
        .zip(order)
        .zip(payees)
        .map(|((l, (_, peers)), pay)| {
            let list: Vec<Value> = peers.iter().take(8).map(|p| json!({"peer": &p[..16], "height": g.offers[p][l.net]["h"],
                "rtt_ms": g.rtt.get(p), "failures": g.failures.get(p).copied().unwrap_or(0)})).collect();
            json!({"net": l.net, "name": l.name, "borrow": g.borrow.get(l.net).copied().unwrap_or(false), "sharers": list,
                   "mempool": g.pool.get(l.net).map_or(0, |p| p.txs.len()), "sharers_agree": g.tips.get(l.net),
                   "sharer_paid_at": pay.map(|(p, a)| json!({"peer": &p[..16], "address": a}))})
        })
        .collect();
    json!({"chains": chains, "asked": g.asked, "wrong_answers": g.refused, "pushed_unreadable": g.pushed_bad})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/src/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap().trim().to_string()
    }
    fn p(net: &str) -> &'static ChainParams {
        lendable().iter().find(|l| l.net == net).unwrap().params
    }
    fn flip(hex_s: &str, at: usize) -> String {
        let mut b = hex::decode(hex_s).unwrap();
        b[at] ^= 1;
        hex::encode(b)
    }

    #[test]
    fn real_blocks_pass_forged_fail() {
        for (net, f) in [("test", "peptest-1570000"), ("kot", "kotia-1543000")] {
            let (hash, raw) = (fixture(&format!("{f}.hash")), fixture(&format!("{f}.hex")));
            let ask = json!([hash, 0]);
            assert_eq!(check(p(net), "getblock", &ask, &json!({"r": raw})), Ok(()), "{net}: real block");
            // a changed transaction byte: merkle root no longer matches
            let n = raw.len() / 2;
            assert!(check(p(net), "getblock", &ask, &json!({"r": flip(&raw, n - 10)})).is_err(), "{net}: changed tx");
            // a changed header byte (nonce/time): not the block asked for
            assert!(check(p(net), "getblock", &ask, &json!({"r": flip(&raw, 70)})).is_err(), "{net}: changed header");
            // another block's bytes for this hash
            let other = if net == "test" { fixture("kotia-1543000.hex") } else { fixture("peptest-1570000.hex") };
            assert!(check(p(net), "getblock", &ask, &json!({"r": other})).is_err(), "{net}: other block");
        }
    }

    #[test]
    fn real_tx_passes_forged_fails() {
        let (txid, raw) = (fixture("peptest-1570000.tx0"), fixture("peptest-1570000.tx0hex"));
        let ask = json!([txid, 0]);
        assert_eq!(check(p("test"), "getrawtransaction", &ask, &json!({"r": raw})), Ok(()));
        assert!(check(p("test"), "getrawtransaction", &ask, &json!({"r": flip(&raw, 50)})).is_err());
    }

    #[test]
    fn only_checkable_forms_are_asked() {
        assert!(allowed("getblock", &json!(["00", 0])).is_ok());
        assert!(allowed("getblock", &json!(["00"])).is_ok());
        assert!(allowed("getblock", &json!(["00", 2])).is_err());
        assert!(allowed("getrawtransaction", &json!(["00", true])).is_err());
        assert!(allowed("getblockheader", &json!(["00", false])).is_ok());
        assert!(allowed("getblockheader", &json!(["00"])).is_err());
        for wallet in ["getbalance", "sendtoaddress", "dumpprivkey", "signrawtransaction", "listunspent", "stop"] {
            assert!(allowed(wallet, &json!([])).is_err(), "{wallet}");
        }
    }
}

#[cfg(test)]
mod kotia_sweep {
    use super::*;
    /// 20 real staked Kotia blocks from 1,000,001 to now (ours among them): every one passes, signature included
    #[test]
    fn real_kotia_blocks_pass() {
        let p = lendable().iter().find(|l| l.net == "kot").unwrap().params;
        let s = std::fs::read_to_string(format!("{}/src/testdata/kotia-many.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
        for line in s.lines() {
            let (h, raw) = line.split_once(' ').unwrap();
            assert_eq!(check(p, "getblock", &json!([h, 0]), &json!({"r": raw})), Ok(()), "{h}");
        }
    }
}

#[cfg(test)]
mod difficulty {
    use super::*;

    #[test]
    fn four_fold_drop_is_refused() {
        let main = lendable().iter().find(|l| l.net == "main").unwrap().params;
        let test = lendable().iter().find(|l| l.net == "test").unwrap().params;
        let kot = lendable().iter().find(|l| l.net == "kot").unwrap().params;
        // 256x easier than the block under it: refused on Pepecoin mainnet
        assert!(arcade_chain::params::too_easy(main, 0x1c0404cb, 0x1b0404cb));
        // 2x easier: fine
        assert!(!arcade_chain::params::too_easy(main, 0x1b0808cb, 0x1b0404cb));
        // a test network may drop to its easiest difficulty after a gap
        assert!(!arcade_chain::params::too_easy(test, test.pow_limit_bits, 0x1b0404cb));
        // Kotia (staked, its work is not checked): the rule is off for it
        assert!(!kot.max_easier);
    }

    #[test]
    fn link_check_uses_the_header_below() {
        // on Pepecoin mainnet (on test networks the arcades' rule lets every block down to the easiest difficulty pass)
        let p = lendable().iter().find(|l| l.net == "main").unwrap().params;
        let raw = std::fs::read_to_string(format!("{}/src/testdata/peptest-1570000.hex", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let raw = raw.trim();
        let h = Header::parse(&hex::decode(raw).unwrap()[..80].try_into().unwrap());
        // the block under it as hard as Pepecoin mainnet: this block would be far more than 4x easier
        st().lock().unwrap().headers.insert(h.prev, 0x1b0404cb);
        let r = link_ok("x".repeat(64).as_str(), "main", p, raw);
        assert!(r.is_err(), "more than 4x easier: refused");
        st().lock().unwrap().headers.insert(h.prev, h.bits);
        assert!(link_ok("x".repeat(64).as_str(), "main", p, raw).is_ok());
    }
}

/// Wohin der Anteil fuer den Teiler einer geliehenen Chain geht (fees::split_with_sharer): der Teiler, der zuerst
/// gefragt wird und eine fuer diese Chain gueltige Adresse nennt (oder der naechste). None: keiner nennt eine.
pub fn payee(net: &str) -> Option<(String, String)> {
    let order = sharers(net);
    let g = st().lock().unwrap();
    order.into_iter().find_map(|p| {
        let a = g.offers.get(&p)?.get(net)?["p"].as_str()?.to_string();
        crate::fees::valid_address(net, &a).then_some((p, a))
    })
}
