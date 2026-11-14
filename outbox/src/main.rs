//! outboxd: the approval tray. Transactions are prepared and signed on the box (by Claude or a tool), but they
//! only go out when the owner taps "Senden" on the approval page and gives the PIN. Nothing is sent otherwise.
//!
//!   outboxd serve                       HTTP on OUTBOX_BIND (127.0.0.1:8093), behind Caddy at /freigabe/ (LAN only)
//!   outboxd prepare-kotia --to ADDR --amount KOT [--data HEX] --title TEXT [--why TEXT]
//!                                       build + sign from the "claude-arbeit" account, put it in the tray
//!   outboxd list                        what waits and what was done
//!
//! Files in OUTBOX_DIR (/state/outbox, root only): pending/<id>.json, done/<id>.json, pin (salt + sha256),
//! log. A request older than 24 h can no longer be approved. Wrong PINs: at most 5 per 15 minutes.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const PAGE: &str = include_str!("page.html");
const MAX_AGE: u64 = 24 * 3600;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("OUTBOX_DIR").unwrap_or_else(|_| "/state/outbox".into()))
}

fn log(s: &str) {
    let line = format!("{} {s}\n", now());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("log")) {
        let _ = f.write_all(line.as_bytes());
    }
    eprint!("{line}");
}

/// The node command line of a chain (its cli wrapper on this box).
fn cli(chain: &str) -> Option<&'static str> {
    Some(match chain {
        "kotia" => "/state/kotia/kotia-cli",
        "pepecoin-test" => "/state/pepecoin/pepecoin-cli",
        "pepecoin" => "/state/pepecoin-main/pepecoin-cli",
        "litecoin" => "/state/litecoin/litecoin-cli",
        "litecoin-test" => "/state/litecoin-test/litecoin-cli",
        "dogecoin" => "/state/dogecoin/dogecoin-cli",
        _ => return None,
    })
}

fn run(chain: &str, args: &[&str]) -> Result<String, String> {
    let c = cli(chain).ok_or("unknown chain")?;
    let o = Command::new("timeout").arg("60").arg(c).args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    hex::encode(b)
}

fn read_json(p: &PathBuf) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn write_private(p: &PathBuf, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = p.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(tmp, p)
}

fn list_dir(sub: &str) -> Vec<Value> {
    let mut v: Vec<Value> = std::fs::read_dir(dir().join(sub)).into_iter().flatten().flatten().filter_map(|e| read_json(&e.path())).collect();
    v.sort_by_key(|x| std::cmp::Reverse(x["created"].as_u64().unwrap_or(0)));
    v
}

// ---------------------------------------------------------------- PIN
fn pin_hash(salt: &str, pin: &str) -> String {
    hex::encode(Sha256::digest(format!("{salt}:{pin}").as_bytes()))
}

