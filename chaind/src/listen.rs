//! Incoming peers (a seed node): others connect to us and download the chain. ONLY block data goes out, nothing
//! comes in:
//!
//! - answered: version/verack, ping/pong, getheaders (headers), getblocks (inv of block ids), getdata for blocks
//!   (the block as stored; anything else: notfound), getaddr (the peers we know).
//! - new blocks of ours are announced to them (inv), so they can fetch them.
//! - everything else is ignored: an incoming peer's transactions (tx, inv of txs, mempool) are never taken, its
//!   blocks and headers never stored (at most they make us ask our own outbound peers sooner).
//! - no RPC, no wallet, no keys here: this module does not see them (the wallet lives behind the RPC on
//!   127.0.0.1 with its cookie; signing and sending are only there). Moving coins needs a signature with a key, and
//!   no message of this port leads to one.
//!
//! Limits: at most MAX_IN incoming peers and PER_IP per address, a version within HANDSHAKE, messages of at most
//! MAX_PAYLOAD bytes (requests are small), at most MAX_GETDATA items per getdata, write timeout, idle timeout.
//! Any breach closes the connection. Listening only with CHAIND_LISTEN (e.g. "0.0.0.0:46174,[::]:46174").

use crate::node::Shared;
use arcade_chain::encode::write_varint;
use arcade_chain::hash::sha256d;
use arcade_chain::p2p::{encode_message, parse_version, MSG_BLOCK};
use arcade_chain::Hash256;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_IN: usize = 48;
const PER_IP: usize = 4;
const HANDSHAKE: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(20 * 60);
const WRITE: Duration = Duration::from_secs(60);
const MAX_PAYLOAD: usize = 128 * 1024;
const MAX_GETDATA: usize = 500;
const MAX_HEADERS: u32 = 2000;
const MAX_INV: u32 = 500;
const NODE_NETWORK: u64 = 1;
/// what an incoming peer may ask; larger messages of other kinds are skipped up to SKIP_MAX
const REQUESTS: [&str; 6] = ["version", "ping", "getheaders", "getblocks", "getdata", "getaddr"];
const SKIP_MAX: usize = 4 * 1024 * 1024;

struct In {
    id: u64,
    ip: IpAddr,
    w: Arc<Mutex<TcpStream>>,
}

pub struct Listen {
    peers: Mutex<Vec<In>>,
    next: AtomicU64,
    /// blocks sent, connections taken / turned away (for the log)
    served: AtomicU64,
    taken: AtomicU64,
    refused: AtomicU64,
}

pub fn start(sh: Arc<Shared>, binds: &str) {
    let l = Arc::new(Listen { peers: Mutex::new(Vec::new()), next: 0.into(), served: 0.into(), taken: 0.into(), refused: 0.into() });
    for b in binds.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let listener = match bind(b) {
            Ok(x) => x,
            Err(e) => {
                sh.log(&format!("listen {b}: {e}"));
                continue;
            }
        };
        sh.log(&format!("listen on {b} (incoming peers: block data only, nothing taken in)"));
        let (sh, l) = (sh.clone(), l.clone());
        std::thread::spawn(move || accept(sh, l, listener));
    }
    let (sh2, l2) = (sh.clone(), l.clone());
    std::thread::spawn(move || announcer(sh2, l2));
}

/// "[::]:46174" takes IPv4 and IPv6 (Linux dual stack); "0.0.0.0:46174" IPv4 only.
fn bind(b: &str) -> std::io::Result<TcpListener> {
    let a: SocketAddr = b.parse().map_err(|_| std::io::Error::other("bad address"))?;
    TcpListener::bind(a)
}

