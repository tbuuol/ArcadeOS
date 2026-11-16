//! Wallet-Zugriff fuer eingeschriebene Apps (Stufe E1 der App-Laufzeit): wer schaut eine Seite an, und darf die
//! Seite das wissen?
//!
//! * ZUSTIMMUNG je Konto und App (Karte): das App-Fenster fragt den angemeldeten Nutzer ("App X moechte deine Wallet
//!   sehen"), die Antwort liegt hier (<state>/access.json, {Konto: {Karte: {"wallet": Zeitpunkt}}}) und laesst sich
//!   zuruecknehmen. Gaeste haben keine Wallet.
//! * BETRACHTER-TICKET wie bei den Arcades (dogecoin-arcade app.py _viewer_of): die Seite laeuft in einer Sandbox
//!   ohne Ursprung und schickt kein Cookie mit; das Fenster holt (mit Cookie) ein Ticket, gebunden an Konto und Seite,
//!   und rahmt /content/<Seite>?v=<Ticket>. Die fetch-Aufrufe der Seite tragen diese Adresse als Referer
//!   (Referrer-Policy: unsafe-url auf /content; connect-src erlaubt nur diese Arcade, das Ticket geht nirgends sonst
//!   hin). Eine Seite darf v= auch selbst anhaengen. Ein Ticket gilt 12 Stunden und verlaengert sich beim Gebrauch;
//!   gespeichert wird nur sha256(Ticket) (<state>/viewer-tickets.json), damit ein Neustart offene Seiten nicht trennt.
//!
//! Das Ticket allein oeffnet nichts: /r/wallet (r.rs) prueft bei JEDER Frage die Zustimmung und ob die App noch
//! freigeschaltet ist. Deshalb bekommt jedes angemeldete Fenster sein Ticket schon beim Oeffnen - eine Zustimmung
//! mitten im Spiel wirkt sofort, ohne die Seite neu zu laden, und Zuruecknehmen ebenso.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::{now, sha256, App};

pub const TICKET_SECS: u64 = 12 * 3600;
const MAX_TICKETS: usize = 20_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    pub account: String, // Ed25519-Anmeldeschluessel (hex)
    pub address: String, // Pepecoin-Testnet-Adresse des Kontos
    pub card: String,    // Karte der App (txid)
    pub page: String,    // die gerahmte Seite (txid)
    pub exp: u64,
}

impl Ticket {
    fn to_json(&self) -> Value {
        json!({"account": self.account, "address": self.address, "card": self.card, "page": self.page, "exp": self.exp})
    }
    fn from_json(v: &Value) -> Option<Ticket> {
        Some(Ticket {
            account: v["account"].as_str()?.into(),
            address: v["address"].as_str()?.into(),
            card: v["card"].as_str()?.into(),
            page: v["page"].as_str()?.into(),
            exp: v["exp"].as_u64()?,
        })
    }
}

struct Store {
    path: PathBuf,
    tickets: HashMap<String, Ticket>, // sha256(Ticket) hex -> Ticket
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);
static CONSENT: Mutex<()> = Mutex::new(());

fn with_store<R>(app: &App, f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = STORE.lock().unwrap();
    let s = g.get_or_insert_with(|| {
        let path = app.cfg.state.join("viewer-tickets.json");
        let raw: HashMap<String, Value> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let t = now();
        let tickets = raw.iter().filter_map(|(k, v)| Ticket::from_json(v).map(|x| (k.clone(), x))).filter(|(_, x)| x.exp > t).collect();
        Store { path, tickets }
    });
    f(s)
}

fn save(s: &Store) {
    let m: serde_json::Map<String, Value> = s.tickets.iter().map(|(k, v)| (k.clone(), v.to_json())).collect();
    let tmp = s.path.with_extension("new");
    if std::fs::write(&tmp, Value::Object(m).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &s.path);
    }
}

fn hashed(tok: &str) -> String {
    hex::encode(sha256(tok.as_bytes()))
}

