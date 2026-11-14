//! A Core-compatible JSON-RPC subset over HTTP on localhost, with Core's cookie authentication
//! (`<dir>/.cookie`, user `__cookie__`): what the arcade's own services ask a coin node.

use crate::node::{Msg, Shared};
use arcade_chain::{Block, Hash256, Tx};
use crossbeam_channel::Sender;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::Arc;

type R = Result<Value, (i64, String)>;

fn err<T>(code: i64, m: impl Into<String>) -> Result<T, (i64, String)> {
    Err((code, m.into()))
}

pub fn serve(sh: Arc<Shared>, bind: String, cookie: String, to_node: Sender<Msg>) {
    let l = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            sh.log(&format!("rpc {bind}: {e}"));
            return;
        }
    };
    sh.log(&format!("rpc on {bind}"));
    let auth = format!("Basic {}", b64(cookie.as_bytes()));
    for s in l.incoming().flatten() {
        let (sh, auth, tn) = (sh.clone(), auth.clone(), to_node.clone());
        std::thread::spawn(move || {
            let _ = handle(&sh, s, &auth, &tn);
        });
    }
}

fn b64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                o.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

fn handle(sh: &Shared, s: TcpStream, auth: &str, tn: &Sender<Msg>) -> std::io::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let (mut len, mut ok) = (0usize, false);
        // HTTP/1.0 or Connection: close: answer, then close (clients that read to the end wait for it)
        let mut close = line.contains("HTTP/1.0");
        loop {
            let mut h = String::new();
            r.read_line(&mut h)?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').unwrap_or((h, ""));
            match k.to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "authorization" => ok = v.trim() == auth,
                "connection" => close = v.trim().eq_ignore_ascii_case("close"),
                _ => {}
            }
        }
        let mut body = vec![0u8; len.min(8 << 20)];
        r.read_exact(&mut body)?;
        let (code, out) = if !ok {
            (401, String::new())
        } else {
            match serde_json::from_slice::<Value>(&body) {
                Ok(Value::Array(calls)) => (200, Value::Array(calls.iter().map(|c| one(sh, c, tn)).collect()).to_string()),
                Ok(c) => {
                    let v = one(sh, &c, tn);
                    (if v["error"].is_null() { 200 } else { 500 }, v.to_string())
                }
                Err(_) => (400, json!({"result": null, "error": {"code": -32700, "message": "Parse error"}, "id": null}).to_string()),
            }
        };
        let reason = match code { 200 => "OK", 401 => "Unauthorized", 400 => "Bad Request", _ => "Internal Server Error" };
        write!(w, "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}\r\n{out}", out.len(), if close { "Connection: close\r\n" } else { "" })?;
        w.flush()?;
        if close {
            return Ok(());
        }
    }
}

fn one(sh: &Shared, c: &Value, tn: &Sender<Msg>) -> Value {
    let id = c["id"].clone();
    let p = c["params"].as_array().cloned().unwrap_or_default();
    match call(sh, c["method"].as_str().unwrap_or(""), &p, tn) {
        Ok(v) => json!({"result": v, "error": null, "id": id}),
        Err((code, m)) => json!({"result": null, "error": {"code": code, "message": m}, "id": id}),
    }
}

fn hash_param(v: &Value) -> Result<Hash256, (i64, String)> {
    v.as_str().and_then(|s| Hash256::from_hex(s).ok()).ok_or((-8, "hash must be a 64-character hex string".into()))
}

fn verbosity(v: Option<&Value>, default: u64) -> u64 {
    match v {
        Some(Value::Bool(b)) => *b as u64,
        Some(Value::Number(n)) => n.as_u64().unwrap_or(default),
        _ => default,
    }
}

fn difficulty(bits: u32) -> f64 {
    let mut shift = (bits >> 24) & 0xff;
    let mut d = 0x0000ffff as f64 / (bits & 0x00ffffff) as f64;
    while shift < 29 {
        d *= 256.0;
        shift += 1;
    }
    while shift > 29 {
        d /= 256.0;
        shift -= 1;
    }
    d
}

