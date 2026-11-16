//! Lokale Schnittstelle fuer Erweiterungen der Arcade (zuerst `cr`, geteilte und geliehene Chain-Knoten):
//! ein Dienst auf dieser Box (arcadeos-admin) bekommt die Mesh-Rahmen der Arten, die er bestellt, und darf
//! Rahmen genau dieser Arten an einzelne verbundene Knoten schicken. Sonst nichts: keine Raeume, kein
//! Adressbuch, keine anderen Arten.
//!
//! Zugang: Unix-Socket (Standard /run/meshd/ext.sock, Rechte 0660, Gruppe von meshd), dazu prueft meshd die
//! Nutzer-ID der Gegenseite (SO_PEERCRED) gegen MESHD_EXT_UIDS (Standard "0,1001"). Welche Arten es ueberhaupt
//! gibt, legt MESHD_EXT_KINDS fest (Standard "cr"). Ohne verbundenen Dienst sendet meshd nie einen dieser
//! Rahmen: ob und was geteilt wird, entscheidet allein der Dienst (Admin-Oberflaeche, Standard: nichts).
//!
//! Auf dem Socket: Rahmen = u32 BE Laenge | u16 BE Kopflaenge | Kopf (JSON-Objekt) | Anhang (Bytes).
//!
//! Dienst -> meshd:
//!   {"op":"hello","kinds":["cr"]}                 bestellt Arten (nur erlaubte werden gewaehrt)
//!   {"op":"send","peer":<id>,"h":{"t":"cr",...}}  + Anhang: an einen verbundenen Knoten (nur bestellte Arten)
//! meshd -> Dienst:
//!   {"op":"hello","node":<eigene id>,"kinds":[...]}   was gewaehrt ist
//!   {"op":"up","peer":<id>,"inbound":b}               Verbindung steht (nach hello fuer alle bestehenden)
//!   {"op":"down","peer":<id>}
//!   {"op":"links","links":[{"peer","rtt_ms","inbound"}]}   alle 15 s (fuer die Wahl des schnellsten)
//!   {"op":"frame","peer":<id>,"h":{...}}              + Anhang: ein empfangener Rahmen einer bestellten Art

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::dlog::ev;

/// groesster Rahmen auf dem Socket (ein Mesh-Rahmen ist hoechstens 64 KB)
pub const MAX_LOCAL: usize = 72 * 1024;
/// Rahmen, die fuer einen langsamen Dienst gepuffert werden; darueber werden empfangene verworfen
const QUEUE: usize = 4096;

pub struct ExtConfig {
    pub socket: PathBuf,
    pub kinds: HashSet<String>,
    pub uids: Vec<u32>,
}

impl ExtConfig {
    pub fn from_env() -> ExtConfig {
        let list = |k: &str, d: &str| -> Vec<String> {
            std::env::var(k).unwrap_or_else(|_| d.into()).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        };
        ExtConfig {
            socket: std::env::var("MESHD_EXT").map(PathBuf::from).unwrap_or_else(|_| "/run/meshd/ext.sock".into()),
            kinds: list("MESHD_EXT_KINDS", "cr").into_iter().collect(),
            uids: list("MESHD_EXT_UIDS", "0,1001").iter().filter_map(|s| s.parse().ok()).collect(),
        }
    }
}

struct Client {
    id: u64,
    kinds: HashSet<String>,
    tx: mpsc::Sender<Vec<u8>>,
}

/// Was die Schnittstelle vom Knoten braucht: an einen Knoten senden, die Verbindungen kennen.
pub trait Links: Send + Sync + 'static {
    fn send_to(&self, peer: &str, h: Value, blob: Vec<u8>) -> bool;
    /// (peer, inbound, rtt in s)
    fn links(&self) -> Vec<(String, bool, Option<f64>)>;
    fn node_id(&self) -> String;
}

pub struct Ext {
    cfg: ExtConfig,
    clients: Mutex<Vec<Client>>,
    next: AtomicU64,
    dropped: AtomicU64,
}

pub fn encode(h: &Value, blob: &[u8]) -> Vec<u8> {
    let head = serde_json::to_vec(h).expect("JSON");
    let mut v = Vec::with_capacity(6 + head.len() + blob.len());
    v.extend_from_slice(&((2 + head.len() + blob.len()) as u32).to_be_bytes());
    v.extend_from_slice(&(head.len() as u16).to_be_bytes());
    v.extend_from_slice(&head);
    v.extend_from_slice(blob);
    v
}

