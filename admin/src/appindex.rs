//! App-Index: die Apps der Arcades, aus der eigenen Pepecoin-Testnet-Chain gelesen (dogecoin-arcade games.py und
//! inscriptions.py: eine App ist eine Inschrift, die eine Seite ist - text/html - und in ihrem JSON-Feld ein
//! Objekt "app" oder "game" mit einem Namen traegt). Eigener Code; dieselben Regeln:
//!
//! - Inschriften kommen in Stuecken (AnyData, meist Class B): "INSC" 01 01 id(8) countdown(2) clen(2) body. Das
//!   Stueck mit dem hoechsten countdown traegt das Manifest (Groesse, sha256, Inhaltstyp, JSON). Je Absender und id
//!   zaehlt jedes countdown einmal. Fertig ist eine Inschrift, wenn alle Stuecke da sind; ihr Inhalt muss dann den
//!   sha256 des Manifests haben, sonst ist sie keine. Name: die txid des Manifest-Stuecks; Ersteller: der Absender.
//! - Eine Karte je Ersteller und Name (ohne Gross/Klein): die neueste ersetzt die aeltere.
//! - Alles auf der Chain sind Worte von irgendwem: Felder werden auf ihre Laenge gekuerzt, Kaputtes faellt weg.
//!
//! Ab ACTIVATION, Block fuer Block, mit Reorg-Erkennung; Stand in <state>/appindex.json. Was davon Nutzer sehen,
//! entscheidet der Betreiber (apps.rs: jede App zuerst AUS).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::utxo::{batch, internal, read_block, shown, Txid};
use crate::{log, rpc, App, ACTIVATION};

const BATCH: u32 = 200;
/// Stuecke unfertiger Inschriften, die gehalten werden (Bytes); darueber faellt die aelteste unfertige weg
const PENDING_MOST: usize = 64 * 1024 * 1024;


/// Eine fertige Inschrift: Inhaltstyp, Groesse, sha256, Ersteller, Hoehe, Stuecke (txids, Manifest-Stueck zuerst)
#[derive(Clone)]
pub struct Insc {
    pub ctype: String,
    pub total: u32,
    pub sha: [u8; 32],
    pub creator: String,
    pub h: u32,
    pub pieces: Vec<String>,
    /// laufende Nummer (Reihenfolge, in der Inschriften fertig wurden, wie die Arcades)
    pub number: u64,
    /// wem sie gehoert (Uebertragungen KIND_TRANSFER; Tausch/Buendel noch nicht)
    pub owner: String,
    pub json: String,
    pub pos: u32,
}

pub fn inscription(txid: &str) -> Option<Insc> {
    idx().lock().unwrap().insc.get(txid).cloned()
}

/// Eine fertige App-Karte
#[derive(Clone)]
pub struct Card {
    pub txid: String,
    pub creator: String,
    pub h: u32,
    pub pos: u32,
    pub game: Value,
}

#[derive(Default)]
struct Pending {
    /// countdown -> (Inhalt des Stuecks, txid, Hoehe, Position)
    pieces: BTreeMap<u16, (Vec<u8>, String, u32, u32)>,
    bytes: usize,
    first_h: u32,
}

#[derive(Default)]
pub struct Idx {
    height: u32,
    hash: Txid,
    /// (Absender, id) -> Stuecke
    pending: HashMap<(String, [u8; 8]), Pending>,
    pending_bytes: usize,
    /// alle App-Karten, auch ersetzte (die neueste je Ersteller+Name zeigt cards())
    cards: Vec<Card>,
    inscriptions: u64,
    /// jede fertige Inschrift: txid -> wo ihre Stuecke liegen (fuer /content, content.rs)
    insc: HashMap<String, Insc>,
}

fn idx() -> &'static Mutex<Idx> {
    static I: OnceLock<Mutex<Idx>> = OnceLock::new();
    I.get_or_init(Default::default)
}

