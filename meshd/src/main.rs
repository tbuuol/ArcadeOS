//! meshd – ArcadeOS Mesh-Dienst (Arcade-Mesh-Protokoll v2).
//!
//! Stand: Link-Schicht. Befehl:
//!   meshd probe [--net arcade-main] [--pings N] host:port ...
//! verbindet sich mit jedem Knoten (frischer Schluessel, lauscht nicht), misst TCP-Aufbau, Handshake
//! und Ping-Antwortzeiten, sammelt die Adressen, die der Knoten nennt, und gibt je Knoten eine
//! JSON-Zeile aus.

mod link;

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use rand::RngCore;
use serde_json::{json, Value};
use tokio::net::TcpStream;
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
    let stream = match timeout(Duration::from_secs(8), TcpStream::connect(&addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return json!({"addr": addr, "error": format!("tcp: {e}")}),
        Err(_) => return json!({"addr": addr, "error": "tcp: Zeitlimit"}),
    };
    out["tcp_ms"] = json!(round1(t0.elapsed().as_secs_f64() * 1000.0));
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
    eprintln!("Aufruf: meshd probe [--net arcade-main] [--pings N] host:port ...");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("probe") {
        usage();
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