fn accept(sh: Arc<Shared>, l: Arc<Listen>, listener: TcpListener) {
    for s in listener.incoming() {
        if sh.stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(s) = s else { continue };
        let Ok(addr) = s.peer_addr() else { continue };
        let ip = canonical(addr.ip());
        {
            let ps = l.peers.lock().unwrap();
            if ps.len() >= MAX_IN || ps.iter().filter(|p| p.ip == ip).count() >= PER_IP {
                l.refused.fetch_add(1, Ordering::Relaxed);
                continue; // dropped: closes it
            }
        }
        let (sh, l) = (sh.clone(), l.clone());
        std::thread::spawn(move || {
            let id = l.next.fetch_add(1, Ordering::Relaxed);
            let _ = serve(&sh, &l, s, addr, ip, id);
            l.peers.lock().unwrap().retain(|p| p.id != id);
        });
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

fn read(s: &mut TcpStream, magic: [u8; 4]) -> Result<(String, Vec<u8>), String> {
    let mut head = [0u8; 24];
    s.read_exact(&mut head).map_err(|e| e.to_string())?;
    if head[..4] != magic {
        return Err("bad magic".into());
    }
    let end = head[4..16].iter().position(|&b| b == 0).unwrap_or(12);
    let cmd = String::from_utf8_lossy(&head[4..4 + end]).into_owned();
    let len = u32::from_le_bytes(head[16..20].try_into().unwrap()) as usize;
    if len > MAX_PAYLOAD {
        // requests are small; a big message of a kind we ignore anyway (inv, addr, tx, block) is skipped unread
        if REQUESTS.contains(&cmd.as_str()) || len > SKIP_MAX {
            return Err(format!("{cmd}: {len} bytes, too large"));
        }
        let n = std::io::copy(&mut (&mut *s).take(len as u64), &mut std::io::sink()).map_err(|e| e.to_string())?;
        if n as usize != len {
            return Err("closed".into());
        }
        return Ok((cmd, Vec::new()));
    }
    let mut p = vec![0u8; len];
    s.read_exact(&mut p).map_err(|e| e.to_string())?;
    if sha256d(&p).0[..4] != head[20..24] {
        return Err("bad checksum".into());
    }
    Ok((cmd, p))
}

fn send(w: &Arc<Mutex<TcpStream>>, magic: [u8; 4], cmd: &str, payload: &[u8]) -> Result<(), String> {
    w.lock().unwrap().write_all(&encode_message(magic, cmd, payload)).map_err(|e| e.to_string())
}

fn net_addr(out: &mut Vec<u8>, addr: Option<SocketAddr>, services: u64) {
    out.extend_from_slice(&services.to_le_bytes());
    let (ip, port) = match addr {
        Some(SocketAddr::V4(a)) => (a.ip().to_ipv6_mapped().octets(), a.port()),
        Some(SocketAddr::V6(a)) => (a.ip().octets(), a.port()),
        None => ([0u8; 16], 0),
    };
    out.extend_from_slice(&ip);
    out.extend_from_slice(&port.to_be_bytes());
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Our version for an incoming peer: we serve the chain (NODE_NETWORK), and do not want its transactions (relay 0).
fn version(sh: &Shared, peer: SocketAddr) -> Vec<u8> {
    let p = sh.params;
    let mut v = Vec::new();
    v.extend_from_slice(&p.protocol_version.to_le_bytes());
    v.extend_from_slice(&NODE_NETWORK.to_le_bytes());
    v.extend_from_slice(&(now() as i64).to_le_bytes());
    net_addr(&mut v, Some(peer), 0);
    net_addr(&mut v, None, NODE_NETWORK);
    let mut nonce = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut nonce);
    }
    v.extend_from_slice(&nonce);
    let ua = p.user_agent().as_bytes();
    write_varint(&mut v, ua.len() as u64);
    v.extend_from_slice(ua);
    v.extend_from_slice(&(sh.ctip.load(Ordering::SeqCst) as i32).to_le_bytes());
    v.push(0);
    v
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let x = self.b.get(self.i..self.i + n)?;
        self.i += n;
        Some(x)
    }
    fn varint(&mut self) -> Option<u64> {
        let f = *self.take(1)?.first()?;
        Some(match f {
            0xfd => u16::from_le_bytes(self.take(2)?.try_into().ok()?) as u64,
            0xfe => u32::from_le_bytes(self.take(4)?.try_into().ok()?) as u64,
            0xff => u64::from_le_bytes(self.take(8)?.try_into().ok()?),
            x => x as u64,
        })
    }
    fn hash(&mut self) -> Option<Hash256> {
        Some(Hash256(self.take(32)?.try_into().ok()?))
    }
}