/// Ein neues Ticket fuer dieses Konto und diese Seite (Text fuer ?v=)
pub fn mint(app: &App, account: &str, address: &str, card: &str, page: &str) -> (String, u64) {
    let mut b = [0u8; 18];
    getrandom::getrandom(&mut b).ok();
    let tok = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, b);
    let exp = now() + TICKET_SECS;
    with_store(app, |s| {
        let t = now();
        s.tickets.retain(|_, x| x.exp > t);
        if s.tickets.len() >= MAX_TICKETS {
            // die aeltesten zuerst weg
            let mut v: Vec<(String, u64)> = s.tickets.iter().map(|(k, x)| (k.clone(), x.exp)).collect();
            v.sort_by_key(|x| x.1);
            for (k, _) in v.into_iter().take(MAX_TICKETS / 10) {
                s.tickets.remove(&k);
            }
        }
        s.tickets.insert(hashed(&tok), Ticket { account: account.into(), address: address.into(), card: card.into(), page: page.into(), exp });
        save(s);
    });
    (tok, exp)
}

/// Ticket und Seite einer Anfrage: das Ticket aus ?v= oder aus dem Referer; die Seite aus dem Referer, wenn er
/// eine /content/<txid>-Adresse ist (sonst leer)
pub fn from_request(query: &str, referer: &str) -> (String, String) {
    let v_of = |q: &str| q.split('&').find_map(|kv| kv.strip_prefix("v=")).unwrap_or("").to_string();
    let mut ticket = v_of(query);
    let mut page = String::new();
    if !referer.is_empty() {
        let rest = referer.split_once("://").map(|x| x.1).unwrap_or(referer);
        let path_q = rest.find('/').map(|i| &rest[i..]).unwrap_or("");
        let path_q = path_q.split('#').next().unwrap_or("");
        let (path, q) = path_q.split_once('?').unwrap_or((path_q, ""));
        if ticket.is_empty() {
            ticket = v_of(q);
        }
        if let Some(p) = path.strip_prefix("/content/") {
            // /content/<txid>, /content/<txid>/..., /content/<txid>i0
            page = p.split('/').next().unwrap_or("").split('i').next().unwrap_or("").to_ascii_lowercase();
        }
    }
    (ticket, page)
}

/// Die reine Pruefung: gibt es das Ticket, ist es nicht abgelaufen, und passt die Seite im Referer (wenn einer eine
/// nennt) zur Seite des Tickets? Ein Ticket gilt fuer eine Seite, nicht fuer jede.
pub fn check(tickets: &HashMap<String, Ticket>, ticket: &str, referer_page: &str, t: u64) -> Option<Ticket> {
    if ticket.is_empty() || ticket.len() > 64 {
        return None;
    }
    let held = tickets.get(&hashed(ticket))?;
    if held.exp <= t {
        return None;
    }
    if !referer_page.is_empty() && referer_page != held.page {
        return None;
    }
    Some(held.clone())
}

/// Wer schaut diese Seite an? Verlaengert das Ticket, wenn es zur Haelfte verbraucht ist.
pub fn viewer_of(app: &App, query: &str, referer: &str) -> Option<Ticket> {
    let (tok, page) = from_request(query, referer);
    with_store(app, |s| {
        let t = now();
        let held = check(&s.tickets, &tok, &page, t)?;
        if held.exp - t < TICKET_SECS / 2 {
            if let Some(x) = s.tickets.get_mut(&hashed(&tok)) {
                x.exp = t + TICKET_SECS;
            }
            save(s);
        }
        Some(held)
    })
}

// ------------------------------------------------------------------ Zustimmungen

fn consent_path(app: &App) -> PathBuf {
    app.cfg.state.join("access.json")
}

fn consent_load(p: &Path) -> Value {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}))
}

/// Darf diese App (Karte) die Wallet dieses Kontos sehen?
pub fn wallet_allowed(app: &App, account: &str, card: &str) -> bool {
    let _g = CONSENT.lock().unwrap();
    consent_load(&consent_path(app))[account][card]["wallet"].is_u64()
}

