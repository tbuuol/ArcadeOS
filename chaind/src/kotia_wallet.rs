//! Kotia's wallet in chaind: what the box's services ask kotiad's wallet today (the Freigabe-Fach's outboxd and
//! the monitor), answered from chaind's own coin set and blocks, so that kotiad can go.
//!
//! Only for Kotia, and only when CHAIND_KOTIA_WALLET names a key file (dumpwallet format: the WIF is the first word
//! of a line, `label=` gives the account; # lines skipped). The file stays as it is and nothing of it is logged or
//! answered: no keys, only addresses, labels and counts. This is its own module; the faucet wallet (wallet.rs) is a
//! different one and stays untouched.
//!
//! Each key owns two scripts, as in Kotia Core: P2PKH (its address) and P2PK (what stakes pay back to).
//!
//! The coins are the chain's: every unspent output of a wallet script. (kotiad's wallet can hide some, when its
//! wallet.dat still holds an old stake that never made it into the chain and "spent" them; compared 2026-11-16:
//! 8 such coins, all unspent by kotiad's own gettxout.)
//!
//! RPCs (Kotia Core's names and answer shapes, the subset the box uses):
//! getbalance, getwalletinfo, listunspent, listtransactions, validateaddress, createrawtransaction,
//! signrawtransaction. sendrawtransaction and decoderawtransaction are chaind's own (rpc.rs, mempool.rs).
//! Nothing here sends: signrawtransaction only signs; a transaction leaves only through sendrawtransaction.

use crate::node::Shared;
use crate::stakeblock::{p2pk, p2pkh};
use crate::store::{decode_undo, outkey, Coin, OutKey};
use arcade_chain::hash::sha256d;
use arcade_chain::params::ChainParams;
use arcade_chain::{Block, Hash256, OutPoint, Tx, TxIn, TxOut};
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

type R = Result<Value, (i64, String)>;

/// how far back listtransactions looks at most (blocks)
const HISTORY_BLOCKS: u32 = 100_000;

struct Key {
    sk: SigningKey,
    pubkey: Vec<u8>,
    label: String,
    address: String,
}

pub struct KotiaWallet {
    keys: Vec<Key>,
    by_script: HashMap<Vec<u8>, usize>,
    /// the wallet's coins at a tip (read again when the tip moves)
    coins: Mutex<Option<(u32, Hash256, Vec<(OutKey, Coin)>)>>,
}

static WALLET: OnceLock<KotiaWallet> = OnceLock::new();

/// `label=` of a dumpwallet line, %-escapes undone.
fn label_of(line: &str) -> String {
    let Some(v) = line.split_whitespace().find_map(|w| w.strip_prefix("label=")) else { return String::new() };
    let b = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(x) = u8::from_str_radix(&v[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read the keys. Returns (keys, labelled keys) for the log.
pub fn load(p: &'static ChainParams, path: &str) -> Result<(usize, usize), String> {
    if !p.format.tx_time {
        return Err("kotia wallet: only for Kotia".into());
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("kotia wallet {path}: {e}"))?;
    let mut w = KotiaWallet { keys: Vec::new(), by_script: HashMap::new(), coins: Mutex::new(None) };
    for line in text.lines() {
        let word = line.split_whitespace().next().unwrap_or("");
        if word.is_empty() || word.starts_with('#') {
            continue;
        }
        let Some(d) = crate::wallet::base58_decode(word) else { continue };
        if d.first() != Some(&p.wif_version) || !(d.len() == 33 || d.len() == 34 && d[33] == 1) {
            continue;
        }
        let Ok(sk) = SigningKey::from_slice(&d[1..33]) else { continue };
        let pubkey = sk.verifying_key().to_encoded_point(d.len() == 34).as_bytes().to_vec();
        if w.by_script.contains_key(&p2pkh(&pubkey)) {
            continue;
        }
        let address = crate::addr::address(p, &p2pkh(&pubkey)).map(|x| x.1).unwrap_or_default();
        let i = w.keys.len();
        w.by_script.insert(p2pkh(&pubkey), i);
        w.by_script.insert(p2pk(&pubkey), i);
        w.keys.push(Key { sk, pubkey, label: label_of(line), address });
    }
    if w.keys.is_empty() {
        return Err(format!("kotia wallet {path}: no Kotia keys in it"));
    }
    let n = (w.keys.len(), w.keys.iter().filter(|k| !k.label.is_empty()).count());
    WALLET.set(w).map_err(|_| "kotia wallet: loaded twice".to_string())?;
    Ok(n)
}

fn amount(v: u64) -> Value {
    json!(v as f64 / 1e8)
}

fn parse_amount(v: &Value) -> Option<u64> {
    let x = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))?;
    (x >= 0.0 && x < 1e11).then(|| (x * 1e8).round() as u64)
}

fn now() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0)
}