/// getheaders / getblocks: (locator, stop)
fn parse_locator(p: &[u8]) -> Option<(Vec<Hash256>, Hash256)> {
    let mut r = Rd { b: p, i: 0 };
    r.take(4)?;
    let n = r.varint()?;
    if n > 101 {
        return None;
    }
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        v.push(r.hash()?);
    }
    Some((v, r.hash()?))
}

/// The height after the first locator entry on our chain (0 = from genesis on).
fn fork_point(sh: &Shared, loc: &[Hash256]) -> u32 {
    let st = sh.store;
    let tip = sh.ctip.load(Ordering::SeqCst);
    for h in loc {
        if let Some(height) = st.height_of(h) {
            if height <= tip && st.id_at(height) == Some(*h) {
                return height + 1;
            }
        }
    }
    0
}

fn serve(sh: &Shared, l: &Listen, mut s: TcpStream, addr: SocketAddr, ip: IpAddr, id: u64) -> Result<(), String> {
    let magic = sh.params.magic;
    let fmt = &sh.params.format;
    s.set_read_timeout(Some(HANDSHAKE)).ok();
    s.set_write_timeout(Some(WRITE)).ok();
    s.set_nodelay(true).ok();
    // the peer speaks first: its version
    let (cmd, p) = read(&mut s, magic)?;
    if cmd != "version" {
        return Err("no version".into());
    }
    let pv = parse_version(&p).map_err(|e| e.to_string())?;
    let w = Arc::new(Mutex::new(s.try_clone().map_err(|e| e.to_string())?));
    send(&w, magic, "version", &version(sh, addr))?;
    send(&w, magic, "verack", &[])?;
    s.set_read_timeout(Some(IDLE)).ok();
    l.peers.lock().unwrap().push(In { id, ip, w: w.clone() });
    let n = l.taken.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 20 || n % 100 == 0 {
        sh.log(&format!("incoming peer {addr} ({}, height {}); {} connected, {} so far, {} turned away, {} blocks served",
            pv.user_agent.chars().filter(|c| !c.is_control()).take(40).collect::<String>(), pv.start_height,
            l.peers.lock().unwrap().len(), n, l.refused.load(Ordering::Relaxed), l.served.load(Ordering::Relaxed)));
    }
    let st = sh.store;
    loop {
        if sh.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let (cmd, p) = read(&mut s, magic)?;
        match cmd.as_str() {
            "ping" => send(&w, magic, "pong", &p)?,
            "getheaders" => {
                let (loc, stop) = parse_locator(&p).ok_or("bad getheaders")?;
                let tip = sh.ctip.load(Ordering::SeqCst);
                // no locator: just the header of `stop` (Core's rule)
                let from = if loc.is_empty() { st.height_of(&stop).filter(|&x| x <= tip && st.id_at(x) == Some(stop)).unwrap_or(tip + 1) } else { fork_point(sh, &loc) };
                let mut out = Vec::new();
                let mut n = 0u64;
                let mut body = Vec::new();
                let mut h = from;
                while h <= tip && n < MAX_HEADERS as u64 {
                    let Some((hid, hd)) = st.header(h) else { break };
                    body.extend_from_slice(&hd.encode());
                    body.push(0); // tx count
                    if fmt.block_sig {
                        body.push(0); // block signature (not in headers)
                    }
                    n += 1;
                    if hid == stop {
                        break;
                    }
                    h += 1;
                }
                write_varint(&mut out, n);
                out.extend(body);
                send(&w, magic, "headers", &out)?;
            }
            "getblocks" => {
                let (loc, stop) = parse_locator(&p).ok_or("bad getblocks")?;
                let tip = sh.ctip.load(Ordering::SeqCst);
                let mut ids = Vec::new();
                let mut h = fork_point(sh, &loc);
                while h <= tip && (ids.len() as u32) < MAX_INV {
                    let Some(bid) = st.id_at(h) else { break };
                    ids.push(bid);
                    if bid == stop {
                        break;
                    }
                    h += 1;
                }
                if !ids.is_empty() {
                    send(&w, magic, "inv", &inv(&ids))?;
                }
            }
            "getdata" => {
                let mut r = Rd { b: &p, i: 0 };
                let n = r.varint().ok_or("bad getdata")?;
                if n as usize > MAX_GETDATA {
                    return Err(format!("getdata of {n} items"));
                }
                let mut missing = Vec::new();
                for _ in 0..n {
                    let kind = u32::from_le_bytes(r.take(4).ok_or("bad getdata")?.try_into().unwrap());
                    let h = r.hash().ok_or("bad getdata")?;
                    let tip = sh.ctip.load(Ordering::SeqCst);
                    // blocks only (MSG_BLOCK; the witness flag bit ignored), and only those of our chain
                    let raw = (kind & 0xffff == MSG_BLOCK)
                        .then(|| st.height_of(&h))
                        .flatten()
                        .filter(|&height| height <= tip && st.id_at(height) == Some(h))
                        .and_then(|height| st.block(height));
                    match raw {
                        Some(raw) => {
                            send(&w, magic, "block", &raw)?;
                            l.served.fetch_add(1, Ordering::Relaxed);
                        }
                        None => missing.push((kind, h)),
                    }
                }
                if !missing.is_empty() {
                    let mut out = Vec::new();
                    write_varint(&mut out, missing.len() as u64);
                    for (k, h) in missing {
                        out.extend_from_slice(&k.to_le_bytes());
                        out.extend_from_slice(&h.0);
                    }
                    send(&w, magic, "notfound", &out)?;
                }
            }
            "getaddr" => {
                let mut out = Vec::new();
                let ps: Vec<SocketAddr> = sh.peers().iter().filter_map(|a| a.parse().ok()).collect();
                write_varint(&mut out, ps.len() as u64);
                for a in ps {
                    out.extend_from_slice(&(now() as u32).to_le_bytes());
                    net_addr(&mut out, Some(a), NODE_NETWORK);
                }
                send(&w, magic, "addr", &out)?;
            }
            // a peer telling us of a new block: our own outbound peers are asked sooner; nothing of it is taken
            "inv" | "headers" | "block" => sh.block_hint.store(true, Ordering::SeqCst),
            // transactions, mempool requests and the rest: ignored
            _ => {}
        }
    }
}

