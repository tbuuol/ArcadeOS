//! Staying in the conversation of the coin's network: a couple of lasting peer connections that hear about new
//! transactions and blocks (inv), fetch the transactions, answer requests for ours, and carry ours out.

use crate::node::Shared;
use arcade_chain::p2p::{encode_message, parse_version, read_message, version_payload, MSG_BLOCK, MSG_TX};
use arcade_chain::params::ChainParams;
use arcade_chain::Hash256;
use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub type Writer = Arc<Mutex<TcpStream>>;

fn send(w: &Writer, magic: [u8; 4], cmd: &str, payload: &[u8]) -> bool {
    w.lock().unwrap().write_all(&encode_message(magic, cmd, payload)).is_ok()
}

fn inv_payload(kind: u32, ids: &[Hash256]) -> Vec<u8> {
    let mut p = Vec::new();
    arcade_chain::encode::write_varint(&mut p, ids.len() as u64);
    for id in ids {
        p.extend_from_slice(&kind.to_le_bytes());
        p.extend_from_slice(&id.0);
    }
    p
}

fn parse_inv(b: &[u8]) -> Vec<(u32, Hash256)> {
    let (n, mut i) = match b.first() {
        Some(&x) if x < 0xfd => (x as usize, 1),
        Some(&0xfd) if b.len() >= 3 => (u16::from_le_bytes([b[1], b[2]]) as usize, 3),
        _ => return vec![],
    };
    let mut v = Vec::with_capacity(n.min(50_000));
    for _ in 0..n {
        let Some(e) = b.get(i..i + 36) else { break };
        v.push((u32::from_le_bytes(e[..4].try_into().unwrap()), Hash256(e[4..].try_into().unwrap())));
        i += 36;
    }
    v
}

/// Tell the relay peers about a transaction of ours.
pub fn announce(sh: &Shared, txid: Hash256) {
    let ws: Vec<Writer> = sh.relay.lock().unwrap().clone();
    for w in ws {
        send(&w, sh.params.magic, "inv", &inv_payload(MSG_TX, &[txid]));
    }
}

pub fn relay_thread(params: &'static ChainParams, sh: Arc<Shared>, no: usize) {
    let mut start = no;
    while !sh.stop.load(Ordering::Relaxed) {
        let peers = sh.peers();
        let addr = peers[start % peers.len()].clone();
        start += 1;
        if let Err(e) = session(params, &sh, &addr) {
            sh.log(&format!("relay {addr}: {e}"));
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

fn session(params: &'static ChainParams, sh: &Shared, addr: &str) -> Result<(), String> {
    let sa = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
    let mut s = TcpStream::connect_timeout(&sa, Duration::from_secs(20)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(600))).ok();
    let magic = params.magic;
    let tip = sh.ctip.load(Ordering::SeqCst) as i32;
    s.write_all(&encode_message(magic, "version", &version_payload(params, Some(sa), tip, true))).map_err(|e| e.to_string())?;
    let (mut got_v, mut got_ack) = (false, false);
    while !got_v || !got_ack {
        let m = read_message(&mut s, magic).map_err(|e| e.to_string())?;
        match m.command.as_str() {
            "version" => {
                parse_version(&m.payload).map_err(|e| e.to_string())?;
                got_v = true;
                s.write_all(&encode_message(magic, "verack", &[])).map_err(|e| e.to_string())?;
            }
            "verack" => got_ack = true,
            _ => {}
        }
    }
    let w: Writer = Arc::new(Mutex::new(s.try_clone().map_err(|e| e.to_string())?));
    sh.relay.lock().unwrap().push(w.clone());
    // ask for what is in its mempool once, to start with the same unconfirmed transactions
    send(&w, magic, "mempool", &[]);
    let r = (|| -> Result<(), String> {
        loop {
            if sh.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let m = read_message(&mut s, magic).map_err(|e| e.to_string())?;
            match m.command.as_str() {
                "ping" => {
                    send(&w, magic, "pong", &m.payload);
                }
                "inv" => {
                    let mut want = Vec::new();
                    for (kind, id) in parse_inv(&m.payload) {
                        if kind == MSG_TX && !sh.mempool.lock().unwrap().txs.contains_key(&id) {
                            want.push(id);
                        } else if kind == MSG_BLOCK {
                            sh.block_hint.store(true, Ordering::SeqCst);
                        }
                    }
                    if !want.is_empty() && sh.ctip.load(Ordering::SeqCst) + 6 >= sh.htip.load(Ordering::SeqCst) {
                        send(&w, magic, "getdata", &inv_payload(MSG_TX, &want));
                    }
                }
                "tx" => {
                    let _ = crate::mempool::accept(sh, &m.payload);
                }
                "getdata" => {
                    for (kind, id) in parse_inv(&m.payload) {
                        if kind == MSG_TX {
                            let raw = sh.mempool.lock().unwrap().txs.get(&id).map(|t| t.tx.raw.clone());
                            if let Some(raw) = raw {
                                send(&w, magic, "tx", &raw);
                            }
                        }
                    }
                }
                "headers" | "block" => sh.block_hint.store(true, Ordering::SeqCst),
                _ => {}
            }
        }
    })();
    sh.relay.lock().unwrap().retain(|x| !Arc::ptr_eq(x, &w));
    r
}