impl KotiaWallet {
    fn key(&self, script: &[u8]) -> Option<&Key> {
        self.by_script.get(script).map(|&i| &self.keys[i])
    }

    /// The wallet's coins on the chain now (read from the coin set once per tip).
    fn coins(&self, sh: &Shared) -> Result<(u32, Vec<(OutKey, Coin)>), (i64, String)> {
        let st = sh.store;
        let tip = sh.ctip.load(Ordering::SeqCst);
        let id = st.id_at(tip).unwrap_or(Hash256::ZERO);
        let mut c = self.coins.lock().unwrap();
        if let Some((t, i, v)) = c.as_ref() {
            if *t == tip && *i == id {
                return Ok((tip, v.clone()));
            }
        }
        let r = st.env.read_txn().map_err(|e| (-1, e.to_string()))?;
        let mut v = Vec::new();
        for item in st.coins.iter(&r).map_err(|e| (-1, e.to_string()))? {
            let (k, b) = item.map_err(|e| (-1, e.to_string()))?;
            if let Some(coin) = Coin::decode(b) {
                if self.by_script.contains_key(&coin.script) {
                    v.push((k.try_into().unwrap(), coin));
                }
            }
        }
        *c = Some((tip, id, v.clone()));
        Ok((tip, v))
    }
}

/// Kotia Core's wallet: a stake or coinbase is spendable once it is deeper than the maturity (depth > maturity).
fn mature(p: &ChainParams, tip: u32, c: &Coin) -> bool {
    let m = p.value_rules.map_or(0, |v| (v.maturity)(c.height));
    !c.coinbase || tip + 1 > c.height + m
}

fn outkey_parts(k: &OutKey) -> (Hash256, u32) {
    (Hash256(k[..32].try_into().unwrap()), u32::from_be_bytes(k[32..].try_into().unwrap()))
}

/// The RPCs of Kotia's wallet; None when no Kotia wallet is loaded or the method is not one of them.
pub fn call(sh: &Shared, m: &str, p: &[Value]) -> Option<R> {
    let w = WALLET.get()?;
    Some(match m {
        "getbalance" => balances(sh, w).map(|b| amount(b.0)),
        "getwalletinfo" => balances(sh, w).map(|(ok, staked, n)| {
            json!({"walletname": "chaind", "walletversion": 130000, "total_balance": amount(ok + staked), "balance": amount(ok),
                   "staked_balance": amount(staked), "unconfirmed_balance": 0.0, "immature_balance": 0.0,
                   "txcount": n, "keypoolsize": w.keys.len()})
        }),
        "listunspent" => listunspent(sh, w, p),
        "listtransactions" => listtransactions(sh, w, p),
        "validateaddress" => Ok(validateaddress(sh, w, p)),
        "createrawtransaction" => createrawtransaction(sh, p),
        "signrawtransaction" => signrawtransaction(sh, w, p),
        _ => return None,
    })
}