fn header_json(sh: &Shared, height: u32) -> R {
    let st = sh.store;
    let (id, h) = st.header(height).ok_or((-5, "Block not found".to_string()))?;
    let tip = sh.ctip.load(Ordering::SeqCst);
    let mut v = json!({
        "hash": id.to_hex(), "confirmations": if height <= tip { (tip - height + 1) as i64 } else { -1 },
        "height": height, "version": h.version, "versionHex": format!("{:08x}", h.version),
        "merkleroot": h.merkle_root.to_hex(), "time": h.time, "nonce": h.nonce,
        "bits": format!("{:08x}", h.bits), "difficulty": difficulty(h.bits),
    });
    if height > 0 {
        v["previousblockhash"] = json!(h.prev.to_hex());
    }
    if let Some(n) = st.id_at(height + 1).filter(|_| height < sh.htip.load(Ordering::SeqCst)) {
        v["nextblockhash"] = json!(n.to_hex());
    }
    Ok(v)
}

fn height_of(sh: &Shared, id: &Hash256) -> Result<u32, (i64, String)> {
    sh.store.height_of(id).ok_or((-5, "Block not found".into()))
}

/// A transaction as Core shows it (decoderawtransaction / getrawtransaction verbose).
fn tx_json(sh: &Shared, t: &Tx) -> Value {
    let p = sh.params;
    let vin: Vec<Value> = t
        .inputs
        .iter()
        .map(|i| {
            if t.is_coinbase() {
                json!({"coinbase": hex::encode(&i.script_sig), "sequence": i.sequence})
            } else {
                json!({"txid": i.prevout.txid.to_hex(), "vout": i.prevout.vout,
                       "scriptSig": {"hex": hex::encode(&i.script_sig)}, "sequence": i.sequence})
            }
        })
        .collect();
    let vout: Vec<Value> = t
        .outputs
        .iter()
        .enumerate()
        .map(|(n, o)| {
            let mut spk = json!({"hex": hex::encode(&o.script_pubkey)});
            if let Some((kind, a)) = crate::addr::address(p, &o.script_pubkey) {
                spk["type"] = json!(kind);
                spk["addresses"] = json!([a]);
                spk["reqSigs"] = json!(1);
            } else if o.script_pubkey.first() == Some(&0x6a) {
                spk["type"] = json!("nulldata");
            } else {
                spk["type"] = json!("nonstandard");
            }
            json!({"value": o.value as f64 / 1e8, "n": n, "scriptPubKey": spk})
        })
        .collect();
    json!({"txid": t.txid.to_hex(), "hash": t.txid.to_hex(), "version": t.version, "size": t.raw.len(),
           "locktime": t.lock_time, "vin": vin, "vout": vout, "hex": hex::encode(&t.raw)})
}

/// Find a transaction by txid through the index: (raw bytes, height).
pub fn find_tx(sh: &Shared, txid: &Hash256) -> Option<(Vec<u8>, u32)> {
    let st = sh.store;
    let r = st.env.read_txn().ok()?;
    let tip = sh.ctip.load(Ordering::SeqCst);
    for item in st.txi.prefix_iter(&r, &txid.0[..8]).ok()? {
        let (k, v) = item.ok()?;
        let height = u32::from_be_bytes(k[8..12].try_into().ok()?);
        if height > tip {
            continue;
        }
        let off = u32::from_le_bytes(v[..4].try_into().ok()?);
        let len = u32::from_le_bytes(v[4..8].try_into().ok()?);
        let raw = st.block_part(height, off, len)?;
        if let Ok(t) = Tx::parse_with(&raw, &sh.params.format) {
            if t.txid == *txid {
                return Some((raw, height));
            }
        }
    }
    None
}

