//! Raeume fuer App-Fenster (Stufe D der App-Laufzeit): das App-Fenster einer freigeschalteten App tritt fuer seine
//! Seite einem Raum bei; meshd (rooms.rs) bringt ihn ins Mesh. Der Raum im Mesh heisst "<Seite>/<Raum>": jede App
//! bleibt in ihren eigenen Raeumen, wie bei den Arcades (web/realtime.py room_key).
//!
//! Spieler sind vorerst Gaeste (guest-<8 hex>, ohne Nachweis; die Arcades zeigen Gaeste ohne Nachweis). Ein Platz
//! (Sitz) gehoert zu einer Marke (token), die nur das App-Fenster kennt; die Seite in der Sandbox sieht sie nie.
//!
//! HTTP nur auf 127.0.0.1 (ADMIN_RT_LISTEN, Standard :8099; Caddy reicht /rt/* der Arcade-Seite hierher), ein Faden
//! je Anfrage (Long-Polling haelt Verbindungen offen):
//!   POST /rt/join  {page, room}      -> {ok, token, me, members, online}
//!   POST /rt/send  {token, data}     (hoechstens 512 Bytes)
//!   POST /rt/leave {token}
//!   GET  /rt/poll?token=&wait=25     -> {ok, events: [{type, member, data}]}; haelt den Platz am Leben
//! Ein Platz ohne Abfrage seit 40 s wird geraeumt.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::{log, App};

const IDLE: Duration = Duration::from_secs(40);
const TOUCH_EVERY: Duration = Duration::from_secs(8);
const MAX_SEATS: usize = 2000;
const ROOM_MAX: usize = 60;
const QUEUE: usize = 500;

struct Seat {
    mesh_room: String,
    room: String,
    member: String,
    events: VecDeque<Value>,
    polled: Instant,
}

#[derive(Default)]
struct St {
    seats: HashMap<String, Seat>,
    replies: HashMap<u64, mpsc::Sender<Value>>,
    next: u64,
    node: String,
}

fn st() -> &'static Mutex<St> {
    static S: OnceLock<Mutex<St>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn writer() -> &'static Mutex<Option<UnixStream>> {
    static W: OnceLock<Mutex<Option<UnixStream>>> = OnceLock::new();
    W.get_or_init(Default::default)
}