fn pin_ok(pin: &str) -> bool {
    let Ok(s) = std::fs::read_to_string(dir().join("pin")) else { return false };
    let Some((salt, h)) = s.trim().split_once(':') else { return false };
    let got = pin_hash(salt, pin);
    // constant-time compare
    got.len() == h.len() && got.bytes().zip(h.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
}

static FAILS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

fn too_many_fails() -> bool {
    let mut f = FAILS.lock().unwrap();
    let t = now();
    f.retain(|x| t - *x < 900);
    f.len() >= 5
}

fn failed() {
    FAILS.lock().unwrap().push(now());
}

// ---------------------------------------------------------------- actions
fn api(method: &str, path: &str, body: &Value) -> (u16, Value) {
    match (method, path) {
        ("GET", "/api/list") => {
            let pend: Vec<Value> = list_dir("pending")
                .into_iter()
                .map(|mut r| {
                    let age = now().saturating_sub(r["created"].as_u64().unwrap_or(0));
                    r["expired"] = json!(age > MAX_AGE);
                    r.as_object_mut().map(|o| o.remove("hex"));
                    r
                })
                .collect();
            let done: Vec<Value> = list_dir("done").into_iter().take(20).map(|mut r| {
                r.as_object_mut().map(|o| o.remove("hex"));
                r
            }).collect();
            (200, json!({"pending": pend, "done": done, "pin_set": dir().join("pin").exists()}))
        }
        ("POST", "/api/pin") => {
            let new = body["new"].as_str().unwrap_or("");
            if new.len() < 6 || !new.chars().all(|c| c.is_ascii_digit()) {
                return (400, json!({"error": "Die PIN braucht mindestens 6 Ziffern."}));
            }
            if dir().join("pin").exists() {
                if too_many_fails() {
                    return (429, json!({"error": "Zu viele falsche PINs, bitte 15 Minuten warten."}));
                }
                if !pin_ok(body["old"].as_str().unwrap_or("")) {
                    failed();
                    return (403, json!({"error": "Die alte PIN stimmt nicht."}));
                }
            }
            let salt = rand_hex(16);
            match write_private(&dir().join("pin"), format!("{salt}:{}", pin_hash(&salt, new)).as_bytes()) {
                Ok(_) => {
                    log("PIN gesetzt");
                    (200, json!({"ok": true}))
                }
                Err(e) => (500, json!({"error": e.to_string()})),
            }
        }
        ("POST", "/api/approve") | ("POST", "/api/reject") => {
            let approve = path.ends_with("approve");
            if too_many_fails() {
                return (429, json!({"error": "Zu viele falsche PINs, bitte 15 Minuten warten."}));
            }
            if !pin_ok(body["pin"].as_str().unwrap_or("")) {
                failed();
                log("falsche PIN");
                return (403, json!({"error": "PIN falsch."}));
            }
            let id = body["id"].as_str().unwrap_or("");
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
                return (400, json!({"error": "bad id"}));
            }
            let p = dir().join("pending").join(format!("{id}.json"));
            let Some(mut r) = read_json(&p) else { return (404, json!({"error": "Auftrag nicht gefunden."})) };
            if approve && now().saturating_sub(r["created"].as_u64().unwrap_or(0)) > MAX_AGE {
                return (410, json!({"error": "Auftrag ist älter als 24 Stunden. Bitte neu vorbereiten lassen."}));
            }
            if approve {
                let chain = r["chain"].as_str().unwrap_or("").to_string();
                let hexs = r["hex"].as_str().unwrap_or("").to_string();
                match run(&chain, &["sendrawtransaction", &hexs]) {
                    Ok(txid) => {
                        r["state"] = json!("gesendet");
                        r["txid"] = json!(txid);
                        log(&format!("FREIGEGEBEN und gesendet {id} ({}) txid {txid}", r["title"]));
                    }
                    Err(e) => {
                        r["state"] = json!("fehlgeschlagen");
                        r["error"] = json!(e);
                        log(&format!("freigegeben, aber Senden fehlgeschlagen {id}: {e}"));
                    }
                }
            } else {
                r["state"] = json!("verworfen");
                log(&format!("verworfen {id} ({})", r["title"]));
            }
            r["decided"] = json!(now());
            let out = dir().join("done").join(format!("{id}.json"));
            let _ = write_private(&out, r.to_string().as_bytes());
            let _ = std::fs::remove_file(&p);
            r.as_object_mut().map(|o| o.remove("hex"));
            (200, r)
        }
        _ => (404, json!({"error": "not found"})),
    }
}

