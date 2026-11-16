//! meshd – ArcadeOS Mesh-Dienst (Arcade-Mesh-Protokoll v2).
//!
//! Befehle:
//!   meshd run [--net arcade-main] [--state DIR] [--status DATEI] [--outbound N] [--listen PORT]
//!       Dauerbetrieb (node.rs). DIR (Standard /state/mesh): key, peers.json, seeds (optional),
//!       listen (optional: Port fuer eingehende Verbindungen, 0 = keine), debug.log (JSON-Zeilen,
//!       rotiert bei 2 MB). Ohne Angabe lauscht meshd auf 8421; --listen geht vor der Datei.
//!   meshd status [--status DATEI]      Zustand des laufenden Dienstes (JSON)
//!   meshd probe [--net arcade-main] [--pings N] host:port|wss://host/mesh ...
//! verbindet sich mit jedem Knoten (frischer Schluessel, lauscht nicht), misst TCP-Aufbau, Handshake
//! und Ping-Antwortzeiten, sammelt die Adressen, die der Knoten nennt, und gibt je Knoten eine
//! JSON-Zeile aus.

mod dlog;
mod ext;
mod rooms;
mod link;
mod node;
mod ws;

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use rand::RngCore;
use serde_json::{json, Value};
use tokio::time::{sleep, timeout};

use link::{handshake, Body};

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

async fn probe(addr: String, net: String, pings: usize, key: SigningKey) -> Value {
    let mut out = json!({"addr": addr});
    let t0 = Instant::now();
    let stream = match timeout(Duration::from_secs(10), link::connect(&addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return json!({"addr": addr, "error": format!("verbinden: {e}")}),
        Err(_) => return json!({"addr": addr, "error": "verbinden: Zeitlimit"}),
    };
    // bei WebSocket inkl. TLS und HTTP-Upgrade
    out["connect_ms"] = json!(round1(t0.elapsed().as_secs_f64() * 1000.0));
    let t1 = Instant::now();
    let link = match handshake(stream, &key, &net, true, None).await {
        Ok(l) => l,
        Err(e) => return json!({"addr": addr, "error": format!("handshake: {e}")}),
    };
    out["handshake_ms"] = json!(round1(t1.elapsed().as_secs_f64() * 1000.0));
    out["id"] = json!(link.peer_id()[..16].to_string());
    out["listen"] = json!(link.peer_listen);

    let link::Link { mut tx, mut rx, .. } = link;
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<(Value, Instant)>();
    // Empfang: Pings sofort beantworten, alles andere an die Auswertung
    let (pong_tx, mut pong_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Body::Json(h, _)) => {
                    let t = h.get("t").and_then(Value::as_str).unwrap_or("?").to_string();
                    if t == "ping" {
                        if let Some(n) = h.get("n").and_then(Value::as_str) {
                            let _ = pong_tx.send(n.chars().take(16).collect());
                        }
                    }
                    let _ = ev_tx.send((h, Instant::now()));
                }
                Ok(Body::BinaryPub(_)) => {
                    let _ = ev_tx.send((json!({"t": "pub(bin)"}), Instant::now()));
                }
                Err(_) => break,
            }
        }
    });

    let mut sent: HashMap<String, Instant> = HashMap::new();
    let mut rtts: Vec<f64> = Vec::new();
    let mut addrs: Vec<Value> = Vec::new();
    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    let mut handle = |h: Value, at: Instant, sent: &mut HashMap<String, Instant>, rtts: &mut Vec<f64>, addrs: &mut Vec<Value>| {
        *kinds.entry(h.get("t").and_then(Value::as_str).unwrap_or("?").to_string()).or_default() += 1;
        match h.get("t").and_then(Value::as_str) {
            Some("pong") => {
                if let Some(t) = h.get("n").and_then(Value::as_str).and_then(|n| sent.remove(n)) {
                    rtts.push((at - t).as_secs_f64() * 1000.0);
                }
            }
            Some("addrs") => {
                if let Some(a) = h.get("a").and_then(Value::as_array) {
                    addrs.extend(a.iter().take(50).cloned());
                }
            }
            _ => {}
        }
    };

    let end_pings = pings;
    let mut i = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let deadline = Instant::now() + Duration::from_millis(250 * pings as u64 + 2500);
    loop {
        tokio::select! {
            _ = tick.tick(), if i < end_pings => {
                let mut b = [0u8; 4];
                OsRng.fill_bytes(&mut b);
                let n = hex::encode(b);
                sent.insert(n.clone(), Instant::now());
                if tx.send(&json!({"t": "ping", "n": n}), b"").await.is_err() { break; }
                i += 1;
            }
            Some(n) = pong_rx.recv() => {
                if tx.send(&json!({"t": "pong", "n": n}), b"").await.is_err() { break; }
            }
            Some((h, at)) = ev_rx.recv() => handle(h, at, &mut sent, &mut rtts, &mut addrs),
            _ = sleep(deadline.saturating_duration_since(Instant::now())) => break,
        }
    }
    drop(tx);
    reader.abort();
    drop(handle);
    rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !rtts.is_empty() {
        out["pings"] = json!(rtts.len());
        out["rtt_ms"] = json!({
            "min": round1(rtts[0]), "median": round1(percentile(&rtts, 0.5)),
            "p90": round1(percentile(&rtts, 0.9)), "p99": round1(percentile(&rtts, 0.99)),
            "max": round1(*rtts.last().unwrap())
        });
    }
    out["frames"] = json!(kinds);
    out["addrs"] = json!(addrs);
    out
}