fn call(sh: &Shared, m: &str, p: &[Value], tn: &Sender<Msg>) -> R {
    let st = sh.store;
    let tip = sh.ctip.load(Ordering::SeqCst);
    match m {
        "getblockcount" => Ok(json!(tip)),
        "getbestblockhash" => Ok(json!(st.id_at(tip).map(|h| h.to_hex()))),
        "getblockhash" => {
            let h = p.first().and_then(|v| v.as_u64()).ok_or((-1, "height".to_string()))? as u32;
            if h > tip {
                return err(-8, "Block height out of range");
            }
            Ok(json!(st.id_at(h).map(|x| x.to_hex())))
        }
        "getblockchaininfo" => {
            let htip = sh.htip.load(Ordering::SeqCst);
            let best = st.header(tip).ok_or((-1, "no tip".to_string()))?;
            Ok(json!({
                "chain": sh.params.net, "blocks": tip, "headers": htip, "bestblockhash": best.0.to_hex(),
                "difficulty": difficulty(best.1.bits), "mediantime": best.1.time,
                "verificationprogress": if htip > 0 { tip as f64 / htip as f64 } else { 0.0 },
                "initialblockdownload": tip + 6 < htip, "pruned": false,
                "chaind": {"scripts_verified_to": sh.vtip.load(Ordering::SeqCst), "written_to": sh.flushed.load(Ordering::SeqCst)},
            }))
        }
        "getblockheader" => {
            let id = hash_param(p.first().unwrap_or(&Value::Null))?;
            let h = height_of(sh, &id)?;
            if verbosity(p.get(1), 1) == 0 {
                return Ok(json!(hex::encode(st.header(h).unwrap().1.encode())));
            }
            header_json(sh, h)
        }
        "getblock" => {
            let id = hash_param(p.first().unwrap_or(&Value::Null))?;
            let h = height_of(sh, &id)?;
            if h > tip {
                return err(-5, "Block not available yet");
            }
            let raw = st.block(h).ok_or((-5, "Block not found".to_string()))?;
            match verbosity(p.get(1), 1) {
                0 => Ok(json!(hex::encode(&raw))),
                v => {
                    let b = Block::parse_with(&raw, &sh.params.format).map_err(|e| (-22, e.to_string()))?;
                    let mut j = header_json(sh, h)?;
                    j["size"] = json!(raw.len());
                    j["tx"] = if v >= 2 { json!(b.txs.iter().map(|t| tx_json(sh, t)).collect::<Vec<_>>()) } else { json!(b.txs.iter().map(|t| t.txid.to_hex()).collect::<Vec<_>>()) };
                    Ok(j)
                }
            }
        }
        "getrawtransaction" => {
            let txid = hash_param(p.first().unwrap_or(&Value::Null))?;
            let mem = sh.mempool.lock().unwrap().txs.get(&txid).map(|t| t.tx.clone());
            if let Some(t) = mem {
                if verbosity(p.get(1), 0) == 0 {
                    return Ok(json!(hex::encode(&t.raw)));
                }
                return Ok(tx_json(sh, &t));
            }
            let (raw, h) = find_tx(sh, &txid).ok_or((-5, "No such mempool or blockchain transaction. Use gettransaction for wallet transactions.".to_string()))?;
            if verbosity(p.get(1), 0) == 0 {
                return Ok(json!(hex::encode(&raw)));
            }
            let t = Tx::parse_with(&raw, &sh.params.format).map_err(|e| (-22, e.to_string()))?;
            let mut j = tx_json(sh, &t);
            let (id, hd) = st.header(h).unwrap();
            j["blockhash"] = json!(id.to_hex());
            j["confirmations"] = json!(tip - h + 1);
            j["time"] = json!(hd.time);
            j["blocktime"] = json!(hd.time);
            Ok(j)
        }
        "decoderawtransaction" => {
            let raw = p.first().and_then(|v| v.as_str()).and_then(|s| hex::decode(s).ok()).ok_or((-22, "TX decode failed".to_string()))?;
            let t = Tx::parse_with(&raw, &sh.params.format).map_err(|_| (-22, "TX decode failed".to_string()))?;
            Ok(tx_json(sh, &t))
        }
        "getrawmempool" => {
            let mp = sh.mempool.lock().unwrap();
            if verbosity(p.first(), 0) == 0 {
                return Ok(json!(mp.order.iter().map(|t| t.to_hex()).collect::<Vec<_>>()));
            }
            let mut o = serde_json::Map::new();
            for id in &mp.order {
                let t = &mp.txs[id];
                o.insert(id.to_hex(), json!({"size": t.tx.raw.len(), "fee": t.fee as f64 / 1e8, "time": t.time, "height": tip}));
            }
            Ok(Value::Object(o))
        }
        "getmempoolinfo" => {
            let mp = sh.mempool.lock().unwrap();
            Ok(json!({"size": mp.txs.len(), "bytes": mp.txs.values().map(|t| t.tx.raw.len()).sum::<usize>(), "mempoolminfee": 0.0}))
        }
        "getnetworkinfo" => Ok(json!({"version": 10000, "subversion": "/chaind:0.1.0/", "protocolversion": sh.params.protocol_version,
            "connections": sh.relay.lock().unwrap().len(), "relayfee": crate::mempool::MIN_FEE_PER_KB as f64 / 1e8})),
        "sendrawtransaction" => {
            let raw = p.first().and_then(|v| v.as_str()).and_then(|s| hex::decode(s).ok()).ok_or((-22, "TX decode failed".to_string()))?;
            let txid = crate::mempool::accept(sh, &raw).map_err(|e| (-26, e))?;
            crate::relay::announce(sh, txid);
            Ok(json!(txid.to_hex()))
        }
        "getbalance" => {
            let w = sh.wallet.lock().unwrap();
            let w = w.as_ref().ok_or((-18, "No wallet is loaded".to_string()))?;
            Ok(json!(w.balance(tip, sh.params).0 as f64 / 1e8))
        }
        "getwalletinfo" => {
            let w = sh.wallet.lock().unwrap();
            let w = w.as_ref().ok_or((-18, "No wallet is loaded".to_string()))?;
            let (ok, imm) = w.balance(tip, sh.params);
            Ok(json!({"walletname": "chaind", "balance": ok as f64 / 1e8, "immature_balance": imm as f64 / 1e8, "unconfirmed_balance": 0.0,
                      "txcount": w.coins.len(), "keypoolsize": w.keys()}))
        }
        "sendtoaddress" => {
            let addr = p.first().and_then(|v| v.as_str()).ok_or((-5, "Invalid address".to_string()))?;
            let script = crate::wallet::address_script(sh.params, addr).ok_or((-5, "Invalid address".to_string()))?;
            let amount = p.get(1).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).ok_or((-3, "Invalid amount".to_string()))?;
            if !(amount > 0.0 && amount < 1e9) {
                return err(-3, "Invalid amount");
            }
            let sats = (amount * 1e8).round() as u64;
            let mut w = sh.wallet.lock().unwrap();
            let w = w.as_mut().ok_or((-18, "No wallet is loaded".to_string()))?;
            let (raw, used) = w.pay(sh.params, tip, script, sats).map_err(|e| (-6, e))?;
            let txid = crate::mempool::accept(sh, &raw).map_err(|e| (-4, format!("Transaction rejected: {e}")))?;
            w.lock(&used);
            crate::relay::announce(sh, txid);
            sh.log(&format!("sent {amount} to {addr}: {}", txid.to_hex()));
            Ok(json!(txid.to_hex()))
        }
        "gettxoutsetinfo" => {
            // write the coins out first, so the set read is that of one block
            let (s, r) = crossbeam_channel::bounded(1);
            let _ = tn.send(Msg::Flush(s));
            let _ = r.recv_timeout(std::time::Duration::from_secs(600));
            let s = crate::utxo::stats(st).map_err(|e| (-1, e))?;
            Ok(json!({"height": s.height, "bestblock": s.best.to_hex(), "transactions": s.transactions, "txouts": s.txouts,
                      "hash_serialized": s.hash.to_hex(), "total_amount": s.total as f64 / 1e8}))
        }
        "uptime" => Ok(json!(sh.started.elapsed().as_secs())),
        "stop" => {
            sh.stop.store(true, Ordering::SeqCst);
            sh.wake();
            Ok(json!("chaind stopping"))
        }
        _ => err(-32601, "Method not found"),
    }
}