fn handle(mut s: TcpStream) -> std::io::Result<()> {
    s.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut it = line.split_whitespace();
    let (method, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("/").to_string());
    let (mut len, mut origin, mut host) = (0usize, String::new(), String::new());
    loop {
        let mut h = String::new();
        r.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "origin" => origin = v.trim().to_string(),
                "x-forwarded-host" | "host" if host.is_empty() || k.eq_ignore_ascii_case("x-forwarded-host") => host = v.trim().to_string(),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; len.min(64 * 1024)];
    r.read_exact(&mut body)?;
    let path = path.split('?').next().unwrap_or("/").to_string();
    let (code, ctype, out): (u16, &str, Vec<u8>) = if method == "GET" && (path == "/" || path.is_empty()) {
        (200, "text/html; charset=utf-8", PAGE.as_bytes().to_vec())
    } else if path.starts_with("/api/") {
        // a POST must come from this page (no other site may make the browser approve)
        if method == "POST" && !origin.is_empty() && !origin.ends_with(&format!("//{host}")) {
            (403, "application/json", br#"{"error":"fremde Seite"}"#.to_vec())
        } else {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let (c, v) = api(&method, &path, &b);
            (c, "application/json", v.to_string().into_bytes())
        }
    } else {
        (404, "text/plain", b"not found".to_vec())
    };
    write!(s, "HTTP/1.1 {code} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", out.len())?;
    s.write_all(&out)
}

// ---------------------------------------------------------------- prepare (Kotia)
fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn prepare_kotia(args: &[String]) -> Result<Value, String> {
    let to = arg(args, "--to").ok_or("--to fehlt")?;
    let amount: f64 = arg(args, "--amount").ok_or("--amount fehlt")?.parse().map_err(|_| "--amount: keine Zahl")?;
    let title = arg(args, "--title").ok_or("--title fehlt")?;
    let why = arg(args, "--why").unwrap_or_default();
    let data = arg(args, "--data");
    let from = std::fs::read_to_string(dir().join("kotia-claude.address")).map_err(|e| e.to_string())?.trim().to_string();
    let v: Value = serde_json::from_str(&run("kotia", &["validateaddress", &to])?).map_err(|e| e.to_string())?;
    if v["isvalid"] != true {
        return Err(format!("{to} ist keine gültige Kotia-Adresse"));
    }
    let utxos: Value = serde_json::from_str(&run("kotia", &["listunspent", "1", "9999999", &json!([from]).to_string()])?).map_err(|e| e.to_string())?;
    let mut coins: Vec<&Value> = utxos.as_array().map(|a| a.iter().collect()).unwrap_or_default();
    coins.sort_by(|a, b| b["amount"].as_f64().partial_cmp(&a["amount"].as_f64()).unwrap());
    let sats = |x: f64| (x * 1e8).round() as i64;
    let want = sats(amount);
    let (mut picked, mut sum) = (Vec::new(), 0i64);
    let mut fee = 0;
    for c in coins {
        picked.push(json!({"txid": c["txid"], "vout": c["vout"]}));
        sum += sats(c["amount"].as_f64().unwrap_or(0.0));
        let size = 10 + 150 * picked.len() as i64 + 34 * 2 + data.as_ref().map_or(0, |d| d.len() as i64 / 2 + 12);
        fee = (size * 100_000 / 1000).max(100_000); // 0.001 KOT per kB, at least 0.001 KOT
        if sum >= want + fee {
            break;
        }
    }
    if sum < want + fee {
        return Err(format!("zu wenig Guthaben auf {from}: {} KOT, gebraucht {} KOT", sum as f64 / 1e8, (want + fee) as f64 / 1e8));
    }
    let change = sum - want - fee;
    let mut outs = serde_json::Map::new();
    outs.insert(to.clone(), json!(want as f64 / 1e8));
    if change > 0 {
        let prev = outs.get(&from).and_then(|x| x.as_f64()).unwrap_or(0.0);
        outs.insert(from.clone(), json!(prev + change as f64 / 1e8));
    }
    if let Some(d) = &data {
        outs.insert("data".into(), json!(d));
    }
    let raw = run("kotia", &["createrawtransaction", &Value::Array(picked.clone()).to_string(), &Value::Object(outs).to_string()])?;
    let signed: Value = serde_json::from_str(&run("kotia", &["signrawtransaction", &raw])?).map_err(|e| e.to_string())?;
    if signed["complete"] != true {
        return Err(format!("Signatur unvollständig: {}", signed["errors"]));
    }
    let hexs = signed["hex"].as_str().unwrap_or("").to_string();
    // check what we built says what we meant
    let dec: Value = serde_json::from_str(&run("kotia", &["decoderawtransaction", &hexs])?).map_err(|e| e.to_string())?;
    let paid: f64 = dec["vout"].as_array().into_iter().flatten()
        .filter(|o| o["scriptPubKey"]["addresses"].as_array().is_some_and(|a| a.iter().any(|x| x == &json!(to))))
        .map(|o| o["value"].as_f64().unwrap_or(0.0)).sum();
    if sats(paid) != want {
        return Err(format!("Prüfung: die Transaktion zahlt {paid} statt {amount} an {to}"));
    }
    let id = rand_hex(8);
    let r = json!({
        "id": id, "created": now(), "chain": "kotia", "title": title, "why": why,
        "from": from, "to": to, "amount": amount, "fee": fee as f64 / 1e8, "change": change as f64 / 1e8,
        "data": data, "inputs": picked.len(), "size": hexs.len() / 2, "txid": dec["txid"], "state": "wartet", "hex": hexs,
    });
    std::fs::create_dir_all(dir().join("pending")).map_err(|e| e.to_string())?;
    write_private(&dir().join("pending").join(format!("{id}.json")), r.to_string().as_bytes()).map_err(|e| e.to_string())?;
    log(&format!("vorbereitet {id}: {title} ({amount} KOT an {to}, Gebühr {})", fee as f64 / 1e8));
    let mut shown = r.clone();
    shown.as_object_mut().map(|o| o.remove("hex"));
    Ok(shown)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let _ = std::fs::create_dir_all(dir().join("pending"));
    let _ = std::fs::create_dir_all(dir().join("done"));
    match args.get(1).map(|s| s.as_str()) {
        Some("serve") => {
            let bind = std::env::var("OUTBOX_BIND").unwrap_or_else(|_| "127.0.0.1:8093".into());
            let l = TcpListener::bind(&bind).unwrap_or_else(|e| {
                eprintln!("outboxd: {bind}: {e}");
                std::process::exit(1)
            });
            eprintln!("outboxd: Freigabe-Fach auf {bind}");
            for s in l.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = handle(s);
                });
            }
        }
        Some("prepare-kotia") => match prepare_kotia(&args[2..]) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => {
                eprintln!("outboxd: {e}");
                std::process::exit(1)
            }
        },
        Some("list") => println!("{}", serde_json::to_string_pretty(&api("GET", "/api/list", &Value::Null).1).unwrap()),
        _ => {
            eprintln!("outboxd serve | prepare-kotia --to ADDR --amount KOT --title TEXT [--why TEXT] [--data HEX] | list");
            std::process::exit(2)
        }
    }
}