/// (spendable, staked = not yet mature, number of coins)
fn balances(sh: &Shared, w: &KotiaWallet) -> Result<(u64, u64, usize), (i64, String)> {
    let (tip, coins) = w.coins(sh)?;
    let (mut ok, mut staked) = (0, 0);
    for (_, c) in &coins {
        if mature(sh.params, tip, c) {
            ok += c.value;
        } else {
            staked += c.value;
        }
    }
    Ok((ok, staked, coins.len()))
}

fn listunspent(sh: &Shared, w: &KotiaWallet, p: &[Value]) -> R {
    let minconf = p.first().and_then(|v| v.as_u64()).unwrap_or(1);
    let maxconf = p.get(1).and_then(|v| v.as_u64()).unwrap_or(9_999_999);
    let only: Option<Vec<String>> = match p.get(2) {
        Some(Value::Array(a)) => {
            let mut v = Vec::new();
            for x in a {
                let s = x.as_str().ok_or((-5, "Invalid Kotia address".to_string()))?;
                crate::wallet::address_script(sh.params, s).ok_or((-5, format!("Invalid Kotia address: {s}")))?;
                v.push(s.to_string());
            }
            Some(v)
        }
        Some(Value::Null) | None => None,
        Some(_) => return Err((-8, "addresses must be an array".into())),
    };
    let (tip, coins) = w.coins(sh)?;
    let mp = sh.mempool.lock().unwrap();
    let mut out = Vec::new();
    for (k, c) in &coins {
        let conf = (tip - c.height + 1) as u64;
        if conf < minconf || conf > maxconf || !mature(sh.params, tip, c) || mp.spends(k).is_some() {
            continue;
        }
        let key = w.key(&c.script).unwrap();
        if only.as_ref().is_some_and(|a| !a.contains(&key.address)) {
            continue;
        }
        let (txid, vout) = outkey_parts(k);
        out.push(json!({"txid": txid.to_hex(), "vout": vout, "address": key.address, "account": key.label,
                        "scriptPubKey": hex::encode(&c.script), "amount": amount(c.value), "confirmations": conf,
                        "spendable": true, "solvable": true}));
    }
    Ok(Value::Array(out))
}

fn validateaddress(sh: &Shared, w: &KotiaWallet, p: &[Value]) -> Value {
    let a = p.first().and_then(|v| v.as_str()).unwrap_or("");
    let Some(script) = crate::wallet::address_script(sh.params, a) else { return json!({"isvalid": false}) };
    let mut v = json!({"isvalid": true, "address": a, "scriptPubKey": hex::encode(&script), "ismine": false,
                       "iswatchonly": false, "isscript": script.first() == Some(&0xa9)});
    if let Some(k) = w.key(&script) {
        v["ismine"] = json!(true);
        v["pubkey"] = json!(hex::encode(&k.pubkey));
        v["iscompressed"] = json!(k.pubkey.len() == 33);
        v["account"] = json!(k.label);
    }
    v
}

fn push_data(s: &mut Vec<u8>, d: &[u8]) {
    match d.len() {
        n if n < 0x4c => s.push(n as u8),
        n if n <= 0xff => s.extend([0x4c, n as u8]),
        n => {
            s.push(0x4d);
            s.extend((n as u16).to_le_bytes());
        }
    }
    s.extend_from_slice(d);
}