pub async fn read_local(r: &mut (impl AsyncReadExt + Unpin)) -> std::io::Result<(Value, Vec<u8>)> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let n = u32::from_be_bytes(len) as usize;
    if !(2..=MAX_LOCAL).contains(&n) {
        return Err(bad("Rahmengroesse"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).await?;
    let hl = u16::from_be_bytes([b[0], b[1]]) as usize;
    if 2 + hl > n {
        return Err(bad("Kopflaenge"));
    }
    let h: Value = serde_json::from_slice(&b[2..2 + hl]).map_err(|_| bad("Kopf ist kein JSON"))?;
    if !h.is_object() {
        return Err(bad("Kopf ist kein Objekt"));
    }
    Ok((h, b[2 + hl..].to_vec()))
}

impl Ext {
    pub fn new(cfg: ExtConfig) -> Arc<Ext> {
        Arc::new(Ext { cfg, clients: Mutex::new(Vec::new()), next: 0.into(), dropped: 0.into() })
    }

    /// Eine Art, die irgendein verbundener Dienst bestellt hat (sonst wird sie wie bisher nur gezaehlt).
    pub fn wants(&self, kind: &str) -> bool {
        self.cfg.kinds.contains(kind) && self.clients.lock().unwrap().iter().any(|c| c.kinds.contains(kind))
    }

    fn to_clients(&self, kind: Option<&str>, msg: Vec<u8>) {
        let cs = self.clients.lock().unwrap();
        for c in cs.iter().filter(|c| kind.is_none_or(|k| c.kinds.contains(k))) {
            if c.tx.try_send(msg.clone()).is_err() {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn frame(&self, peer: &str, kind: &str, h: &Value, blob: &[u8]) {
        if self.wants(kind) {
            self.to_clients(Some(kind), encode(&json!({"op": "frame", "peer": peer, "h": h}), blob));
        }
    }

    pub fn link_up(&self, peer: &str, inbound: bool) {
        self.to_clients(None, encode(&json!({"op": "up", "peer": peer, "inbound": inbound}), b""));
    }

    pub fn link_down(&self, peer: &str) {
        self.to_clients(None, encode(&json!({"op": "down", "peer": peer}), b""));
    }

    pub async fn run(self: Arc<Self>, node: Arc<dyn Links>) {
        let path = self.cfg.socket.clone();
        let _ = std::fs::remove_file(&path);
        let l = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                ev("error", "ext_listen_failed", json!({"socket": path.display().to_string(), "error": e.to_string()}));
                return;
            }
        };
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660));
        }
        ev("info", "ext_listen", json!({"socket": path.display().to_string(), "kinds": self.cfg.kinds, "uids": self.cfg.uids}));
        // Verbindungsliste mit Antwortzeiten alle 15 s
        {
            let (e, n) = (self.clone(), node.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    let links: Vec<Value> = n.links().into_iter()
                        .map(|(p, i, r)| json!({"peer": p, "inbound": i, "rtt_ms": r.map(|x| (x * 10000.0).round() / 10.0)}))
                        .collect();
                    e.to_clients(None, encode(&json!({"op": "links", "links": links}), b""));
                }
            });
        }
        loop {
            let Ok((s, _)) = l.accept().await else { continue };
            let uid = s.peer_cred().map(|c| c.uid()).ok();
            if !uid.is_some_and(|u| self.cfg.uids.contains(&u)) {
                ev("warn", "ext_refused", json!({"uid": uid}));
                continue;
            }
            let (e, n) = (self.clone(), node.clone());
            tokio::spawn(async move { e.client(s, n).await });
        }
    }

    async fn client(self: Arc<Self>, s: UnixStream, node: Arc<dyn Links>) {
        let (mut r, mut w) = s.into_split();
        // erst hello
        let kinds: HashSet<String> = match read_local(&mut r).await {
            Ok((h, _)) if h["op"] == "hello" => h["kinds"].as_array().into_iter().flatten()
                .filter_map(|k| k.as_str()).filter(|k| self.cfg.kinds.contains(*k)).map(str::to_string).collect(),
            _ => return,
        };
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(QUEUE);
        let mut sorted: Vec<&String> = kinds.iter().collect();
        sorted.sort();
        let _ = tx.try_send(encode(&json!({"op": "hello", "node": node.node_id(), "kinds": sorted}), b""));
        for (p, inbound, _) in node.links() {
            let _ = tx.try_send(encode(&json!({"op": "up", "peer": p, "inbound": inbound}), b""));
        }
        ev("info", "ext_client", json!({"client": id, "kinds": sorted}));
        self.clients.lock().unwrap().push(Client { id, kinds: kinds.clone(), tx });
        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if w.write_all(&m).await.is_err() {
                    break;
                }
            }
        });
        let mut sent = 0u64;
        let mut refused = 0u64;
        loop {
            let Ok((h, blob)) = read_local(&mut r).await else { break };
            if h["op"] != "send" {
                continue;
            }
            let (Some(peer), Some(f)) = (h["peer"].as_str(), h.get("h")) else { continue };
            // nur Rahmen der bestellten Arten, als Objekt mit genau dieser Art
            let kind = f.get("t").and_then(Value::as_str).unwrap_or("");
            if !f.is_object() || !kinds.contains(kind) || blob.len() > crate::link::MAX_FRAME - 1024 {
                refused += 1;
                continue;
            }
            if node.send_to(peer, f.clone(), blob) {
                sent += 1;
            }
        }
        writer.abort();
        self.clients.lock().unwrap().retain(|c| c.id != id);
        ev("info", "ext_client_gone", json!({"client": id, "sent": sent, "refused": refused, "dropped_total": self.dropped.load(Ordering::Relaxed)}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        sent: Mutex<Vec<(String, Value, Vec<u8>)>>,
    }
    impl Links for Fake {
        fn send_to(&self, peer: &str, h: Value, blob: Vec<u8>) -> bool {
            self.sent.lock().unwrap().push((peer.into(), h, blob));
            true
        }
        fn links(&self) -> Vec<(String, bool, Option<f64>)> {
            vec![("aa".repeat(32), true, Some(0.05))]
        }
        fn node_id(&self) -> String {
            "bb".repeat(32)
        }
    }

    #[tokio::test]
    async fn order_receive_send_refuse() {
        let dir = std::env::temp_dir().join(format!("meshd-ext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("ext.sock");
        let uid = unsafe_uid();
        let e = Ext::new(ExtConfig { socket: sock.clone(), kinds: ["cr".to_string()].into(), uids: vec![uid] });
        let fake = Arc::new(Fake { sent: Mutex::new(Vec::new()) });
        let f2: Arc<dyn Links> = fake.clone();
        tokio::spawn(e.clone().run(f2));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let s = UnixStream::connect(&sock).await.unwrap();
        let (mut r, mut w) = s.into_split();
        // asks for cr and op: only cr is allowed
        w.write_all(&encode(&json!({"op": "hello", "kinds": ["cr", "op"]}), b"")).await.unwrap();
        let (h, _) = read_local(&mut r).await.unwrap();
        assert_eq!(h["op"], "hello");
        assert_eq!(h["kinds"], json!(["cr"]));
        let (h, _) = read_local(&mut r).await.unwrap();
        assert_eq!(h["op"], "up");
        // a received cr frame comes through with its attachment; an op frame does not
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(e.wants("cr") && !e.wants("op"));
        e.frame("cc", "op", &json!({"t": "op"}), b"");
        e.frame("cc", "cr", &json!({"t": "cr", "op": "q"}), b"[1]");
        let (h, b) = read_local(&mut r).await.unwrap();
        assert_eq!((h["op"].as_str(), h["h"]["op"].as_str(), b.as_slice()), (Some("frame"), Some("q"), &b"[1]"[..]));
        // sending: cr goes out, other kinds are refused
        w.write_all(&encode(&json!({"op": "send", "peer": "cc", "h": {"t": "cr", "op": "a"}}), b"xyz")).await.unwrap();
        w.write_all(&encode(&json!({"op": "send", "peer": "cc", "h": {"t": "sub", "op": "a"}}), b"")).await.unwrap();
        w.write_all(&encode(&json!({"op": "send", "peer": "cc", "h": "kein Objekt"}), b"")).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let sent = fake.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].1["t"].as_str(), sent[0].2.as_slice()), (Some("cr"), &b"xyz"[..]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wrong_uid_refused() {
        let dir = std::env::temp_dir().join(format!("meshd-ext2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("ext.sock");
        let e = Ext::new(ExtConfig { socket: sock.clone(), kinds: ["cr".to_string()].into(), uids: vec![unsafe_uid() + 4242] });
        let f: Arc<dyn Links> = Arc::new(Fake { sent: Mutex::new(Vec::new()) });
        tokio::spawn(e.clone().run(f));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let s = UnixStream::connect(&sock).await.unwrap();
        let (mut r, mut w) = s.into_split();
        let _ = w.write_all(&encode(&json!({"op": "hello", "kinds": ["cr"]}), b"")).await;
        assert!(read_local(&mut r).await.is_err(), "closed without hello");
        assert!(!e.wants("cr"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn unsafe_uid() -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0)
    }
}