fn encode(h: &Value, blob: &[u8]) -> Vec<u8> {
    let head = serde_json::to_vec(h).unwrap();
    let mut v = ((2 + head.len() + blob.len()) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(&(head.len() as u16).to_be_bytes());
    v.extend_from_slice(&head);
    v.extend_from_slice(blob);
    v
}

fn read_local(r: &mut UnixStream) -> std::io::Result<(Value, Vec<u8>)> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    if !(2..=80 * 1024).contains(&n) {
        return Err(std::io::Error::other("size"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    let hl = u16::from_be_bytes([b[0], b[1]]) as usize;
    let h: Value = serde_json::from_slice(b.get(2..2 + hl).ok_or_else(|| std::io::Error::other("head"))?).map_err(std::io::Error::other)?;
    Ok((h, b[2 + hl..].to_vec()))
}

/// Ein Raum-Befehl an meshd, mit Antwort (oder Fehler nach 10 s)
fn ask(mut h: Value, blob: &[u8]) -> Value {
    let (tx, rx) = mpsc::channel();
    let id = {
        let mut g = st().lock().unwrap();
        g.next += 1;
        let n = g.next;
        g.replies.insert(n, tx);
        g.next
    };
    h["op"] = json!("room");
    h["id"] = json!(id);
    let sent = writer().lock().unwrap().as_mut().is_some_and(|w| w.write_all(&encode(&h, blob)).is_ok());
    let r = if sent { rx.recv_timeout(Duration::from_secs(10)).unwrap_or(json!({"ok": false, "error": "meshd did not answer"})) } else { json!({"ok": false, "error": "the mesh is not reachable from this arcade right now"}) };
    st().lock().unwrap().replies.remove(&id);
    r
}

fn member_of(m: &str, node: &str) -> Value {
    json!({"id": m, "node": &node[..16.min(node.len())]})
}

/// Ereignis von meshd an alle Plaetze in diesem Raum (ausser dem, der es selbst ausgeloest hat)
fn deliver(h: &Value, blob: &[u8]) {
    let room = h["room"].as_str().unwrap_or("");
    let member = h["member"].as_str().unwrap_or("");
    let node = h["node"].as_str().unwrap_or("");
    let mut g = st().lock().unwrap();
    let me = g.node.clone();
    for s in g.seats.values_mut().filter(|s| s.mesh_room == room && !(s.member == member && node == me)) {
        let mut e = json!({"room": s.room, "type": h["type"], "member": member_of(member, node)});
        if h["type"] == "message" {
            e["data"] = json!(String::from_utf8_lossy(blob));
        }
        if s.events.len() >= QUEUE {
            s.events.pop_front();
        }
        s.events.push_back(e);
    }
}

/// Die Verbindung zu meshd (Art "rooms"); nach einem Abbruch werden alle Plaetze wieder angemeldet
fn mesh_loop() {
    loop {
        let path = std::env::var("ADMIN_MESHD_EXT").unwrap_or_else(|_| "/run/meshd/ext.sock".into());
        if let Ok(mut s) = UnixStream::connect(&path) {
            let _ = s.write_all(&encode(&json!({"op": "hello", "kinds": ["rooms"]}), b""));
            *writer().lock().unwrap() = s.try_clone().ok();
            // Plaetze von vorher wieder hinein (meshd wurde neu gestartet)
            let again: Vec<(String, String, String)> = st().lock().unwrap().seats.iter().map(|(t, s)| (t.clone(), s.mesh_room.clone(), s.member.clone())).collect();
            if !again.is_empty() {
                std::thread::spawn(move || {
                    for (t, room, member) in again {
                        let r = ask(json!({"do": "join", "room": room, "member": member}), b"");
                        if let Some(s) = st().lock().unwrap().seats.get_mut(&t) {
                            s.events.push_back(json!({"room": s.room, "type": if r["ok"] == true { "rejoined" } else { "closed" }, "member": {"id": s.member}}));
                        }
                    }
                });
            }
            loop {
                let Ok((h, blob)) = read_local(&mut s) else { break };
                match h["op"].as_str() {
                    Some("hello") => st().lock().unwrap().node = h["node"].as_str().unwrap_or("").to_string(),
                    Some("room-reply") => {
                        let id = h["id"].as_u64().unwrap_or(0);
                        if let Some(tx) = st().lock().unwrap().replies.remove(&id) {
                            let _ = tx.send(h);
                        }
                    }
                    Some("room-event") => deliver(&h, &blob),
                    _ => {}
                }
            }
            *writer().lock().unwrap() = None;
            for s in st().lock().unwrap().seats.values_mut() {
                s.events.push_back(json!({"room": s.room, "type": "reconnecting", "member": {"id": s.member}}));
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Alle TOUCH_EVERY: Plaetze, die abgefragt werden, bei meshd am Leben halten; verlassene raeumen
fn keeper() {
    loop {
        std::thread::sleep(TOUCH_EVERY);
        let (live, gone): (Vec<(String, String)>, Vec<(String, String, String)>) = {
            let mut g = st().lock().unwrap();
            let gone: Vec<String> = g.seats.iter().filter(|(_, s)| s.polled.elapsed() > IDLE).map(|(t, _)| t.clone()).collect();
            let gone: Vec<(String, String, String)> = gone.into_iter().filter_map(|t| g.seats.remove(&t).map(|s| (t, s.mesh_room, s.member))).collect();
            (g.seats.values().map(|s| (s.mesh_room.clone(), s.member.clone())).collect(), gone)
        };
        for (room, member) in live {
            let _ = ask(json!({"do": "touch", "room": room, "member": member}), b"");
        }
        for (_, room, member) in gone {
            let _ = ask(json!({"do": "leave", "room": room, "member": member}), b"");
        }
    }
}

fn token() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).ok();
    hex::encode(b)
}

fn join(app: &App, b: &Value) -> (u16, Value) {
    let page = b["page"].as_str().unwrap_or("").to_ascii_lowercase();
    let room = b["room"].as_str().unwrap_or("");
    if room.is_empty() || room.chars().count() > ROOM_MAX || room.chars().any(|c| (c as u32) < 32) {
        return (400, json!({"ok": false, "error": format!("a room is a name of 1 to {ROOM_MAX} characters")}));
    }
    // nur fuer Seiten freigeschalteter Apps
    let allowed = std::env::var("ADMIN_RT_ANY_PAGE").as_deref() == Ok("1") && std::env::var("ADMIN_ONLY").as_deref() == Ok("share")
        || crate::apps::public_list(app)["apps"].as_array().is_some_and(|a| a.iter().any(|x| x["page"] == page.as_str()));
    if !allowed {
        return (403, json!({"ok": false, "error": "this app is not enabled on this arcade"}));
    }
    if st().lock().unwrap().seats.len() >= MAX_SEATS {
        return (503, json!({"ok": false, "error": "this arcade has no room for more players right now"}));
    }
    let mut r = [0u8; 4];
    getrandom::getrandom(&mut r).ok();
    let member = format!("guest-{}", hex::encode(r));
    let mesh_room = format!("{page}/{room}");
    let said = ask(json!({"do": "join", "room": mesh_room, "member": member}), b"");
    if said["ok"] != true {
        return (200, json!({"ok": true, "online": false, "why": said["error"]}));
    }
    let node = said["node"].as_str().unwrap_or("").to_string();
    let members: Vec<Value> = said["members"].as_array().into_iter().flatten()
        .map(|m| member_of(m["member"].as_str().unwrap_or(""), m["node"].as_str().unwrap_or(&node))).collect();
    let t = token();
    st().lock().unwrap().seats.insert(t.clone(), Seat { mesh_room, room: room.to_string(), member: member.clone(), events: VecDeque::new(), polled: Instant::now() });
    (200, json!({"ok": true, "online": true, "token": t, "me": member_of(&member, &node), "members": members}))
}

fn seat_of(b: &Value) -> Option<(String, String)> {
    let g = st().lock().unwrap();
    g.seats.get(b["token"].as_str()?).map(|s| (s.mesh_room.clone(), s.member.clone()))
}

fn handle(app: &App, method: &str, path: &str, query: &str, b: &Value) -> (u16, Value) {
    match (method, path) {
        ("POST", "/rt/join") => join(app, b),
        ("POST", "/rt/send") => {
            let Some((room, member)) = seat_of(b) else { return (404, json!({"ok": false, "error": "no such seat"})) };
            let data = b["data"].as_str().unwrap_or("");
            if data.len() > 512 {
                return (400, json!({"ok": false, "error": "a message is at most 512 bytes"}));
            }
            let r = ask(json!({"do": "send", "room": room, "member": member}), data.as_bytes());
            (200, r)
        }
        ("POST", "/rt/leave") => {
            let t = b["token"].as_str().unwrap_or("").to_string();
            let seat = st().lock().unwrap().seats.remove(&t);
            if let Some(s) = seat {
                let _ = ask(json!({"do": "leave", "room": s.mesh_room, "member": s.member}), b"");
            }
            (200, json!({"ok": true}))
        }
        ("GET", "/rt/poll") => {
            let t = query.split('&').find_map(|kv| kv.strip_prefix("token=")).unwrap_or("").to_string();
            let wait = query.split('&').find_map(|kv| kv.strip_prefix("wait=")).and_then(|w| w.parse().ok()).unwrap_or(25u64).min(30);
            let until = Instant::now() + Duration::from_secs(wait);
            loop {
                {
                    let mut g = st().lock().unwrap();
                    let Some(s) = g.seats.get_mut(&t) else { return (404, json!({"ok": false, "error": "no such seat"})) };
                    s.polled = Instant::now();
                    if !s.events.is_empty() || Instant::now() >= until {
                        let ev: Vec<Value> = s.events.drain(..).collect();
                        return (200, json!({"ok": true, "events": ev}));
                    }
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
        _ => (404, json!({"ok": false, "error": "unknown"})),
    }
}

pub fn serve(app: Arc<App>, listen: String) {
    if !listen.starts_with("127.0.0.1:") {
        return log("rt: nur auf 127.0.0.1");
    }
    let server = match tiny_http::Server::http(&listen) {
        Ok(s) => s,
        Err(e) => return log(&format!("rt: kann nicht auf {listen} lauschen: {e}")),
    };
    std::thread::spawn(mesh_loop);
    std::thread::spawn(keeper);
    log(&format!("Raeume fuer App-Fenster auf {listen}"));
    for mut req in server.incoming_requests() {
        let app = app.clone();
        std::thread::spawn(move || {
            let mut body = String::new();
            let _ = req.as_reader().take(8 * 1024).read_to_string(&mut body);
            let b: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let url = req.url().to_string();
            let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
            let (code, v) = handle(&app, req.method().as_str(), path, query, &b);
            let h = |s: &str| s.parse::<tiny_http::Header>().unwrap();
            let _ = req.respond(tiny_http::Response::from_string(v.to_string()).with_status_code(code)
                .with_header(h("Content-Type: application/json; charset=utf-8")).with_header(h("Cache-Control: no-store")));
        });
    }
}