/// As Kotia Core: version 1, the time now, the inputs unsigned (sequence max, or max-1 with a lock time), the
/// outputs in the order given (an address with its amount; "data" as an OP_RETURN of the hex).
fn createrawtransaction(sh: &Shared, p: &[Value]) -> R {
    let ins = p.first().and_then(|v| v.as_array()).ok_or((-8, "Invalid parameter, inputs must be an array".to_string()))?;
    let outs = p.get(1).and_then(|v| v.as_object()).ok_or((-8, "Invalid parameter, outputs must be an object".to_string()))?;
    let lock_time = p.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let seq = if lock_time > 0 { u32::MAX - 1 } else { u32::MAX };
    let mut inputs = Vec::new();
    for i in ins {
        let txid = i["txid"].as_str().and_then(|s| Hash256::from_hex(s).ok()).ok_or((-8, "Invalid parameter, expected txid".to_string()))?;
        let vout = i["vout"].as_u64().filter(|v| *v <= u32::MAX as u64).ok_or((-8, "Invalid parameter, missing vout key".to_string()))? as u32;
        let sequence = i["sequence"].as_u64().map(|s| s as u32).unwrap_or(seq);
        inputs.push(TxIn { prevout: OutPoint { txid, vout }, script_sig: Vec::new(), sequence, witness: Vec::new() });
    }
    let mut outputs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (k, v) in outs {
        if k == "data" {
            let d = v.as_str().and_then(|s| hex::decode(s).ok()).ok_or((-8, "Data must be hexadecimal string".to_string()))?;
            let mut s = vec![0x6a];
            push_data(&mut s, &d);
            outputs.push(TxOut { value: 0, script_pubkey: s });
            continue;
        }
        let script = crate::wallet::address_script(sh.params, k).ok_or((-5, format!("Invalid Kotia address: {k}")))?;
        if !seen.insert(k.clone()) {
            return Err((-8, format!("Invalid parameter, duplicated address: {k}")));
        }
        let value = parse_amount(v).ok_or((-3, "Invalid amount".to_string()))?;
        outputs.push(TxOut { value, script_pubkey: script });
    }
    let t = Tx { version: 1, time: Some(now()), inputs, outputs, lock_time, txid: Hash256::ZERO, raw: Vec::new(), hogex: false };
    Ok(json!(hex::encode(t.encode_legacy())))
}

/// Sign every input whose coin is ours (legacy SIGHASH_ALL; Kotia's tx time is part of what is signed), then check
/// the whole transaction's scripts as the next block would. Only the plain form is supported (no extra coins, keys
/// or sighash types). Nothing is sent.
fn signrawtransaction(sh: &Shared, w: &KotiaWallet, p: &[Value]) -> R {
    if p.iter().skip(1).any(|v| !v.is_null()) {
        return Err((-8, "chaind: only signrawtransaction \"hex\" is supported (the wallet's own coins, SIGHASH_ALL)".into()));
    }
    let raw = p.first().and_then(|v| v.as_str()).and_then(|s| hex::decode(s).ok()).ok_or((-22, "TX decode failed".to_string()))?;
    let mut tx = Tx::parse_with(&raw, &sh.params.format).map_err(|_| (-22, "TX decode failed".to_string()))?;
    if tx.time.is_none() {
        return Err((-22, "TX decode failed (no time)".into()));
    }
    let st = sh.store;
    let tip = sh.ctip.load(Ordering::SeqCst);
    // the coins spent: from the chain or the mempool
    let mut spent: Vec<Option<Coin>> = Vec::new();
    {
        let mp = sh.mempool.lock().unwrap();
        let r = st.env.read_txn().map_err(|e| (-1, e.to_string()))?;
        for i in &tx.inputs {
            let k = outkey(&i.prevout.txid, i.prevout.vout);
            spent.push(mp.coin(&k).or_else(|| st.coins.get(&r, &k).ok().flatten().and_then(Coin::decode)));
        }
    }
    let mut errors = Vec::new();
    let mut sigs: Vec<Option<Vec<u8>>> = vec![None; tx.inputs.len()];
    for (n, c) in spent.iter().enumerate() {
        let i = &tx.inputs[n];
        let fail = |why: &str| json!({"txid": i.prevout.txid.to_hex(), "vout": i.prevout.vout, "scriptSig": hex::encode(&i.script_sig), "sequence": i.sequence, "error": why});
        let Some(c) = c else {
            errors.push(fail("Input not found or already spent"));
            continue;
        };
        let Some(key) = w.key(&c.script) else {
            errors.push(fail("Unable to sign input, the coin is not the wallet's"));
            continue;
        };
        let mut pre = tx.clone();
        for (j, x) in pre.inputs.iter_mut().enumerate() {
            x.script_sig = if j == n { c.script.clone() } else { Vec::new() };
        }
        let mut b = pre.encode_legacy();
        b.extend(1u32.to_le_bytes()); // SIGHASH_ALL
        let d = sha256d(&b);
        let sig: Signature = key.sk.sign_prehash(&d.0).map_err(|e| (-1, e.to_string()))?;
        let sig = sig.normalize_s().unwrap_or(sig);
        let mut der = sig.to_der().as_bytes().to_vec();
        der.push(1);
        let mut ss = Vec::new();
        push_data(&mut ss, &der);
        if c.script != p2pk(&key.pubkey) {
            push_data(&mut ss, &key.pubkey);
        }
        sigs[n] = Some(ss);
    }
    for (n, s) in sigs.into_iter().enumerate() {
        if let Some(s) = s {
            tx.inputs[n].script_sig = s;
        }
    }
    let hex_out = tx.encode_legacy();
    let mut complete = errors.is_empty();
    if complete {
        // check it as the next block would
        let t = Tx::parse_with(&hex_out, &sh.params.format).map_err(|_| (-22, "TX decode failed".to_string()))?;
        if let Some(flags) = crate::rules::script_flags(sh.params, tip + 1, true) {
            let sp: Vec<crate::rules::Spent> = spent.iter().flatten()
                .map(|c| crate::rules::Spent { value: c.value, script: &c.script, height: c.height.min(tip), coinbase: c.coinbase, pegout: c.pegout })
                .collect();
            if let Err(e) = crate::rules::tx_scripts(sh.params, &t, &sp, tip + 1, flags) {
                complete = false;
                errors.push(json!({"error": format!("signed, but the check failed: {}", e.why)}));
            }
        }
    }
    let mut v = json!({"hex": hex::encode(&hex_out), "complete": complete});
    if !errors.is_empty() {
        v["errors"] = Value::Array(errors);
    }
    Ok(v)
}

