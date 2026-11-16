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
}

fn st() -> &'static Mutex<St> {
    static S: OnceLock<Mutex<St>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(St { next_id: 1, ..Default::default() }))
}

pub fn set_settings(borrow: BTreeMap<String, bool>) {
    st().lock().unwrap().borrow = borrow;
}

pub fn settings_from(v: &Value) -> BTreeMap<String, bool> {
    lendable().iter().map(|l| (l.net.to_string(), v["borrow"][l.net].as_bool() == Some(true))).collect()
}

// ------------------------------------------------------------------ was meshd meldet (aus share.rs)

pub fn on_offer(peer: &str, nets: &Value) {
    let mut g = st().lock().unwrap();
    match nets.as_object() {
        Some(m) if !m.is_empty() => {
            g.offers.insert(peer.to_string(), m.iter().filter(|(_, v)| v.is_object()).map(|(k, v)| (k.clone(), v.clone())).collect());
        }
        _ => {
            g.offers.remove(peer);
        }
    }
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
    for (m, p) in calls {
        allowed(m, p)?;
    }
    let mut tried = Vec::new();
    for peer in sharers(net) {
        match ask_once(&peer, net, calls) {
            Ok(answers) => {
                let bad = calls.iter().zip(&answers).find_map(|((m, p), a)| check(l.params, m, p, a).err().map(|e| format!("{m}: {e}")));
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

/// Fuer die Admin-Oberflaeche
pub fn status() -> Value {
    let order: Vec<(&str, Vec<String>)> = lendable().iter().map(|l| (l.net, sharers(l.net))).collect();
    let g = st().lock().unwrap();
    let chains: Vec<Value> = lendable()
        .iter()
        .zip(order)
        .map(|(l, (_, peers))| {
            let list: Vec<Value> = peers.iter().take(8).map(|p| json!({"peer": &p[..16], "height": g.offers[p][l.net]["h"],
                "rtt_ms": g.rtt.get(p), "failures": g.failures.get(p).copied().unwrap_or(0)})).collect();
            json!({"net": l.net, "name": l.name, "borrow": g.borrow.get(l.net).copied().unwrap_or(false), "sharers": list})
        })
        .collect();
    json!({"chains": chains, "asked": g.asked, "wrong_answers": g.refused})
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
