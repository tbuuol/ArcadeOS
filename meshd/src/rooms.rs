//! Raeume fuer die Spieler dieser Box (Arcade-Mesh v2, wie dogecoin-arcade mesh/node.py; eigener Code).
//!
//! Ein Raum ist ein Name (die Arcade bildet ihn als "<App>/<Raum>"). Wer Spieler in Raeumen hat, sagt das dem
//! ganzen Mesh mit einer signierten SUB (8-Byte-Fingerabdruecke der Raeume, BLAKE2b); SUBs fluten, und der Weg,
//! auf dem die SUB eines Knotens ankam, ist der Weg zurueck zu ihm. Raum-Nachrichten (PUB: msg, join, leave, here)
//! sind vom Ursprungsknoten signiert und gehen nur zu Knoten, die im Raum sind, je Weg eine Kopie mit "d" (die
//! Kurz-Kennungen der Knoten, fuer die sie ist).
//!
//! meshd bleibt Blatt: es merkt sich die Wege aus allen SUBs, schickt aber nur EIGENE SUBs und PUBs (fuer die
//! Spieler dieser Box) und nimmt nur PUBs an, die in unsere Raeume gehoeren; fremden Verkehr leitet es nicht weiter.
//! Signatur, Zeit (60 s) und Doppelte werden geprueft wie bei den Arcades.
//!
//! Wer ein Mitglied ist, sagt die Arcade (der lokale Dienst): ein Gast heisst guest-<8 hex>; ein angemeldeter
//! Spieler traegt einen Nachweis (cred), den die Knoten der anderen Arcades pruefen. meshd selbst zeigt seinen
//! Spielern jedes Mitglied, das ein Knoten mit gueltiger Signatur nennt (pruefen kann die Arcade darueber).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Value};

pub const MAX_PAYLOAD: usize = 512;
pub const MAX_ROOM: usize = 128;
pub const MAX_MEMBER: usize = 64;
const MAX_HERE: usize = 60 * 1024;
const MAX_HOPS: i64 = 16;
const MAX_ROOMS: usize = 2048;
const MAX_MEMBERS_PER_ROOM: usize = 256;
const RATE: f64 = 5.0;
const BURST: f64 = 10.0;
pub const IDLE: Duration = Duration::from_secs(10);
pub const LOCAL_GRACE: Duration = Duration::from_secs(30);
pub const SUB_EVERY: Duration = Duration::from_secs(30);
pub const HERE_EVERY: Duration = Duration::from_secs(8);
const HERE_FULL: u64 = 6;
const SKEW: f64 = 60.0;
const SEEN_MAX: usize = 50_000;
const SHORT: usize = 8;
const KINDS: [&str; 4] = ["msg", "join", "leave", "here"];

pub fn room_id(room: &str) -> String {
    hex::encode(blake2b_simd::Params::new().hash_length(8).hash(room.as_bytes()).as_bytes())
}