// ------------------------------------------------------------------ Karten lesen (games.py parse)

fn text(v: &Value, limit: usize) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return String::new(),
    };
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(limit).collect()
}

fn txid_field(v: &Value) -> String {
    let s = v.as_str().unwrap_or("").to_ascii_lowercase();
    if s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()) { s } else { String::new() }
}

/// Die Karte, die ein JSON-Feld beschreibt ("app" oder "game" mit Namen), gesaeubert; None wenn keine
pub fn parse_card(json_text: &str) -> Option<Value> {
    let said: Value = serde_json::from_str(json_text).ok()?;
    let g = ["game", "app"].iter().find_map(|k| said.get(*k).filter(|x| x.is_object()))?;
    let name = text(&g["name"], 60);
    if name.is_empty() {
        return None;
    }
    let flag = |k: &str| g.get(k) == Some(&Value::Bool(true));
    Some(json!({
        "name": name, "description": text(&g["description"], 280), "version": text(&g["version"], 20),
        "players": text(&g["players"], 20), "genre": text(&g["genre"], 24),
        "multiplayer": flag("multiplayer"), "wallet": flag("wallet"), "media": flag("media"), "pictures": flag("pictures"),
        "notify": flag("notify"), "contacts": flag("contacts"), "groups": flag("groups"),
        "cover": txid_field(&g["cover"]), "play": txid_field(&g["play"]),
    }))
}

/// Schluessel einer App fuer die Schalter: Ersteller und Name als Link-Teil (games.py app_key / slug)
pub fn app_key(creator: &str, name: &str) -> String {
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    format!("app:{creator}:{}", if slug.is_empty() { "app" } else { &slug[..slug.len().min(80)] })
}

/// Die aktuellen Karten: je Ersteller und Name die neueste
pub fn cards() -> Vec<Card> {
    let g = idx().lock().unwrap();
    let mut all = g.cards.clone();
    all.sort_by(|a, b| (b.h, b.pos).cmp(&(a.h, a.pos)));
    let mut seen = std::collections::HashSet::new();
    all.into_iter().filter(|c| seen.insert((c.creator.clone(), c.game["name"].as_str().unwrap_or("").to_lowercase()))).collect()
}

pub fn progress() -> (u32, u64, usize) {
    let g = idx().lock().unwrap();
    (g.height, g.inscriptions, g.pending.len())
}

// ------------------------------------------------------------------ Stuecke zusammensetzen

/// Ein Stueck: ("INSC" 01 01) id(8) countdown(2) clen(2) body
pub fn chunk(data: &[u8]) -> Option<([u8; 8], u16, &[u8])> {
    let p = data.strip_prefix(b"INSC\x01\x01")?;
    let id: [u8; 8] = p.get(..8)?.try_into().ok()?;
    let countdown = u16::from_be_bytes(p.get(8..10)?.try_into().ok()?);
    let clen = u16::from_be_bytes(p.get(10..12)?.try_into().ok()?) as usize;
    let body = p.get(12..12 + clen)?;
    Some((id, countdown, body))
}

/// Manifest: total(4) sha256(32) ctype_len(1) ctype json_len(2) json, danach Inhalt
pub fn manifest(raw: &[u8]) -> Option<(u32, [u8; 32], String, String, &[u8])> {
    let total = u32::from_be_bytes(raw.get(..4)?.try_into().ok()?);
    let sha: [u8; 32] = raw.get(4..36)?.try_into().ok()?;
    let cl = *raw.get(36)? as usize;
    let ctype = String::from_utf8_lossy(raw.get(37..37 + cl)?).into_owned();
    let at = 37 + cl;
    let jl = u16::from_be_bytes(raw.get(at..at + 2)?.try_into().ok()?) as usize;
    let js = String::from_utf8_lossy(raw.get(at + 2..at + 2 + jl)?).into_owned();
    Some((total, sha, ctype, js, &raw[at + 2 + jl..]))
}