fn usage() -> ! {
    eprintln!("Aufruf: meshd run [--net arcade-main] [--state /state/mesh] [--status /run/meshd.json] [--outbound 6] [--listen 8421|0]");
    eprintln!("       meshd status [--status /run/meshd.json]");
    eprintln!("       meshd probe [--net arcade-main] [--pings N] host:port|wss://host/mesh ...");
    std::process::exit(2);
}

fn cmd_run(args: Vec<String>) {
    let mut net = "arcade-main".to_string();
    let mut state = std::path::PathBuf::from("/state/mesh");
    let mut status = std::path::PathBuf::from("/run/meshd.json");
    let mut outbound = node::OUTBOUND_TARGET;
    let mut listen_arg: Option<u16> = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--net" => net = it.next().unwrap_or_else(|| usage()),
            "--state" => state = it.next().unwrap_or_else(|| usage()).into(),
            "--status" => status = it.next().unwrap_or_else(|| usage()).into(),
            "--outbound" => outbound = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "--listen" => listen_arg = Some(it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage())),
            _ => usage(),
        }
    }
    dlog::init(state.join("debug.log"));
    let listen = listen_port(listen_arg, &state.join("listen"));
    // Wer diese Arcade betreibt: von arcadeos-admin (Einrichtung unter http://<box>/admin/)
    let about = std::path::PathBuf::from(std::env::var("MESHD_ABOUT").unwrap_or_else(|_| "/state/arcade/about.json".into()));
    let cfg = node::Config {
        net,
        key_file: state.join("key"),
        peers_file: state.join("peers.json"),
        seeds_file: state.join("seeds"),
        status_file: status,
        outbound,
        listen,
        max_inbound: node::MAX_INBOUND,
        about_file: Some(about),
        // lokale Schnittstelle fuer Erweiterungen (ext.rs); MESHD_EXT=off schaltet sie ab
        ext: (std::env::var("MESHD_EXT").as_deref() != Ok("off")).then(ext::ExtConfig::from_env),
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    rt.block_on(async {
        let n = match node::Node::new(cfg) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("meshd: Start fehlgeschlagen: {e}");
                std::process::exit(1);
            }
        };
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
        let runner = n.clone();
        tokio::select! {
            _ = runner.run() => {}
            _ = term.recv() => eprintln!("meshd: SIGTERM, beende"),
            _ = tokio::signal::ctrl_c() => eprintln!("meshd: SIGINT, beende"),
        }
        n.shutdown();
    });
}

/// Port fuer eingehende Verbindungen: --listen, sonst Datei `listen` im Zustand, sonst 8421; 0 = keine.
/// Eine unlesbare Datei zaehlt wie keine (mit Warnung), damit ein Tippfehler den Knoten nicht abschaltet.
fn listen_port(arg: Option<u16>, file: &std::path::Path) -> Option<u16> {
    let port = arg.unwrap_or_else(|| match std::fs::read_to_string(file) {
        Ok(s) => s.trim().parse().unwrap_or_else(|_| {
            dlog::ev("warn", "listen_file_invalid", json!({"path": file.display().to_string(), "content": s.trim()}));
            node::LISTEN_PORT
        }),
        Err(_) => node::LISTEN_PORT,
    });
    (port != 0).then_some(port)
}