fn now_s() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Pythons json.dumps(h, separators=(",", ":"), sort_keys=True): Schluessel sortiert, ASCII mit \uXXXX
pub fn canon(v: &Value) -> String {
    fn s(out: &mut String, x: &str) {
        out.push('"');
        for c in x.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                    let mut b = [0u16; 2];
                    for u in c.encode_utf16(&mut b) {
                        out.push_str(&format!("\\u{:04x}", u));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    fn w(out: &mut String, v: &Value) {
        match v {
            Value::Object(m) => {
                out.push('{');
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    s(out, k);
                    out.push(':');
                    w(out, &m[*k]);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    w(out, x);
                }
                out.push(']');
            }
            Value::String(x) => s(out, x),
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    w(&mut out, v);
    out
}

fn signed_part(h: &Value) -> Value {
    let mut m = h.as_object().cloned().unwrap_or_default();
    for k in ["sig", "hops", "d"] {
        m.remove(k);
    }
    Value::Object(m)
}

fn verify(h: &Value, blob: &[u8]) -> bool {
    let (Some(o), Some(sig)) = (h["o"].as_str(), h["sig"].as_str()) else { return false };
    let (Ok(o), Ok(sig)) = (hex::decode(o), hex::decode(sig)) else { return false };
    let (Ok(o), Ok(sig)) = (<[u8; 32]>::try_from(o.as_slice()), <[u8; 64]>::try_from(sig.as_slice())) else { return false };
    let Ok(k) = VerifyingKey::from_bytes(&o) else { return false };
    let mut msg = canon(&signed_part(h)).into_bytes();
    msg.extend_from_slice(blob);
    k.verify(&msg, &Signature::from_bytes(&sig)).is_ok()
}

/// Ein binaerer Raum-Rahmen (FF FF 01 ...) als dasselbe Dict wie die JSON-Form, und die Nutzlast
pub fn unpack_pub(b: &[u8]) -> Option<(Value, Vec<u8>)> {
    if b.len() < 3 + 96 + 10 || b[..3] != [0xff, 0xff, 1] {
        return None;
    }
    let mut i = 3;
    let origin = hex::encode(&b[i..i + 32]);
    let sig = hex::encode(&b[i + 32..i + 96]);
    i += 96;
    let q = u64::from_be_bytes(b[i..i + 8].try_into().ok()?);
    let hops = b[i + 8];
    let kind = *KINDS.get(b[i + 9] as usize)?;
    i += 10;
    let n = *b.get(i)? as usize;
    let room = String::from_utf8(b.get(i + 1..i + 1 + n)?.to_vec()).ok()?;
    i += 1 + n;
    let n = *b.get(i)? as usize;
    let member = String::from_utf8(b.get(i + 1..i + 1 + n)?.to_vec()).ok()?;
    i += 1 + n;
    let n = u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize;
    let cred = b.get(i + 2..i + 2 + n)?;
    i += 2 + n;
    let n = u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize;
    i += 2;
    let mut h = json!({"t": "pub", "r": room, "o": origin, "q": q, "m": member, "k": kind, "hops": hops, "sig": sig});
    if !cred.is_empty() {
        h["c"] = serde_json::from_slice(cred).ok()?;
    }
    if n != 0xffff {
        let d: Option<Vec<String>> = (0..n).map(|k| b.get(i + 4 * k..i + 4 * k + 4).map(hex::encode)).collect();
        h["d"] = json!(d?);
        i += 4 * n;
    }
    Some((h, b.get(i..)?.to_vec()))
}

fn valid_name(x: &str, most: usize) -> bool {
    !x.is_empty() && x.chars().count() <= most && !x.chars().any(|c| (c as u32) < 32)
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * RATE).min(BURST);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct Local {
    cred: Option<Value>,
    bucket: Bucket,
    touched: Instant,
}

/// Was die Raumschicht dem lokalen Dienst sagt: (Art, Raum, Mitglied, Knoten, Nachweis, Nutzlast)
pub struct Event {
    pub kind: &'static str,
    pub room: String,
    pub member: String,
    pub node: String,
    pub cred: Option<Value>,
    pub data: Vec<u8>,
}

#[derive(Default)]
struct Remote {
    members: BTreeMap<String, Option<Value>>,
    until: Option<Instant>,
}

#[derive(Default)]
pub struct Rooms {
    /// Fingerabdruck -> Ursprung -> (ueber welche Verbindung, gueltig bis)
    routes: HashMap<String, HashMap<String, (String, Instant)>>,
    /// (Raum, Mitglied) dieser Box
    local: BTreeMap<(String, String), Local>,
    /// Raum -> Ursprung -> wer dort ist
    remote: HashMap<String, HashMap<String, Remote>>,
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
    seq: u64,
    sub_sent: Vec<String>,
    here_beat: u64,
    pub stats: BTreeMap<&'static str, u64>,
}

/// Was nach einer Aenderung hinaus muss: (an welche Verbindung, Kopf, Nutzlast); None = an alle Verbindungen
pub type Out = Vec<(Option<String>, Value, Vec<u8>)>;

impl Rooms {
    fn next_seq(&mut self) -> u64 {
        self.seq = (self.seq + 1).max((now_s() * 1_000_000.0) as u64);
        self.seq
    }

    fn mark(&mut self, key: String) {
        if self.seen.insert(key.clone()) {
            self.seen_order.push_back(key);
            while self.seen_order.len() > SEEN_MAX {
                if let Some(k) = self.seen_order.pop_front() {
                    self.seen.remove(&k);
                }
            }
        }
    }

    fn fresh(&self, h: &Value) -> bool {
        let Some(q) = h["q"].as_u64() else { return false };
        (q as f64 / 1_000_000.0 - now_s()).abs() <= SKEW && !self.seen.contains(&format!("{}:{}", h["o"].as_str().unwrap_or(""), q))
    }

    fn own_rooms(&self) -> Vec<String> {
        let set: HashSet<&String> = self.local.keys().map(|(r, _)| r).collect();
        set.into_iter().cloned().collect()
    }

    fn locals_in(&self, room: &str) -> Vec<String> {
        self.local.keys().filter(|(r, _)| r == room).map(|(_, m)| m.clone()).collect()
    }

    /// Eine SUB eines anderen Knotens: die Wege merken (nicht weitergeben: Blatt)
    pub fn on_sub(&mut self, via: &str, h: &Value, me: &str) -> bool {
        let origin = h["o"].as_str().unwrap_or("").to_string();
        let (Some(rooms), Some(hops)) = (h["rooms"].as_array(), h["hops"].as_i64()) else { return false };
        if origin == me || hops >= MAX_HOPS || !self.fresh(h) || !verify(h, b"") {
            return false;
        }
        self.mark(format!("{origin}:{}", h["q"]));
        let ids: HashSet<String> = rooms.iter().take(MAX_ROOMS).filter_map(|r| r.as_str()).filter(|r| r.len() == 16).map(String::from).collect();
        for (rid, m) in self.routes.iter_mut() {
            if !ids.contains(rid) {
                m.remove(&origin);
            }
        }
        self.routes.retain(|_, m| !m.is_empty());
        let until = Instant::now() + SUB_EVERY.mul_f64(3.5).max(IDLE);
        for rid in ids {
            self.routes.entry(rid).or_default().insert(origin.clone(), (via.to_string(), until));
        }
        *self.stats.entry("subs_heard").or_default() += 1;
        true
    }

    /// Eine PUB (JSON oder binaer, als Dict): nur fuer unsere Raeume; Ereignisse fuer unsere Spieler
    pub fn on_pub(&mut self, h: &Value, data: &[u8], me: &str) -> Vec<Event> {
        let mut ev = Vec::new();
        let origin = h["o"].as_str().unwrap_or("").to_string();
        let room = h["r"].as_str().unwrap_or("").to_string();
        let kind = h["k"].as_str().unwrap_or("");
        let member = if kind == "here" { "-".to_string() } else { h["m"].as_str().unwrap_or("").to_string() };
        if origin == me || !valid_name(&room, MAX_ROOM) || (kind != "here" && !valid_name(&member, MAX_MEMBER)) {
            return ev;
        }
        if !KINDS.contains(&kind) || data.len() > if kind == "here" { MAX_HERE } else { MAX_PAYLOAD } {
            return ev;
        }
        if self.locals_in(&room).is_empty() || !self.fresh(h) || !verify(h, data) {
            return ev;
        }
        self.mark(format!("{origin}:{}", h["q"]));
        *self.stats.entry("pubs_taken").or_default() += 1;
        let cred = h.get("c").filter(|c| c.is_object()).cloned();
        let now = Instant::now();
        let known = self.remote.entry(room.clone()).or_default().entry(origin.clone()).or_default();
        match kind {
            "join" => {
                if !known.members.contains_key(&member) {
                    ev.push(Event { kind: "join", room: room.clone(), member: member.clone(), node: origin.clone(), cred: cred.clone(), data: vec![] });
                }
                known.members.insert(member, cred);
                known.until = Some(known.until.unwrap_or(now).max(now + IDLE));
            }
            "leave" => {
                if known.members.remove(&member).is_some() {
                    ev.push(Event { kind: "leave", room, member, node: origin, cred: None, data: vec![] });
                }
            }
            "here" => {
                let Ok(Value::Array(body)) = serde_json::from_slice::<Value>(data) else { return ev };
                let mut names: BTreeMap<String, Option<Value>> = BTreeMap::new();
                for item in body.iter().take(MAX_MEMBERS_PER_ROOM) {
                    let Some(m) = item.get(0).and_then(Value::as_str).filter(|m| valid_name(m, MAX_MEMBER)) else { continue };
                    let c = item.get(1).filter(|c| c.is_object()).cloned().or_else(|| known.members.get(m).cloned().flatten());
                    names.insert(m.to_string(), c);
                }
                for m in known.members.keys().filter(|m| !names.contains_key(*m)) {
                    ev.push(Event { kind: "leave", room: room.clone(), member: m.clone(), node: origin.clone(), cred: None, data: vec![] });
                }
                for (m, c) in &names {
                    if !known.members.contains_key(m) {
                        ev.push(Event { kind: "join", room: room.clone(), member: m.clone(), node: origin.clone(), cred: c.clone(), data: vec![] });
                    }
                }
                known.members = names;
                known.until = Some(now + IDLE.max(HERE_EVERY * 3));
            }
            _ => {
                if known.members.contains_key(&member) {
                    let c = known.members[&member].clone();
                    ev.push(Event { kind: "message", room, member, node: origin, cred: c, data: data.to_vec() });
                }
            }
        }
        ev
    }

    /// Eine eigene PUB bauen und zu jedem Knoten im Raum schicken (je Weg eine Kopie mit "d")
    fn publish(&mut self, key: &SigningKey, me: &str, room: &str, member: &str, kind: &str, data: &[u8], cred: Option<&Value>) -> Out {
        let mut h = json!({"t": "pub", "r": room, "o": me, "q": self.next_seq(), "m": member, "k": kind, "hops": 0});
        if let Some(c) = cred {
            h["c"] = c.clone();
        }
        let mut msg = canon(&signed_part(&h)).into_bytes();
        msg.extend_from_slice(data);
        h["sig"] = json!(hex::encode(key.sign(&msg).to_bytes()));
        self.mark(format!("{me}:{}", h["q"]));
        let now = Instant::now();
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (origin, (via, until)) in self.routes.get(&room_id(room)).into_iter().flatten() {
            if origin != me && *until > now {
                groups.entry(via.clone()).or_default().push(origin[..SHORT.min(origin.len())].to_string());
            }
        }
        *self.stats.entry("pubs_sent").or_default() += 1;
        groups.into_iter().map(|(via, d)| {
            let mut c = h.clone();
            c["d"] = json!(d);
            (Some(via), c, data.to_vec())
        }).collect()
    }

    fn own_sub(&mut self, key: &SigningKey, me: &str, force: bool) -> Out {
        let mut ids: Vec<String> = self.own_rooms().iter().map(|r| room_id(r)).collect();
        ids.sort();
        if !force && ids == self.sub_sent {
            return vec![];
        }
        self.sub_sent = ids.clone();
        let mut h = json!({"t": "sub", "o": me, "q": self.next_seq(), "rooms": ids, "hops": 0});
        h["sig"] = json!(hex::encode(key.sign(canon(&signed_part(&h)).as_bytes()).to_bytes()));
        self.mark(format!("{me}:{}", h["q"]));
        vec![(None, h, vec![])]
    }

    /// Ein Spieler dieser Box betritt einen Raum: Ereignisse fuer unsere anderen Spieler dort, und was hinaus muss
    pub fn join(&mut self, key: &SigningKey, me: &str, room: &str, member: &str, cred: Option<Value>) -> Result<(Vec<Event>, Out, Vec<Value>), String> {
        if !valid_name(room, MAX_ROOM) || !valid_name(member, MAX_MEMBER) {
            return Err("a room is 1 to 128 and a member 1 to 64 printable characters".into());
        }
        if cred.as_ref().is_some_and(|c| !c.is_object() || canon(c).len() > 400) {
            return Err("a credential is a small object".into());
        }
        let k = (room.to_string(), member.to_string());
        let mut ev = vec![];
        let mut out = vec![];
        if !self.local.contains_key(&k) {
            if self.own_rooms().len() >= MAX_ROOMS && !self.own_rooms().contains(&room.to_string()) {
                return Err("this node is in as many rooms as it may be".into());
            }
            if self.locals_in(room).len() >= MAX_MEMBERS_PER_ROOM {
                return Err("this room is full on this node".into());
            }
            if !self.locals_in(room).is_empty() {
                ev.push(Event { kind: "join", room: room.into(), member: member.into(), node: me.into(), cred: cred.clone(), data: vec![] });
            }
            self.local.insert(k, Local { cred: cred.clone(), bucket: Bucket { tokens: BURST, at: Instant::now() }, touched: Instant::now() });
            out.extend(self.own_sub(key, me, false));
            out.extend(self.publish(key, me, room, member, "join", b"", cred.as_ref()));
            out.extend(self.here(key, me, Some(room), true));
        }
        Ok((ev, out, self.members(room, me)))
    }

    pub fn leave(&mut self, key: &SigningKey, me: &str, room: &str, member: &str) -> (Vec<Event>, Out) {
        let mut out = vec![];
        let mut ev = vec![];
        if self.local.remove(&(room.to_string(), member.to_string())).is_some() {
            if !self.locals_in(room).is_empty() {
                ev.push(Event { kind: "leave", room: room.into(), member: member.into(), node: me.into(), cred: None, data: vec![] });
            }
            out.extend(self.publish(key, me, room, member, "leave", b"", None));
            out.extend(self.own_sub(key, me, false));
        }
        (ev, out)
    }

    pub fn send(&mut self, key: &SigningKey, me: &str, room: &str, member: &str, data: &[u8]) -> Result<(Vec<Event>, Out), String> {
        if data.len() > MAX_PAYLOAD {
            return Err(format!("a message is at most {MAX_PAYLOAD} bytes"));
        }
        let cred = {
            let Some(l) = self.local.get_mut(&(room.to_string(), member.to_string())) else { return Err("join the room first".into()) };
            if !l.bucket.take() {
                return Err("too fast: about 5 messages a second".into());
            }
            l.touched = Instant::now();
            l.cred.clone()
        };
        let ev: Vec<Event> = if self.locals_in(room).iter().any(|m| m != member) {
            vec![Event { kind: "message", room: room.into(), member: member.into(), node: me.into(), cred: cred.clone(), data: data.to_vec() }]
        } else {
            vec![]
        };
        Ok((ev, self.publish(key, me, room, member, "msg", data, None)))
    }

    pub fn touch(&mut self, room: &str, member: &str) -> bool {
        match self.local.get_mut(&(room.to_string(), member.to_string())) {
            Some(l) => {
                l.touched = Instant::now();
                true
            }
            None => false,
        }
    }

    /// HERE in unsere Raeume (alle HERE_EVERY; mit Nachweisen jeden HERE_FULL-ten Schlag oder sofort)
    fn here(&mut self, key: &SigningKey, me: &str, only: Option<&str>, full_now: bool) -> Out {
        let mut rooms: BTreeMap<String, Vec<(String, Option<Value>)>> = BTreeMap::new();
        for ((r, m), l) in &self.local {
            if only.is_none_or(|o| o == r) {
                rooms.entry(r.clone()).or_default().push((m.clone(), l.cred.clone()));
            }
        }
        let full = full_now || self.here_beat % HERE_FULL == 0;
        let mut out = vec![];
        for (room, members) in rooms {
            let body: Vec<Value> = members.iter().map(|(m, c)| if full { json!([m, c]) } else { json!([m]) }).collect();
            let data = Value::Array(body).to_string().into_bytes();
            if data.len() <= MAX_HERE {
                out.extend(self.publish(key, me, &room, "-", "here", &data, None));
            }
        }
        out
    }

    /// Der Takt (jede HERE_EVERY): verwaiste eigene Mitglieder gehen, stumme fremde Knoten verschwinden, HERE und
    /// (alle SUB_EVERY oder bei Aenderung) SUB hinaus
    pub fn tick(&mut self, key: &SigningKey, me: &str, sub_due: bool) -> (Vec<Event>, Out) {
        let mut ev = vec![];
        let mut out = vec![];
        let now = Instant::now();
        let gone: Vec<(String, String)> = self.local.iter().filter(|(_, l)| now.duration_since(l.touched) > LOCAL_GRACE).map(|(k, _)| k.clone()).collect();
        for (r, m) in gone {
            let (e, o) = self.leave(key, me, &r, &m);
            ev.extend(e);
            out.extend(o);
        }
        for (room, nodes) in self.remote.iter_mut() {
            for (origin, r) in nodes.iter_mut() {
                if r.until.is_some_and(|u| u < now) {
                    for m in std::mem::take(&mut r.members).into_keys() {
                        ev.push(Event { kind: "leave", room: room.clone(), member: m, node: origin.clone(), cred: None, data: vec![] });
                    }
                }
            }
            nodes.retain(|_, r| !r.members.is_empty());
        }
        self.remote.retain(|r, n| !n.is_empty() && self.local.keys().any(|(lr, _)| lr == r));
        for m in self.routes.values_mut() {
            m.retain(|_, (_, u)| *u > now);
        }
        self.routes.retain(|_, m| !m.is_empty());
        self.here_beat += 1;
        out.extend(self.here(key, me, None, false));
        out.extend(self.own_sub(key, me, sub_due));
        (ev, out)
    }

    /// Wer in einem Raum ist: unsere eigenen und die, die andere Knoten nennen
    pub fn members(&self, room: &str, me: &str) -> Vec<Value> {
        let mut out: Vec<Value> = self.local.iter().filter(|((r, _), _)| r == room).map(|((_, m), l)| json!({"member": m, "node": me, "cred": l.cred})).collect();
        for (origin, r) in self.remote.get(room).into_iter().flatten() {
            for (m, c) in &r.members {
                out.push(json!({"member": m, "node": origin, "cred": c}));
            }
        }
        out
    }

    pub fn status(&self) -> Value {
        json!({"rooms": self.own_rooms().len(), "members": self.local.len(), "routes": self.routes.len(), "stats": self.stats})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_like_python() {
        // json.dumps({"b": 1, "a": "ä\n\"x"}, separators=(",", ":"), sort_keys=True)
        assert_eq!(canon(&json!({"b": 1, "a": "\u{e4}\n\"x"})), "{\"a\":\"\\u00e4\\n\\\"x\",\"b\":1}");
        assert_eq!(canon(&json!({"r": "\u{1f600}"})), "{\"r\":\"\\ud83d\\ude00\"}");
        assert_eq!(room_id("abc").len(), 16);
    }

    #[test]
    fn sign_verify_and_binary_form() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let me = hex::encode(key.verifying_key().to_bytes());
        let mut r = Rooms::default();
        // somebody (other) is in room "g/x" and reachable via link "L"
        let other = SigningKey::from_bytes(&[9; 32]);
        let oid = hex::encode(other.verifying_key().to_bytes());
        let mut sub = json!({"t": "sub", "o": oid, "q": (now_s() * 1e6) as u64, "rooms": [room_id("g/x")], "hops": 0});
        sub["sig"] = json!(hex::encode(other.sign(canon(&signed_part(&sub)).as_bytes()).to_bytes()));
        assert!(r.on_sub("L", &sub, &me));
        assert!(!r.on_sub("L", &sub, &me), "a duplicate is refused");
        let (_, out, _) = r.join(&key, &me, "g/x", "guest-0000abcd", None).unwrap();
        // our SUB to all, our join PUB to L with d = other's short id, a HERE to L
        assert!(out.iter().any(|(to, h, _)| to.is_none() && h["t"] == "sub"));
        let join = out.iter().find(|(to, h, _)| to.as_deref() == Some("L") && h["k"] == "join").unwrap();
        assert_eq!(join.1["d"], json!([&oid[..8]]));
        assert!(verify(&join.1, &join.2));
        // a message from the other node: first its join (as a binary frame), then a msg
        let mk = |k: u8, member: &str, data: &[u8], q: u64| {
            let h = json!({"t": "pub", "r": "g/x", "o": oid, "q": q, "m": member, "k": KINDS[k as usize], "hops": 0});
            let mut msg = canon(&signed_part(&h)).into_bytes();
            msg.extend_from_slice(data);
            let sig = other.sign(&msg).to_bytes();
            let mut b = vec![0xff, 0xff, 1];
            b.extend_from_slice(&other.verifying_key().to_bytes());
            b.extend_from_slice(&sig);
            b.extend_from_slice(&q.to_be_bytes());
            b.extend_from_slice(&[0, k, 3]);
            b.extend_from_slice(b"g/x");
            b.push(member.len() as u8);
            b.extend_from_slice(member.as_bytes());
            b.extend_from_slice(&[0, 0, 0xff, 0xff]);
            b.extend_from_slice(data);
            b
        };
        let q0 = (now_s() * 1e6) as u64 + 5;
        let (h, d) = unpack_pub(&mk(1, "guest-11112222", b"", q0)).unwrap();
        let ev = r.on_pub(&h, &d, &me);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, "join");
        let (h, d) = unpack_pub(&mk(0, "guest-11112222", b"hello", q0 + 1)).unwrap();
        let ev = r.on_pub(&h, &d, &me);
        assert_eq!((ev[0].kind, ev[0].data.as_slice()), ("message", &b"hello"[..]));
        // tampered payload: refused
        let mut bad = mk(0, "guest-11112222", b"hello", q0 + 2);
        *bad.last_mut().unwrap() ^= 1;
        let (h, d) = unpack_pub(&bad).unwrap();
        assert!(r.on_pub(&h, &d, &me).is_empty());
        assert_eq!(r.members("g/x", &me).len(), 2);
    }
}

impl Rooms {
    /// Unsere aktuelle SUB fuer eine neue Verbindung (None: wir sind in keinem Raum)
    pub fn current_sub(&mut self, key: &SigningKey, me: &str) -> Option<Value> {
        if self.local.is_empty() {
            return None;
        }
        self.own_sub(key, me, true).pop().map(|x| x.1)
    }
}