impl Idx {
    fn piece(&mut self, from: &str, id: [u8; 8], countdown: u16, body: &[u8], txid: String, h: u32, pos: u32) {
        let key = (from.to_string(), id);
        let p = self.pending.entry(key.clone()).or_insert_with(|| Pending { first_h: h, ..Default::default() });
        if p.pieces.contains_key(&countdown) {
            return; // dasselbe Stueck noch einmal: zaehlt nicht
        }
        p.bytes += body.len();
        self.pending_bytes += body.len();
        p.pieces.insert(countdown, (body.to_vec(), txid, h, pos));
        // fertig, wenn 0..=hoechster countdown alle da sind
        let top = *p.pieces.keys().next_back().unwrap();
        if p.pieces.len() != top as usize + 1 {
            self.trim();
            return;
        }
        // Wie die Arcades (Assembly.join): alle Stuecke vom hoechsten countdown abwaerts aneinander, daraus das
        // Manifest (es kann ueber mehrere Stuecke reichen) und der Inhalt, gekuerzt auf seine Groesse; stimmt der
        // sha256 nicht, bleiben die Stuecke liegen (ein spaeteres Stueck kann den Satz noch richtig machen).
        let p = self.pending.get(&key).unwrap();
        let stream: Vec<u8> = (0..=top).rev().flat_map(|c| p.pieces[&c].0.iter().copied()).collect();
        let first_txid = p.pieces[&top].1.clone();
        let Some((total, sha, ctype, js, rest)) = manifest(&stream) else { return self.trim() };
        let total = total as usize;
        if rest.len() < total || <[u8; 32]>::from(Sha256::digest(&rest[..total])) != sha {
            return self.trim();
        }
        let p = self.pending.remove(&key).unwrap();
        self.pending_bytes -= p.bytes;
        self.inscriptions += 1;
        let number = self.inscriptions - 1;
        self.insc.insert(first_txid.clone(), Insc { ctype: ctype.clone(), total: total as u32, sha, creator: from.to_string(), h,
            number, owner: from.to_string(), json: js.clone(), pos,
            pieces: (0..=top).rev().map(|c| p.pieces[&c].1.clone()).collect() });
        if !ctype.to_ascii_lowercase().starts_with("text/html") {
            return;
        }
        if let Some(game) = parse_card(&js) {
            self.cards.push(Card { txid: first_txid, creator: from.to_string(), h, pos, game });
        }
    }

    /// Zu viele unfertige Stuecke: die aelteste unfertige Inschrift geht
    fn trim(&mut self) {
        while self.pending_bytes > PENDING_MOST {
            let Some(old) = self.pending.iter().min_by_key(|(_, p)| p.first_h).map(|(k, _)| k.clone()) else { break };
            if let Some(p) = self.pending.remove(&old) {
                self.pending_bytes -= p.bytes;
            }
        }
    }

    fn rewind(&mut self, cfg: &crate::Cfg, to: u32) -> Result<(), String> {
        self.cards.retain(|c| c.h <= to);
        self.insc.retain(|_, i| i.h <= to);
        for p in self.pending.values_mut() {
            let gone: Vec<u16> = p.pieces.iter().filter(|(_, x)| x.2 > to).map(|(k, _)| *k).collect();
            for k in gone {
                if let Some(x) = p.pieces.remove(&k) {
                    p.bytes -= x.0.len();
                    self.pending_bytes -= x.0.len();
                }
            }
        }
        self.pending.retain(|_, p| !p.pieces.is_empty());
        self.height = to;
        self.hash = internal(rpc(cfg, "getblockhash", json!([to]))?.as_str().unwrap_or(""))?;
        Ok(())
    }