/// Zustimmung geben (on) oder zuruecknehmen
pub fn set_wallet(app: &App, account: &str, card: &str, on: bool) -> Result<(), String> {
    let _g = CONSENT.lock().unwrap();
    let p = consent_path(app);
    let mut v = consent_load(&p);
    let all = v.as_object_mut().unwrap();
    if on {
        let mine = all.entry(account.to_string()).or_insert_with(|| json!({}));
        if !mine.is_object() {
            *mine = json!({});
        }
        mine.as_object_mut().unwrap().insert(card.to_string(), json!({"wallet": now()}));
    } else if let Some(mine) = all.get_mut(account).and_then(Value::as_object_mut) {
        mine.remove(card);
        if mine.is_empty() {
            all.remove(account);
        }
    }
    let tmp = p.with_extension("new");
    std::fs::write(&tmp, v.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

/// Alle Apps, denen dieses Konto die Wallet zeigt: {Karte: Zeitpunkt}
pub fn wallet_list(app: &App, account: &str) -> Value {
    let _g = CONSENT.lock().unwrap();
    let v = consent_load(&consent_path(app));
    let m: serde_json::Map<String, Value> = v[account]
        .as_object()
        .map(|o| o.iter().filter_map(|(k, x)| x["wallet"].as_u64().map(|t| (k.clone(), json!(t)))).collect())
        .unwrap_or_default();
    Value::Object(m)
}

/// Die freigeschaltete App zu einer Karte, wie die Nutzer sie sehen (apps.rs public_list): (Name, Seite)
pub fn enabled_app(app: &App, card: &str) -> Option<(String, String)> {
    let list = crate::apps::public_list(app);
    let a = list["apps"].as_array()?.iter().find(|a| a["card"] == card)?.clone();
    Some((a["name"].as_str().unwrap_or("").to_string(), a["page"].as_str()?.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    const OTHER: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

    fn store(tok: &str, exp: u64) -> HashMap<String, Ticket> {
        let mut m = HashMap::new();
        m.insert(hashed(tok), Ticket { account: "pk".into(), address: "nAddr".into(), card: "card".into(), page: PAGE.into(), exp });
        m
    }

    #[test]
    fn ticket_from_query_or_referer() {
        assert_eq!(from_request("v=abc&x=1", ""), ("abc".into(), "".into()));
        let r = format!("https://arcade.liteworlds.quest/content/{PAGE}?v=xyz");
        assert_eq!(from_request("", &r), ("xyz".into(), PAGE.into()));
        // ?v= der Anfrage gewinnt, die Seite kommt trotzdem aus dem Referer
        assert_eq!(from_request("v=own", &r), ("own".into(), PAGE.into()));
        // /content/<txid>i0 und Unterpfade
        let r = format!("https://h/content/{PAGE}i0/x.js?a=1&v=q#frag");
        assert_eq!(from_request("", &r), ("q".into(), PAGE.into()));
        // anderer Pfad: keine Seite
        assert_eq!(from_request("", "https://h/app/?id=1&v=t"), ("t".into(), "".into()));
        assert_eq!(from_request("", "https://h"), ("".into(), "".into()));
    }

    #[test]
    fn ticket_check() {
        let m = store("good", 1000);
        assert_eq!(check(&m, "good", PAGE, 999).map(|t| t.address), Some("nAddr".into()));
        assert!(check(&m, "good", "", 999).is_some()); // ohne Seite im Referer (Seite haengt v= selbst an)
        assert!(check(&m, "good", OTHER, 999).is_none()); // Ticket einer anderen Seite
        assert!(check(&m, "good", PAGE, 1000).is_none()); // abgelaufen
        assert!(check(&m, "bad", PAGE, 1).is_none()); // unbekannt
        assert!(check(&m, "", PAGE, 1).is_none());
        assert!(check(&m, &"g".repeat(65), PAGE, 1).is_none());
    }

    #[test]
    fn referer_page_must_match() {
        let m = store("t1", u64::MAX);
        let (tok, page) = from_request("", &format!("https://h/content/{OTHER}?v=t1"));
        assert!(check(&m, &tok, &page, 1).is_none());
        let (tok, page) = from_request("v=t1", &format!("https://h/content/{OTHER}"));
        assert!(check(&m, &tok, &page, 1).is_none());
        let (tok, page) = from_request("", &format!("https://h/content/{PAGE}?v=t1"));
        assert!(check(&m, &tok, &page, 1).is_some());
    }
}