fn inv(ids: &[Hash256]) -> Vec<u8> {
    let mut p = Vec::new();
    write_varint(&mut p, ids.len() as u64);
    for id in ids {
        p.extend_from_slice(&MSG_BLOCK.to_le_bytes());
        p.extend_from_slice(&id.0);
    }
    p
}

/// When our tip moves on, the incoming peers hear of the new blocks (at most the last 10; a peer further behind
/// asks with getheaders/getblocks anyway). A peer that cannot take it is dropped.
fn announcer(sh: Arc<Shared>, l: Arc<Listen>) {
    let mut last = sh.ctip.load(Ordering::SeqCst);
    let mut said = std::time::Instant::now();
    while !sh.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        let tip = sh.ctip.load(Ordering::SeqCst);
        // a synced tip only (no announcements while catching up)
        if tip != last && tip + 6 >= sh.htip.load(Ordering::SeqCst) {
            let from = if tip > last { (last + 1).max(tip.saturating_sub(9)) } else { tip };
            let ids: Vec<Hash256> = (from..=tip).filter_map(|h| sh.store.id_at(h)).collect();
            let ws: Vec<(u64, Arc<Mutex<TcpStream>>)> = l.peers.lock().unwrap().iter().map(|p| (p.id, p.w.clone())).collect();
            let msg = inv(&ids);
            let mut bad = Vec::new();
            for (id, w) in ws {
                if send(&w, sh.params.magic, "inv", &msg).is_err() {
                    bad.push(id);
                    let _ = w.lock().unwrap().shutdown(std::net::Shutdown::Both);
                }
            }
            l.peers.lock().unwrap().retain(|p| !bad.contains(&p.id));
        }
        last = tip;
        if said.elapsed() > Duration::from_secs(3600) {
            said = std::time::Instant::now();
            let by_ip: HashMap<IpAddr, usize> = l.peers.lock().unwrap().iter().fold(HashMap::new(), |mut m, p| {
                *m.entry(p.ip).or_default() += 1;
                m
            });
            sh.log(&format!("incoming: {} peers from {} addresses, {} blocks served, {} turned away", by_ip.values().sum::<usize>(), by_ip.len(),
                l.served.load(Ordering::Relaxed), l.refused.load(Ordering::Relaxed)));
        }
    }
}
