//! Dauerbetrieb: ein Knoten im Arcade-Mesh.
//!
//! Verhalten wie die Arcades (docs/mesh-protocol.md): Adressbuch mit Rueckzug bei Fehlschlaegen,
//! OUTBOUND_TARGET ausgehende Verbindungen (direkte Adressen vor Tunneln), eine Verbindung pro Knoten,
//! ping alle 15 s, tot nach 45 s Stille, beim Verbindungsaufbau die erreichten Adressen weitergeben.
//!
//! Stand: Blatt-Knoten. Raumverkehr (sub/pub) wird gezaehlt, aber nicht weitergeleitet – andere
//! Knoten bauen ihre Wege nur ueber Knoten, deren Ankuendigungen sie erreicht haben, also laeuft
//! ueber ein Blatt kein fremder Verkehr. Nur ausgehend, keine Ankuendigung auf der Chain.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use rand::seq::SliceRandom;
use rand::RngCore;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

use crate::dlog::ev;
use crate::link::{self, handshake, Body};

pub const OUTBOUND_TARGET: usize = 6;
pub const PING_EVERY: Duration = Duration::from_secs(15);
pub const DEAD_AFTER: Duration = Duration::from_secs(45);
const DIAL_EVERY: Duration = Duration::from_secs(1);
const SAVE_EVERY: Duration = Duration::from_secs(300);
const STATUS_EVERY: Duration = Duration::from_secs(5);
const MAX_ADDRS: usize = 1000;
const STATS_EVERY: Duration = Duration::from_secs(60);

/// Bekannte Live-Arcades als Startpunkt (ohne Chain-Ankuendigungen)
pub const DEFAULT_SEEDS: &[&str] = &[
    "207.244.232.43:8421",
    "104.153.31.40:8421",
    "156.67.28.112:8421",
    "wss://app.dogecoinarcade.com/mesh",
    "wss://pepecoinarcade.com/mesh",
];

pub struct Config {
    pub net: String,
    pub key_file: PathBuf,
    pub peers_file: PathBuf,
    pub seeds_file: PathBuf,
    pub status_file: PathBuf,
    pub outbound: usize,
}

fn now_unix() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn is_ws(host: &str) -> bool {
    host.starts_with("ws://") || host.starts_with("wss://")
}

/// Adressen, die andere Knoten nennen: oeffentliche IP-Literale oder ws(s)-Adressen, nie Namen,
/// nie private/lokale Bereiche (wie die Arcades bei Ankuendigungen)
fn acceptable(host: &str, port: i64) -> bool {
    if !(1..=65535).contains(&port) || host.len() > 255 {
        return false;
    }
    if is_ws(host) {
        return crate::ws::WsUrl::parse(host).is_some();
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => !(a.is_private() || a.is_loopback() || a.is_link_local() || a.is_unspecified()
            || a.is_broadcast() || a.is_documentation() || a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 64),
        Ok(IpAddr::V6(a)) => !(a.is_loopback() || a.is_unspecified() || (a.segments()[0] & 0xfe00) == 0xfc00
            || (a.segments()[0] & 0xffc0) == 0xfe80),
        Err(_) => false,
    }
}