/// Kotia Core's listtransactions ("*" or an account, count, skip): the wallet's entries of the blocks, oldest first,
/// the newest `count` after skipping the newest `skip`. A stake shows as Kotia Core shows it: a receive of what came
/// back and a send of what was staked, both "generated", the send's fee = the stake reward. Only confirmed
/// transactions; at most HISTORY_BLOCKS blocks back.
fn listtransactions(sh: &Shared, w: &KotiaWallet, p: &[Value]) -> R {
    let account = p.first().and_then(|v| v.as_str()).unwrap_or("*").to_string();
    let count = p.get(1).and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let skip = p.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let st = sh.store;
    let params = sh.params;
    let tip = sh.ctip.load(Ordering::SeqCst);
    let want = count + skip;
    let mut rev: Vec<Value> = Vec::new(); // newest first
    let mut h = tip;
    while rev.len() < want && h > 0 && tip - h < HISTORY_BLOCKS {
        let Some(raw) = st.block(h) else { break };
        let b = Block::parse_with(&raw, &params.format).map_err(|e| (-1, e.to_string()))?;
        let spent: HashMap<OutKey, Coin> = st.undo(h).and_then(|u| decode_undo(&u)).unwrap_or_default().into_iter().collect();
        let mut made: HashMap<OutKey, (u64, Vec<u8>)> = HashMap::new(); // outputs of this block, for spends inside it
        let mut entries = Vec::new();
        let (id, hd) = st.header(h).ok_or((-1, "header".to_string()))?;
        for (bi, t) in b.txs.iter().enumerate() {
            let mut in_all = 0u64;
            let mut in_mine = 0u64;
            let mut all_mine = !t.is_coinbase();
            if !t.is_coinbase() {
                for i in &t.inputs {
                    let k = outkey(&i.prevout.txid, i.prevout.vout);
                    let (v, s) = match (spent.get(&k), made.get(&k)) {
                        (Some(c), _) => (c.value, c.script.clone()),
                        (None, Some((v, s))) => (*v, s.clone()),
                        _ => (0, Vec::new()),
                    };
                    in_all += v;
                    if w.by_script.contains_key(&s) {
                        in_mine += v;
                    } else {
                        all_mine = false;
                    }
                }
            }
            for (n, o) in t.outputs.iter().enumerate() {
                made.insert(outkey(&t.txid, n as u32), (o.value, o.script_pubkey.clone()));
            }
            let out_all: u64 = t.outputs.iter().map(|o| o.value).sum();
            let mine: Vec<(usize, &TxOut, &Key)> = t.outputs.iter().enumerate().filter_map(|(n, o)| w.key(&o.script_pubkey).map(|k| (n, o, k))).collect();
            if in_mine == 0 && mine.is_empty() {
                continue;
            }
            let conf = tip - h + 1;
            let time = t.time.unwrap_or(hd.time);
            let base = |e: Value| -> Value {
                let mut e = e;
                e["confirmations"] = json!(conf);
                e["blockhash"] = json!(id.to_hex());
                e["blockindex"] = json!(bi);
                e["blocktime"] = json!(hd.time);
                e["txid"] = json!(t.txid.to_hex());
                e["walletconflicts"] = json!([]);
                e["time"] = json!(time);
                e["timereceived"] = json!(time);
                e
            };
            if t.is_coinstake() && in_mine > 0 {
                let reward = out_all.saturating_sub(in_all);
                // as Kotia Core: per output back (a split stake has two) a receive and a send (fee = the reward),
                // then a send of the empty first output
                for (n, o, k) in &mine {
                    entries.push(base(json!({"account": k.label, "address": k.address, "category": "receive", "amount": amount(o.value),
                                             "label": k.label, "vout": n, "generated": true})));
                }
                for (n, o, k) in &mine {
                    entries.push(base(json!({"account": "", "address": k.address, "category": "send", "amount": -(o.value as f64) / 1e8,
                                             "label": k.label, "vout": n, "fee": amount(reward), "generated": true, "abandoned": false})));
                }
                entries.push(base(json!({"account": "", "category": "send", "amount": 0.0, "vout": 0, "fee": amount(reward),
                                         "generated": true, "abandoned": false})));
            } else if t.is_coinbase() {
                for (n, o, k) in &mine {
                    let cat = if conf > params.value_rules.map_or(0, |v| (v.maturity)(h)) { "generate" } else { "immature" };
                    entries.push(base(json!({"account": k.label, "address": k.address, "category": cat, "amount": amount(o.value),
                                             "label": k.label, "vout": n, "generated": true})));
                }
            } else if in_mine > 0 {
                let fee = if all_mine { in_all.saturating_sub(out_all) } else { 0 };
                for (n, o) in t.outputs.iter().enumerate() {
                    if w.by_script.contains_key(&o.script_pubkey) {
                        continue; // change
                    }
                    let mut e = json!({"account": "", "category": "send", "amount": -(o.value as f64) / 1e8, "vout": n,
                                       "fee": -(fee as f64) / 1e8, "abandoned": false});
                    if let Some((_, a)) = crate::addr::address(params, &o.script_pubkey) {
                        e["address"] = json!(a);
                    }
                    entries.push(base(e));
                }
            } else {
                for (n, o, k) in &mine {
                    entries.push(base(json!({"account": k.label, "address": k.address, "category": "receive", "amount": amount(o.value),
                                             "label": k.label, "vout": n})));
                }
            }
        }
        if account != "*" {
            entries.retain(|e| e["account"] == account.as_str());
        }
        rev.extend(entries.into_iter().rev());
        h -= 1;
    }
    let mut out: Vec<Value> = rev.into_iter().skip(skip).take(count).collect();
    out.reverse();
    Ok(Value::Array(out))
}