fn cmd_status(args: Vec<String>) {
    let mut status = "/run/meshd.json".to_string();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--status" => status = it.next().unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    match std::fs::read_to_string(&status) {
        Ok(s) => print!("{s}"),
        Err(_) => {
            println!("{}", json!({"running": false}));
            std::process::exit(1);
        }
    }
}

/// meshd ext-probe [SOCKET] [NET] [PEER-PREFIX]: Pruefung der lokalen Schnittstelle (ext.rs) am laufenden meshd. Bestellt `cr`,
/// sammelt 25 s lang die Angebote (offer) der verbundenen Arcades und fragt die erste, die NET (Standard "test")
/// teilt, nach getblockcount und getbestblockhash (nur lesen). Gibt eine JSON-Zeile je Ereignis aus.
fn cmd_ext_probe(args: Vec<String>) {
    let sock = args.first().cloned().unwrap_or_else(|| "/run/meshd/ext.sock".into());
    let net = args.get(1).cloned().unwrap_or_else(|| "test".into());
    let only = args.get(2).cloned().unwrap_or_default(); // nur diesen Knoten fragen (Anfang seiner Kennung)
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    rt.block_on(async move {
        use tokio::io::AsyncWriteExt;
        let s = match tokio::net::UnixStream::connect(&sock).await {
            Ok(s) => s,
            Err(e) => {
                println!("{}", json!({"error": format!("{sock}: {e}")}));
                std::process::exit(1);
            }
        };
        let (mut r, mut w) = s.into_split();
        w.write_all(&ext::encode(&json!({"op": "hello", "kinds": ["cr"]}), b"")).await.unwrap();
        let start = Instant::now();
        let mut offers: BTreeMap<String, Value> = BTreeMap::new();
        let mut asked: Option<(String, Instant)> = None;
        loop {
            let left = Duration::from_secs(if asked.is_some() { 60 } else { 25 }).saturating_sub(start.elapsed());
            let got = match timeout(left, ext::read_local(&mut r)).await {
                Ok(Ok(x)) => x,
                Ok(Err(e)) => {
                    println!("{}", json!({"error": e.to_string()}));
                    break;
                }
                Err(_) => {
                    println!("{}", json!({"done": "time", "offers": offers.len(), "asked": asked.as_ref().map(|a| &a.0[..16])}));
                    break;
                }
            };
            let (h, blob) = got;
            match h["op"].as_str() {
                Some("frame") if h["h"]["op"] == "offer" => {
                    let peer = h["peer"].as_str().unwrap_or("").to_string();
                    let nets: Vec<&String> = h["h"]["nets"].as_object().map(|m| m.keys().collect()).unwrap_or_default();
                    println!("{}", json!({"offer": &peer[..16.min(peer.len())], "nets": nets}));
                    offers.insert(peer.clone(), h["h"]["nets"].clone());
                    if asked.is_none() && peer.starts_with(&only) && h["h"]["nets"].get(&net).is_some() {
                        let q = json!({"t": "cr", "op": "q", "id": 1, "net": net, "seq": 0, "end": true});
                        let calls = std::env::var("MESHD_PROBE_CALLS").unwrap_or_else(|_| r#"[["getblockcount",[]],["getbestblockhash",[]]]"#.into());
                        let calls = calls.as_bytes();
                        w.write_all(&ext::encode(&json!({"op": "send", "peer": peer, "h": q}), calls)).await.unwrap();
                        println!("{}", json!({"asked": &peer[..16], "net": net}));
                        asked = Some((peer, Instant::now()));
                    }
                }
                Some("frame") if h["h"]["op"] == "a" && h["h"]["id"] == 1 => {
                    let ms = asked.as_ref().map(|a| a.1.elapsed().as_millis());
                    let body: Value = serde_json::from_slice(&blob).unwrap_or(Value::Null);
                    println!("{}", json!({"answer_from": h["peer"].as_str().map(|p| &p[..16]), "ms": ms, "head": h["h"], "answers": body}));
                    break;
                }
                Some("hello") | Some("up") | Some("down") => println!("{}", json!({"event": h})),
                Some("frame") => println!("{}", json!({"other_cr": h["h"], "from": h["peer"].as_str().map(|p| &p[..16]), "blob_len": blob.len()})),
                _ => {}
            }
        }
    });
}

/// meshd room-probe SOCKET ROOM SECONDS: Pruefung der Raeume am laufenden meshd. Tritt ROOM als guest-<zufall>
/// bei, sagt alle 5 s eine Zeile, gibt jedes Ereignis als JSON-Zeile aus und geht nach SECONDS wieder.
fn cmd_room_probe(args: Vec<String>) {
    let sock = args.first().cloned().unwrap_or_else(|| "/run/meshd/ext.sock".into());
    let room = args.get(1).cloned().unwrap_or_else(|| "arcadeos-test/lobby".into());
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    rt.block_on(async move {
        use tokio::io::AsyncWriteExt;
        let s = tokio::net::UnixStream::connect(&sock).await.expect("socket");
        let (mut r, mut w) = s.into_split();
        let mut b = [0u8; 4];
        rand::rngs::OsRng.fill_bytes(&mut b);
        let me = format!("guest-{}", hex::encode(b));
        w.write_all(&ext::encode(&json!({"op": "hello", "kinds": ["rooms"]}), b"")).await.unwrap();
        w.write_all(&ext::encode(&json!({"op": "room", "do": "join", "id": 1, "room": room, "member": me}), b"")).await.unwrap();
        let start = Instant::now();
        let mut said = 0u64;
        let mut next = Instant::now() + Duration::from_secs(3);
        while start.elapsed() < Duration::from_secs(secs) {
            if Instant::now() >= next {
                said += 1;
                let line = format!("hello {said} from {me}");
                w.write_all(&ext::encode(&json!({"op": "room", "do": "send", "id": 100 + said, "room": room, "member": me}), line.as_bytes())).await.unwrap();
                w.write_all(&ext::encode(&json!({"op": "room", "do": "touch", "id": 0, "room": room, "member": me}), b"")).await.unwrap();
                next = Instant::now() + Duration::from_secs(5);
            }
            match timeout(Duration::from_millis(500), ext::read_local(&mut r)).await {
                Ok(Ok((h, blob))) if h["op"] == "room-event" || (h["op"] == "room-reply" && h["id"] != 0) => {
                    let mut h = h;
                    if !blob.is_empty() {
                        h["data"] = json!(String::from_utf8_lossy(&blob));
                    }
                    if let Some(n) = h["node"].as_str() {
                        h["node"] = json!(&n[..16.min(n.len())]);
                    }
                    println!("{h}");
                }
                Ok(Err(e)) => {
                    println!("{}", json!({"error": e.to_string()}));
                    break;
                }
                _ => {}
            }
        }
        let _ = w.write_all(&ext::encode(&json!({"op": "room", "do": "leave", "id": 2, "room": room, "member": me}), b"")).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    });
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => return cmd_run(args[1..].to_vec()),
        Some("status") => return cmd_status(args[1..].to_vec()),
        Some("ext-probe") => return cmd_ext_probe(args[1..].to_vec()),
        Some("room-probe") => return cmd_room_probe(args[1..].to_vec()),
        Some("probe") => {}
        _ => usage(),
    }
    let mut net = "arcade-main".to_string();
    let mut pings = 20usize;
    let mut targets = Vec::new();
    let mut it = args.into_iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--net" => net = it.next().unwrap_or_else(|| usage()),
            "--pings" => pings = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            _ => targets.push(a),
        }
    }
    if targets.is_empty() {
        usage();
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    rt.block_on(async {
        let key = SigningKey::generate(&mut OsRng);
        let tasks: Vec<_> = targets
            .into_iter()
            .map(|t| tokio::spawn(probe(t, net.clone(), pings, key.clone())))
            .collect();
        for t in tasks {
            if let Ok(v) = t.await {
                println!("{v}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::listen_port;

    #[test]
    fn listen_port_from_arg_file_or_default() {
        let dir = std::env::temp_dir().join(format!("meshd-listen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("listen");
        assert_eq!(listen_port(None, &f), Some(8421), "ohne Datei: Standardport");
        std::fs::write(&f, "9000\n").unwrap();
        assert_eq!(listen_port(None, &f), Some(9000));
        assert_eq!(listen_port(Some(7000), &f), Some(7000), "--listen geht vor");
        std::fs::write(&f, "0").unwrap();
        assert_eq!(listen_port(None, &f), None, "0 = nur ausgehend");
        std::fs::write(&f, "quatsch").unwrap();
        assert_eq!(listen_port(None, &f), Some(8421), "unlesbar: Standardport");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