/// Wie link::connect sie erwartet
fn dial_string(host: &str, port: u16) -> String {
    if is_ws(host) {
        host.to_string()
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[derive(Clone)]
struct Entry {
    host: String,
    port: u16,
    id: Option<String>,
    ok: f64,     // Unix-Zeit des letzten Erfolgs (0 = nie)
    fail: u32,
    next: Instant,
    seed: bool,
}

struct LinkInfo {
    addr: (String, u16),
    since: Instant,
    rtt: Option<f64>,
    tx: mpsc::UnboundedSender<Value>,
    frames: BTreeMap<String, u64>,
    frames_logged: BTreeMap<String, u64>, // Stand bei der letzten Statistik (fuer Differenzen)
}

struct State {
    addrs: HashMap<(String, u16), Entry>,
    links: HashMap<String, LinkInfo>,
    dialing: HashSet<(String, u16)>,
}

pub struct Node {
    cfg: Config,
    key: SigningKey,
    id: String,
    started: Instant,
    st: Mutex<State>,
}

/// Mesh-Key laden oder (beim ersten Start) erzeugen: 32-Byte-Seed als Hex, Datei 0600
pub fn load_or_create_key(path: &PathBuf) -> std::io::Result<SigningKey> {
    if let Ok(s) = std::fs::read_to_string(path) {
        let b = hex::decode(s.trim()).map_err(|_| std::io::Error::other("Mesh-Key unlesbar"))?;
        let seed: [u8; 32] = b.try_into().map_err(|_| std::io::Error::other("Mesh-Key hat falsche Laenge"))?;
        return Ok(SigningKey::from_bytes(&seed));
    }
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("new");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(hex::encode(seed).as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    ev("info", "key_created", json!({"path": path.display().to_string()}));
    Ok(SigningKey::from_bytes(&seed))
}

impl Node {
    pub fn new(cfg: Config) -> std::io::Result<Arc<Node>> {
        let key = load_or_create_key(&cfg.key_file)?;
        let id = hex::encode(key.verifying_key().to_bytes());
        let node = Arc::new(Node {
            cfg,
            key,
            id,
            started: Instant::now(),
            st: Mutex::new(State { addrs: HashMap::new(), links: HashMap::new(), dialing: HashSet::new() }),
        });
        node.load_addresses();
        Ok(node)
    }

    fn add_address(&self, host: &str, port: u16, id: Option<&str>, seed: bool) {
        let mut st = self.st.lock().unwrap();
        if st.addrs.len() >= MAX_ADDRS && !st.addrs.contains_key(&(host.to_string(), port)) {
            return;
        }
        let e = st.addrs.entry((host.to_string(), port)).or_insert_with(|| Entry {
            host: host.to_string(),
            port,
            id: None,
            ok: 0.0,
            fail: 0,
            next: Instant::now(),
            seed,
        });
        if e.id.is_none() {
            e.id = id.filter(|s| s.len() == 64).map(str::to_string);
        }
        e.seed |= seed;
    }

    fn load_addresses(&self) {
        let seeds: Vec<String> = match std::fs::read_to_string(&self.cfg.seeds_file) {
            Ok(s) => s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect(),
            Err(_) => DEFAULT_SEEDS.iter().map(|s| s.to_string()).collect(),
        };
        for s in seeds {
            if is_ws(&s) {
                if let Some(u) = crate::ws::WsUrl::parse(&s) {
                    self.add_address(&s, u.port, None, true);
                }
            } else if let Ok(sa) = s.parse::<std::net::SocketAddr>() {
                self.add_address(&sa.ip().to_string(), sa.port(), None, true);
            } else {
                ev("warn", "seed_invalid", json!({"seed": s}));
            }
        }
        if let Ok(s) = std::fs::read_to_string(&self.cfg.peers_file) {
            if let Ok(Value::Array(list)) = serde_json::from_str::<Value>(&s) {
                for a in list {
                    let (Some(h), Some(p)) = (a["h"].as_str(), a["p"].as_i64()) else { continue };
                    if acceptable(h, p) {
                        self.add_address(h, p as u16, a["id"].as_str(), false);
                        let mut st = self.st.lock().unwrap();
                        if let Some(e) = st.addrs.get_mut(&(h.to_string(), p as u16)) {
                            e.ok = a["ok"].as_f64().unwrap_or(0.0);
                        }
                    }
                }
            }
        }
    }

    fn save_addresses(&self) {
        let list: Vec<Value> = {
            let st = self.st.lock().unwrap();
            st.addrs.values().filter(|e| e.ok > 0.0).map(|e| json!({"h": e.host, "p": e.port, "id": e.id, "ok": e.ok})).collect()
        };
        let tmp = self.cfg.peers_file.with_extension("new");
        if let Some(dir) = self.cfg.peers_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&tmp, serde_json::to_vec(&list).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.cfg.peers_file);
        }
    }

    fn outbound_count(&self) -> usize {
        let st = self.st.lock().unwrap();
        st.links.len() + st.dialing.len()
    }

    /// Kandidaten zum Waehlen: faellig, nicht wir, nicht schon verbunden; direkte vor Tunneln
    fn pick(&self, want: usize) -> Vec<(String, u16)> {
        let mut st = self.st.lock().unwrap();
        let now = Instant::now();
        let mut choices: Vec<(String, u16)> = st
            .addrs
            .iter()
            .filter(|(a, e)| {
                e.next <= now
                    && e.id.as_deref() != Some(self.id.as_str())
                    && !e.id.as_ref().is_some_and(|id| st.links.contains_key(id))
                    && !st.dialing.contains(*a)
            })
            .map(|(a, _)| a.clone())
            .collect();
        choices.shuffle(&mut OsRng);
        let direct_ok: HashSet<String> = st
            .addrs
            .values()
            .filter(|e| !is_ws(&e.host) && e.fail < 3)
            .filter_map(|e| e.id.clone())
            .collect();
        let mut ordered: Vec<(String, u16)> = choices.iter().filter(|a| !is_ws(&a.0)).cloned().collect();
        ordered.extend(choices.into_iter().filter(|a| {
            is_ws(&a.0) && !st.addrs[a].id.as_ref().is_some_and(|id| direct_ok.contains(id))
        }));
        ordered.truncate(want);
        for a in &ordered {
            st.dialing.insert(a.clone());
            if let Some(e) = st.addrs.get_mut(a) {
                e.next = now + Duration::from_secs(30);
            }
        }
        ordered
    }

    pub async fn run(self: Arc<Self>) {
        {
            let st = self.st.lock().unwrap();
            ev("info", "start", json!({
                "node_id": self.id, "net": self.cfg.net, "outbound_target": self.cfg.outbound,
                "version": env!("CARGO_PKG_VERSION"), "addrs_loaded": st.addrs.len(),
                "seeds": st.addrs.values().filter(|e| e.seed).count(),
                "reached_before": st.addrs.values().filter(|e| e.ok > 0.0).count(),
            }));
        }
        let n = self.clone();
        tokio::spawn(async move { n.status_loop().await });
        let n = self.clone();
        tokio::spawn(async move { n.stats_loop().await });
        let n = self.clone();
        tokio::spawn(async move {
            loop {
                sleep(SAVE_EVERY).await;
                n.save_addresses();
            }
        });
        loop {
            let deficit = self.cfg.outbound.saturating_sub(self.outbound_count());
            if deficit > 0 {
                for addr in self.pick(deficit) {
                    let n = self.clone();
                    tokio::spawn(async move { n.dial(addr).await });
                }
            }
            sleep(DIAL_EVERY).await;
        }
    }

    pub fn shutdown(&self) {
        ev("info", "shutdown", json!({"uptime_s": self.started.elapsed().as_secs(), "links": self.st.lock().unwrap().links.len()}));
        self.save_addresses();
        let _ = std::fs::remove_file(&self.cfg.status_file);
    }

    async fn dial(self: Arc<Self>, addr: (String, u16)) {
        let target = dial_string(&addr.0, addr.1);
        let limit = if is_ws(&addr.0) { 12 } else { 5 };
        let t0 = Instant::now();
        let mut connect_ms = 0.0;
        let res = async {
            let io = timeout(Duration::from_secs(limit), link::connect(&target)).await.map_err(|_| link::LinkError::Timeout)??;
            connect_ms = t0.elapsed().as_secs_f64() * 1000.0;
            handshake(io, &self.key, &self.cfg.net, true, None).await
        }
        .await;
        let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let lk = match res {
            Ok(l) => l,
            Err(e) => {
                let mut st = self.st.lock().unwrap();
                st.dialing.remove(&addr);
                let (mut fail, mut wait) = (0, 0);
                if let Some(en) = st.addrs.get_mut(&addr) {
                    en.fail += 1;
                    wait = (5u64 << en.fail.min(7)).min(600);
                    en.next = Instant::now() + Duration::from_secs(wait);
                    fail = en.fail;
                }
                drop(st);
                ev("debug", "dial_failed", json!({"addr": target, "error": e.to_string(), "ms": (total_ms * 10.0).round() / 10.0,
                    "fails_in_row": fail, "retry_in_s": wait}));
                return;
            }
        };
        let peer = lk.peer_id();
        let fresh;
        {
            let mut st = self.st.lock().unwrap();
            st.dialing.remove(&addr);
            let en = st.addrs.get_mut(&addr).expect("Adresse");
            fresh = en.ok == 0.0;
            en.id = Some(peer.clone());
            en.ok = now_unix();
            en.fail = 0;
            en.next = Instant::now() + Duration::from_secs(30);
            if st.links.contains_key(&peer) {
                // eine Verbindung pro Knoten (z.B. TCP und Tunnel zum selben Knoten)
                drop(st);
                ev("debug", "duplicate_link", json!({"addr": target, "peer": peer}));
                return;
            }
        }
        ev("info", "link_up", json!({"peer": peer, "addr": target, "transport": if is_ws(&addr.0) { "websocket" } else { "tcp" },
            "connect_ms": (connect_ms * 10.0).round() / 10.0, "handshake_ms": ((total_ms - connect_ms) * 10.0).round() / 10.0,
            "first_time": fresh}));
        if fresh {
            self.tell_others(&addr, &peer);
        }
        self.run_link(lk, addr, peer).await;
    }

    /// Neu erreichte Adresse den bestehenden Verbindungen mitteilen
    fn tell_others(&self, addr: &(String, u16), peer: &str) {
        let st = self.st.lock().unwrap();
        for (id, l) in &st.links {
            if id != peer {
                let _ = l.tx.send(json!({"t": "addrs", "a": [{"h": addr.0, "p": addr.1, "id": peer}]}));
            }
        }
    }

    async fn run_link(self: Arc<Self>, lk: link::Link, addr: (String, u16), peer: String) {
        let link::Link { mut tx, mut rx, .. } = lk;
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
        {
            let mut st = self.st.lock().unwrap();
            if st.links.contains_key(&peer) {
                return;
            }
            st.links.insert(
                peer.clone(),
                LinkInfo { addr: addr.clone(), since: Instant::now(), rtt: None, tx: out_tx.clone(), frames: BTreeMap::new(),
                    frames_logged: BTreeMap::new() },
            );
            // unsere erreichten Adressen weitergeben (wie die Arcades beim Verbindungsaufbau)
            let good: Vec<Value> = st
                .addrs
                .values()
                .filter(|e| e.ok > 0.0 && e.id.is_some() && e.id.as_deref() != Some(peer.as_str()))
                .take(50)
                .map(|e| json!({"h": e.host, "p": e.port, "id": e.id}))
                .collect();
            if !good.is_empty() {
                let _ = out_tx.send(json!({"t": "addrs", "a": good}));
            }
        }

        // Schreiber: alles, was in den Kanal kommt, geht verschluesselt raus
        let writer = tokio::spawn(async move {
            while let Some(v) = out_rx.recv().await {
                if tx.send(&v, b"").await.is_err() {
                    break;
                }
            }
        });
        // Pinger: nach 1 s, dann alle 15 s
        let pings: Arc<Mutex<HashMap<String, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
        let (p2, t2) = (pings.clone(), out_tx.clone());
        let pinger = tokio::spawn(async move {
            sleep(Duration::from_secs(1)).await;
            loop {
                let mut b = [0u8; 4];
                OsRng.fill_bytes(&mut b);
                let n = hex::encode(b);
                {
                    let mut p = p2.lock().unwrap();
                    p.insert(n.clone(), Instant::now());
                    if p.len() > 8 {
                        let oldest = p.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone());
                        if let Some(k) = oldest {
                            p.remove(&k);
                        }
                    }
                }
                if t2.send(json!({"t": "ping", "n": n})).is_err() {
                    break;
                }
                sleep(PING_EVERY).await;
            }
        });

        let reason;
        loop {
            let body = match timeout(DEAD_AFTER, rx.recv()).await {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => {
                    reason = e.to_string();
                    break;
                }
                Err(_) => {
                    reason = format!("{} s still", DEAD_AFTER.as_secs());
                    break;
                }
            };
            let (kind, header) = match body {
                Body::Json(h, _) => (h.get("t").and_then(Value::as_str).unwrap_or("?").to_string(), Some(h)),
                Body::BinaryPub(_) => ("pub".to_string(), None),
            };
            let first = {
                let mut st = self.st.lock().unwrap();
                let l = st.links.get_mut(&peer).expect("Link");
                let c = l.frames.entry(kind.clone()).or_default();
                *c += 1;
                *c == 1
            };
            if first {
                // beim ersten Auftreten einer Nachrichtenart: Kopf (gekuerzt) mitschreiben
                let sample = match &body_preview(&header) {
                    Some(s) => Value::from(s.clone()),
                    None => Value::Null,
                };
                ev("debug", "first_frame", json!({"peer": peer, "kind": kind, "sample": sample}));
            }
            let Some(h) = header else { continue };
            match kind.as_str() {
                "ping" => {
                    if let Some(n) = h.get("n").and_then(Value::as_str) {
                        let _ = out_tx.send(json!({"t": "pong", "n": n.chars().take(16).collect::<String>()}));
                    }
                }
                "pong" => {
                    let sent = h.get("n").and_then(Value::as_str).and_then(|n| pings.lock().unwrap().remove(n));
                    if let Some(t) = sent {
                        let rtt = t.elapsed().as_secs_f64();
                        if let Some(l) = self.st.lock().unwrap().links.get_mut(&peer) {
                            l.rtt = Some(l.rtt.map_or(rtt, |r| 0.7 * r + 0.3 * rtt));
                        }
                    }
                }
                "addrs" => {
                    if let Some(list) = h.get("a").and_then(Value::as_array) {
                        let (mut new, mut refused) = (Vec::new(), Vec::new());
                        for a in list.iter().take(50) {
                            let (Some(host), Some(port)) = (a.get("h").and_then(Value::as_str), a.get("p").and_then(Value::as_i64)) else { continue };
                            if acceptable(host, port) {
                                let known = self.st.lock().unwrap().addrs.contains_key(&(host.to_string(), port as u16));
                                self.add_address(host, port as u16, a.get("id").and_then(Value::as_str), false);
                                if !known {
                                    new.push(dial_string(host, port as u16));
                                }
                            } else {
                                refused.push(format!("{host}:{port}"));
                            }
                        }
                        if !new.is_empty() || !refused.is_empty() {
                            ev("debug", "addrs", json!({"peer": peer, "new": new, "refused": refused}));
                        }
                    }
                }
                // sub/pub und Erweiterungen der Arcade-Software: zaehlen, nicht weiterleiten (Blatt)
                _ => {}
            }
        }
        pinger.abort();
        writer.abort();
        let gone = self.st.lock().unwrap().links.remove(&peer);
        if let Some(l) = gone {
            ev("info", "link_down", json!({"peer": peer, "addr": dial_string(&l.addr.0, l.addr.1), "reason": reason,
                "up_s": l.since.elapsed().as_secs(), "rtt_ms": l.rtt.map(|r| (r * 10000.0).round() / 10.0), "frames": l.frames}));
        }
    }

    /// Jede Minute: pro Verbindung Antwortzeit und Nachrichten seit der letzten Statistik
    async fn stats_loop(self: Arc<Self>) {
        loop {
            sleep(STATS_EVERY).await;
            let mut st = self.st.lock().unwrap();
            let mut links = Vec::new();
            for (id, l) in st.links.iter_mut() {
                let delta: BTreeMap<String, u64> = l
                    .frames
                    .iter()
                    .map(|(k, v)| (k.clone(), v - l.frames_logged.get(k).copied().unwrap_or(0)))
                    .filter(|(_, d)| *d > 0)
                    .collect();
                l.frames_logged = l.frames.clone();
                links.push(json!({"peer": &id[..16], "addr": dial_string(&l.addr.0, l.addr.1),
                    "rtt_ms": l.rtt.map(|r| (r * 10000.0).round() / 10.0), "up_s": l.since.elapsed().as_secs(), "frames_1min": delta}));
            }
            let known = st.addrs.len();
            let reached = st.addrs.values().filter(|e| e.ok > 0.0).count();
            let backing_off = st.addrs.values().filter(|e| e.next > Instant::now() && e.fail > 0).count();
            drop(st);
            ev("debug", "stats", json!({"links": links, "addrs_known": known, "addrs_reached": reached, "backing_off": backing_off}));
        }
    }

    fn status(&self) -> Value {
        let st = self.st.lock().unwrap();
        let links: Vec<Value> = st
            .links
            .iter()
            .map(|(id, l)| {
                json!({
                    "id": id,
                    "addr": dial_string(&l.addr.0, l.addr.1),
                    "transport": if is_ws(&l.addr.0) { "websocket" } else { "tcp" },
                    "rtt_ms": l.rtt.map(|r| (r * 10000.0).round() / 10.0),
                    "up_s": l.since.elapsed().as_secs(),
                    "frames": l.frames,
                })
            })
            .collect();
        json!({
            "node_id": self.id,
            "net": self.cfg.net,
            "mode": "leaf-outbound",
            "uptime_s": self.started.elapsed().as_secs(),
            "links": links,
            "dialing": st.dialing.len(),
            "addrs_known": st.addrs.len(),
            "addrs_reached": st.addrs.values().filter(|e| e.ok > 0.0).count(),
        })
    }

    async fn status_loop(self: Arc<Self>) {
        loop {
            let tmp = self.cfg.status_file.with_extension("new");
            if std::fs::write(&tmp, format!("{}\n", self.status())).is_ok() {
                let _ = std::fs::rename(&tmp, &self.cfg.status_file);
            }
            sleep(STATUS_EVERY).await;
        }
    }
}