    fn save(&self, path: &PathBuf) {
        let cards: Vec<Value> = self.cards.iter().map(|c| json!({"txid": c.txid, "creator": c.creator, "h": c.h, "pos": c.pos, "game": c.game})).collect();
        let pending: Vec<Value> = self.pending.iter().map(|((from, id), p)| json!({"from": from, "id": hex::encode(id), "first_h": p.first_h,
            "pieces": p.pieces.iter().map(|(c, (b, t, h, pos))| json!([c, hex::encode(b), t, h, pos])).collect::<Vec<_>>()})).collect();
        let insc: serde_json::Map<String, Value> = self.insc.iter().map(|(t, i)| (t.clone(), json!([i.ctype, i.total, hex::encode(i.sha), i.creator, i.h, i.pieces, i.number, i.owner, i.json, i.pos])))
            .collect();
        let v = json!({"version": 3, "height": self.height, "hash": shown(&self.hash), "inscriptions": self.inscriptions, "cards": cards, "pending": pending, "insc": insc});
        let tmp = path.with_extension("new");
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    fn load(path: &PathBuf) -> Idx {
        let mut i = Idx { height: ACTIVATION as u32 - 1, ..Default::default() };
        let Some(v) = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { return i };
        // aeltere Fassung ohne die Stuecke jeder Inschrift: von vorn lesen
        if v["version"] != 3 {
            return i;
        }
        for (t, x) in v["insc"].as_object().into_iter().flatten() {
            let Some(sha) = x[2].as_str().and_then(|s| hex::decode(s).ok()).and_then(|b| <[u8; 32]>::try_from(b).ok()) else { continue };
            i.insc.insert(t.clone(), Insc { ctype: x[0].as_str().unwrap_or("").into(), total: x[1].as_u64().unwrap_or(0) as u32, sha,
                creator: x[3].as_str().unwrap_or("").into(), h: x[4].as_u64().unwrap_or(0) as u32,
                pieces: x[5].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect(),
                number: x[6].as_u64().unwrap_or(0), owner: x[7].as_str().unwrap_or("").into(), json: x[8].as_str().unwrap_or("").into(),
                pos: x[9].as_u64().unwrap_or(0) as u32 });
        }
        i.height = v["height"].as_u64().unwrap_or(i.height as u64) as u32;
        i.hash = internal(v["hash"].as_str().unwrap_or("")).unwrap_or([0; 32]);
        i.inscriptions = v["inscriptions"].as_u64().unwrap_or(0);
        for c in v["cards"].as_array().into_iter().flatten() {
            i.cards.push(Card { txid: c["txid"].as_str().unwrap_or("").into(), creator: c["creator"].as_str().unwrap_or("").into(),
                h: c["h"].as_u64().unwrap_or(0) as u32, pos: c["pos"].as_u64().unwrap_or(0) as u32, game: c["game"].clone() });
        }
        for p in v["pending"].as_array().into_iter().flatten() {
            let Some(id) = p["id"].as_str().and_then(|s| hex::decode(s).ok()).and_then(|b| <[u8; 8]>::try_from(b).ok()) else { continue };
            let mut pe = Pending { first_h: p["first_h"].as_u64().unwrap_or(0) as u32, ..Default::default() };
            for x in p["pieces"].as_array().into_iter().flatten() {
                let body = hex::decode(x[1].as_str().unwrap_or("")).unwrap_or_default();
                pe.bytes += body.len();
                pe.pieces.insert(x[0].as_u64().unwrap_or(0) as u16, (body, x[2].as_str().unwrap_or("").into(), x[3].as_u64().unwrap_or(0) as u32, x[4].as_u64().unwrap_or(0) as u32));
            }
            i.pending_bytes += pe.bytes;
            i.pending.insert((p["from"].as_str().unwrap_or("").into(), id), pe);
        }
        i
    }
}

// ------------------------------------------------------------------ Ablauf

fn step(app: &App, path: &PathBuf, marker: &[u8]) -> Result<bool, String> {
    let cfg = &app.cfg;
    let tip = rpc(cfg, "getblockcount", json!([]))?.as_u64().ok_or("getblockcount")? as u32;
    let height = {
        let mut g = idx().lock().unwrap();
        if g.hash == [0; 32] {
            let h = g.height;
            g.hash = internal(rpc(cfg, "getblockhash", json!([h]))?.as_str().unwrap_or(""))?;
        }
        if tip < g.height {
            let back = tip.saturating_sub(10).max(ACTIVATION as u32);
            g.rewind(cfg, back)?;
        }
        g.height
    };
    if tip <= height {
        return Ok(false);
    }
    let to = tip.min(height + BATCH);
    let hashes = batch(cfg, "getblockhash", (height + 1..=to).map(|h| json!([h])))?;
    let blocks = batch(cfg, "getblock", hashes.iter().map(|h| json!([h, false])))?;
    let mut g = idx().lock().unwrap();
    if g.height != height {
        return Ok(true);
    }
    let before = g.cards.len();
    for (i, b) in blocks.iter().enumerate() {
        let raw = hex::decode(b.as_str().unwrap_or("")).map_err(|e| e.to_string())?;
        let (hash, prev, txs) = read_block(&raw)?;
        let h = height + 1 + i as u32;
        if prev != g.hash {
            log(&format!("App-Index: Reorg bei Block {h}, lese die letzten Bloecke neu"));
            g.rewind(cfg, h.saturating_sub(20).max(ACTIVATION as u32))?;
            return Ok(true);
        }
        for (pos, tx) in txs.iter().enumerate() {
            g.apply(tx, marker, h, pos as u32);
        }
        g.height = h;
        g.hash = hash;
    }
    if g.cards.len() != before || g.height == tip || g.height % 10_000 < BATCH {
        g.save(path);
    }
    Ok(g.height < tip)
}

pub fn run(app: Arc<App>) {
    let path = app.cfg.state.join("appindex.json");
    let marker = crate::msgs::marker();
    *idx().lock().unwrap() = Idx::load(&path);
    let mut logged = false;
    loop {
        match step(&app, &path, &marker) {
            Ok(true) => continue,
            Ok(false) => {
                if !logged {
                    let (h, n, _) = progress();
                    log(&format!("App-Index: bis Block {h} gelesen, {n} Inschriften, {} Apps", cards().len()));
                    logged = true;
                }
                std::thread::sleep(Duration::from_secs(10));
            }
            Err(e) => {
                log(&format!("App-Index: {e}"));
                std::thread::sleep(Duration::from_secs(20));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_as_the_arcades_read_them() {
        let c = parse_card(r#"{"app": {"name": "  Video   Call ", "wallet": true, "cover": "AB", "play": "00000000000000000000000000000000000000000000000000000000000000aa"}}"#).unwrap();
        assert_eq!(c["name"], "Video Call");
        assert_eq!(c["wallet"], true);
        assert_eq!(c["cover"], "");
        assert_eq!(c["play"].as_str().unwrap().len(), 64);
        assert!(parse_card(r#"{"game": {"name": ""}}"#).is_none());
        assert!(parse_card(r#"{"game": "x"}"#).is_none());
        assert!(parse_card("not json").is_none());
        assert_eq!(app_key("nAbc", "Ghost Fleet!"), "app:nAbc:ghost-fleet");
    }

    #[test]
    fn pieces_assemble_and_are_checked() {
        let content = b"<html>hello app</html>".to_vec();
        let js = r#"{"app":{"name":"Hello"}}"#;
        let ct = b"text/html";
        let mut man = (content.len() as u32).to_be_bytes().to_vec();
        man.extend_from_slice(&Sha256::digest(&content));
        man.push(ct.len() as u8);
        man.extend_from_slice(ct);
        man.extend_from_slice(&(js.len() as u16).to_be_bytes());
        man.extend_from_slice(js.as_bytes());
        // two pieces: the manifest with the first 10 bytes (countdown 1), the rest (countdown 0), arriving in reverse
        let mut first = man.clone();
        first.extend_from_slice(&content[..10]);
        let mut ix = Idx::default();
        ix.piece("nCreator", [7; 8], 0, &content[10..], "t0".into(), 5, 1);
        assert!(ix.cards.is_empty());
        ix.piece("nCreator", [7; 8], 1, &first, "t1".into(), 6, 3);
        assert_eq!(ix.cards.len(), 1);
        assert_eq!(ix.cards[0].txid, "t1");
        assert_eq!(ix.cards[0].game["name"], "Hello");
        // a set whose content does not match its sha256 is no inscription
        let mut ix = Idx::default();
        let mut bad = first.clone();
        *bad.last_mut().unwrap() ^= 1;
        ix.piece("n", [1; 8], 0, &content[10..], "a".into(), 5, 1);
        ix.piece("n", [1; 8], 1, &bad, "b".into(), 5, 2);
        assert!(ix.cards.is_empty() && ix.inscriptions == 0);
        // the chunk header
        let mut d = b"INSC\x01\x01".to_vec();
        d.extend_from_slice(&[9; 8]);
        d.extend_from_slice(&3u16.to_be_bytes());
        d.extend_from_slice(&2u16.to_be_bytes());
        d.extend_from_slice(b"hi\0\0\0");
        assert_eq!(chunk(&d), Some(([9; 8], 3, &b"hi"[..])));
    }
}

/// Die Adresse einer Ausgabe (P2PKH, P2SH; Pepecoin-Testnet), sonst None
fn out_addr(spk: &[u8]) -> Option<String> {
    if spk.len() == 25 && spk[..3] == [0x76, 0xa9, 20] && spk[23..] == [0x88, 0xac] {
        return Some(crate::b58check(crate::PKH, &spk[3..23]));
    }
    if spk.len() == 23 && spk[..2] == [0xa9, 20] && spk[22] == 0x87 {
        return Some(crate::b58check(crate::P2SH, &spk[2..22]));
    }
    None
}

/// Die Referenzadresse wie die Arcades (tx.py determine_reference): die Ausgaben mit Adresse ausser dem Marker;
/// eine einzige ist es; sonst die erste an den Absender als Wechselgeld uebersprungen und die letzte der anderen
fn reference(tx: &crate::utxo::Tx, sender: &str, marker: &[u8]) -> Option<String> {
    let c: Vec<String> = tx.outs.iter().filter(|(_, s)| *s != marker).filter_map(|(_, s)| out_addr(s)).collect();
    match c.len() {
        0 => None,
        1 => c.into_iter().next(),
        _ => {
            let (mut r, mut change) = (None, false);
            for a in c {
                if a == sender && !change {
                    change = true;
                    continue;
                }
                r = Some(a);
            }
            r
        }
    }
}

impl Idx {
    /// Eine Transaktion eines Blocks: Uebertragung oder Stueck einer Inschrift
    fn apply(&mut self, tx: &crate::utxo::Tx, marker: &[u8], h: u32, pos: u32) {
        let Some((from, data)) = crate::msgs::anydata(tx, marker) else { return };
        if let Some(target) = data.strip_prefix(b"INSC\x01\x02").and_then(|t| t.get(..32)) {
            // Uebertragung: an die Referenzadresse, nur durch den Besitzer (state.py _inscription_transfer)
            let mut id = target.to_vec();
            id.reverse();
            if let Some(to) = reference(tx, &from, marker) {
                self.transfer(&hex::encode(id), &from, &to);
            }
        } else if let Some((id, countdown, body)) = chunk(&data) {
            self.piece(&from, id, countdown, body, shown(&tx.txid), h, pos);
        }
    }

    fn transfer(&mut self, txid: &str, from: &str, to: &str) {
        if let Some(i) = self.insc.get_mut(txid) {
            if i.owner == from && to != i.owner {
                i.owner = to.to_string();
            }
        }
    }
}

/// Alle Inschriften (fuer die Lese-Schnittstellen r.rs): (txid, Inschrift), neueste zuerst
pub fn all_newest_first() -> Vec<(String, Insc)> {
    let g = idx().lock().unwrap();
    let mut v: Vec<(String, Insc)> = g.insc.iter().map(|(t, i)| (t.clone(), i.clone())).collect();
    v.sort_by(|a, b| b.1.number.cmp(&a.1.number));
    v
}

pub fn by_number(n: u64) -> Option<(String, Insc)> {
    let g = idx().lock().unwrap();
    g.insc.iter().find(|(_, i)| i.number == n).map(|(t, i)| (t.clone(), i.clone()))
}

#[cfg(test)]
mod transfer_tests {
    use super::*;

    /// Eine Transaktion wie die, die das App-Fenster nach dem Ja signiert: Input von `pubkey` (scriptSig <sig> <pubkey>),
    /// die Ausgaben aus sendreq::outputs und Wechselgeld an den Absender
    fn raw_tx(pubkey: &[u8; 33], outs: &[(u64, String)]) -> Vec<u8> {
        let mut t = 1u32.to_le_bytes().to_vec();
        t.push(1);
        t.extend_from_slice(&[0x11; 32]);
        t.extend_from_slice(&0u32.to_le_bytes());
        let mut sig = vec![71u8];
        sig.extend_from_slice(&[0x30; 71]);
        sig.push(33);
        sig.extend_from_slice(pubkey);
        t.push(sig.len() as u8);
        t.extend_from_slice(&sig);
        t.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        t.push(outs.len() as u8);
        for (v, s) in outs {
            let s = hex::decode(s).unwrap();
            t.extend_from_slice(&v.to_le_bytes());
            t.push(s.len() as u8);
            t.extend_from_slice(&s);
        }
        t.extend_from_slice(&0u32.to_le_bytes());
        t
    }

    fn held_by(owner: &str) -> Insc {
        Insc { ctype: "text/html".into(), total: 1, sha: [0; 32], creator: owner.into(), h: 1, pieces: vec![], number: 7, owner: owner.into(), json: String::new(), pos: 0 }
    }

    #[test]
    fn built_transfer_moves_the_inscription_to_the_recipient() {
        let pk = [2u8; 33];
        let sender = crate::b58check(crate::PKH, &crate::hash160(&pk));
        let to = crate::b58check(crate::PKH, &[9u8; 20]);
        let piece = format!("{}{}", "ab".repeat(31), "01");
        let marker = crate::msgs::marker();
        let own = crate::p2pkh_script(&crate::hash160(&pk));

        // mit Wechselgeld an den Absender (der Normalfall von fund())
        let mut outs = crate::sendreq::outputs("inscription", &to, 0, &piece).unwrap();
        outs.push((5 * crate::COIN, own.clone()));
        let raw = raw_tx(&pk, &outs);
        let tx = crate::utxo::parse_tx(&raw).unwrap();
        assert_eq!(reference(&tx, &sender, &marker), Some(to.clone()));
        let mut ix = Idx::default();
        ix.insc.insert(piece.clone(), held_by(&sender));
        ix.apply(&tx, &marker, 10, 1);
        assert_eq!(ix.insc[&piece].owner, to);

        // ohne Wechselgeld
        let outs = crate::sendreq::outputs("inscription", &to, 0, &piece).unwrap();
        let raw = raw_tx(&pk, &outs);
        let tx = crate::utxo::parse_tx(&raw).unwrap();
        let mut ix = Idx::default();
        ix.insc.insert(piece.clone(), held_by(&sender));
        ix.apply(&tx, &marker, 10, 1);
        assert_eq!(ix.insc[&piece].owner, to);

        // wer nicht besitzt, uebertraegt nichts
        let mut ix = Idx::default();
        ix.insc.insert(piece.clone(), held_by("nSomebodyElse"));
        ix.apply(&tx, &marker, 10, 1);
        assert_eq!(ix.insc[&piece].owner, "nSomebodyElse");
    }
}