/// Kopf einer Nachricht fuer das Log, auf 300 Zeichen gekuerzt (Signaturen/Keys bleiben drin: oeffentlich)
fn body_preview(h: &Option<Value>) -> Option<String> {
    h.as_ref().map(|v| v.to_string().chars().take(300).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_filter() {
        assert!(acceptable("207.244.232.43", 8421));
        assert!(acceptable("2605:59ca:13cf:2e10:e2e4:ad5b:f18f:fe08", 8421));
        assert!(acceptable("wss://app.dogecoinarcade.com/mesh", 443));
        assert!(!acceptable("192.168.1.5", 8421));
        assert!(!acceptable("10.0.2.15", 8421));
        assert!(!acceptable("127.0.0.1", 8421));
        assert!(!acceptable("100.64.1.1", 8421));
        assert!(!acceptable("fe80::1", 8421));
        assert!(!acceptable("fd00::1", 8421));
        assert!(!acceptable("arcade.example.com", 8421));
        assert!(!acceptable("207.244.232.43", 0));
    }

    #[test]
    fn dial_strings() {
        assert_eq!(dial_string("1.2.3.4", 8421), "1.2.3.4:8421");
        assert_eq!(dial_string("2605::1", 8421), "[2605::1]:8421");
        assert_eq!(dial_string("wss://x.y/mesh", 443), "wss://x.y/mesh");
    }

    #[test]
    fn key_is_created_once_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("meshd-test-{}", std::process::id()));
        let p = dir.join("key");
        let k1 = load_or_create_key(&p).unwrap();
        let k2 = load_or_create_key(&p).unwrap();
        assert_eq!(k1.to_bytes(), k2.to_bytes());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
