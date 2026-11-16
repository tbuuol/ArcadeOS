//! Senden fuer eingeschriebene Apps (Stufe E2 der App-Laufzeit): POST /r/send ist eine BITTE, nie eine Aktion.
//!
//! * Die Seite (in der Sandbox, ohne Ursprung) bittet: Coins an eine Adresse oder @name, oder eine Inschrift, die
//!   der Betrachter besitzt. Wer bittet, zeigt das Ticket des Fensters (viewer.rs); die App muss die Wallet sehen
//!   duerfen (Zustimmung) und freigeschaltet sein. Antwort 202 {id, status: "pending"}; die Seite fragt
//!   GET /r/send/<id>, bis "sent" (mit txid), "denied", "failed" oder "expired" kommt (wie die Arcades, app.py
//!   r_send / _ask_the_reader / _page_send_told). Mit "ref" (die eigene Kennung der Seite) wird dieselbe Bitte nicht
//!   zweimal gestellt: die erste wird so gezeigt, wie sie jetzt steht.
//! * Das App-Fenster des Betrachters sieht die offenen Bitten seiner Seite (/api/sendreqs), laesst die Box die
//!   UNSIGNIERTE Transaktion bauen (fund(), Coins nur von der Adresse des Kontos), zeigt Empfaenger, Betrag oder
//!   Inschrift und Gebuehr. Nur nach dem Ja signiert der Browser (Tresor mit dem Passwort, crypto.js signTx) und die
//!   Box sendet genau diese Transaktion (send_offer prueft Inputs und Outputs). Die Box hat keine Schluessel.
//! * Inschrift uebertragen: OP_RETURN "arcd" + AnyData-Kopf + "INSC" 01 02 <txid, Leitungs-Reihenfolge>, dazu eine
//!   kleine Ausgabe an den Empfaenger, die nach determine_reference (appindex.rs reference) die Referenz ist; das
//!   Wechselgeld geht an den Absender und zaehlt dort nicht. Nur der jetzige Besitzer.
//! * Nur Pepecoin-Testnet: Adressen anderer Netze (auch Mainnet) sind unlesbar und werden abgelehnt.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::{now, App, ANYDATA, COIN, DUST, INSC, MARKER};

const ASK_FOR: u64 = 3600; // eine unbeantwortete Bitte verfaellt
const KEEP_FOR: u64 = 6 * 3600;
const MAX_WAITING: usize = 5; // offene Bitten je Betrachter
const MAX_REF: usize = 64;
const MAX_ROWS: usize = 5000;

#[derive(Clone, Debug)]
pub struct Ask {
    pub id: String,
    pub account: String,
    pub address: String,
    pub card: String,
    pub page: String,
    pub kind: String, // coins | inscription
    pub to: String,   // wie gebeten (Adresse oder @name)
    pub to_address: String,
    pub amount: String, // wie gebeten (Coins), "1" bei einer Inschrift
    pub sats: u64,
    pub inscription: String,
    pub name: String, // "#<Nummer> (<Name>)" einer Inschrift
    pub label: String,
    pub note: String,
    pub r#ref: String,
    pub status: String, // pending | approved | sent | denied | failed | expired
    pub txid: String,
    pub error: String,
    pub created: u64,
    pub offer: String, // das gebaute Angebot (fund), nur dieses wird gesendet
}

impl Ask {
    fn to_json(&self) -> Value {
        json!({"id": self.id, "account": self.account, "address": self.address, "card": self.card, "page": self.page,
            "kind": self.kind, "to": self.to, "to_address": self.to_address, "amount": self.amount, "sats": self.sats,
            "inscription": self.inscription, "name": self.name, "label": self.label, "note": self.note, "ref": self.r#ref,
            "status": self.status, "txid": self.txid, "error": self.error, "created": self.created})
    }
    fn from_json(v: &Value) -> Option<Ask> {
        let s = |k: &str| v[k].as_str().map(String::from);
        Some(Ask {
            id: s("id")?, account: s("account")?, address: s("address")?, card: s("card")?, page: s("page")?, kind: s("kind")?,
            to: s("to")?, to_address: s("to_address")?, amount: s("amount")?, sats: v["sats"].as_u64()?,
            inscription: s("inscription")?, name: s("name")?, label: s("label")?, note: s("note")?, r#ref: s("ref")?,
            status: s("status")?, txid: s("txid")?, error: s("error")?, created: v["created"].as_u64()?, offer: String::new(),
        })
    }
}

struct Store {
    path: std::path::PathBuf,
    rows: HashMap<String, Ask>,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

fn with<R>(app: &App, f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = STORE.lock().unwrap();
    let s = g.get_or_insert_with(|| {
        let path = app.cfg.state.join("sendreqs.json");
        let raw: Vec<Value> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let rows = raw.iter().filter_map(Ask::from_json).map(|a| (a.id.clone(), a)).collect();
        Store { path, rows }
    });
    tidy(s, now());
    f(s)
}

fn tidy(s: &mut Store, t: u64) {
    for a in s.rows.values_mut() {
        if a.status == "pending" && t.saturating_sub(a.created) > ASK_FOR {
            a.status = "expired".into();
        }
    }
    s.rows.retain(|_, a| t.saturating_sub(a.created) <= KEEP_FOR);
}

fn save(s: &Store) {
    let v: Vec<Value> = s.rows.values().map(Ask::to_json).collect();
    let tmp = s.path.with_extension("new");
    if std::fs::write(&tmp, Value::Array(v).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &s.path);
    }
}

/// Wie eine Seite eine Bitte sieht (app.py _page_send_told)
fn told(a: &Ask, confirmations: Option<u64>) -> Value {
    let mut v = json!({"id": a.id, "kind": a.kind, "to": a.to, "amount": a.amount, "propertyid": null,
        "status": a.status, "txid": a.txid, "error": a.error, "chain": "test"});
    if a.kind == "inscription" {
        v["inscription"] = json!(a.inscription);
    }
    if let Some(c) = confirmations {
        v["confirmations"] = json!(c);
        v["confirmed"] = json!(c > 0);
    }
    v
}

/// Was das Fenster fuer die Karte braucht
fn for_window(app: &App, a: &Ask) -> Value {
    let tag = app.index.lock().unwrap().by_addr.get(&a.to_address).cloned();
    json!({"id": a.id, "kind": a.kind, "to": a.to, "to_address": a.to_address, "to_tag": tag, "amount": a.amount, "sats": a.sats,
        "inscription": a.inscription, "name": a.name, "label": a.label, "note": a.note, "status": a.status, "created": a.created})
}

// ------------------------------------------------------------------ Bau

/// Betrag in PEP ("1.5", 1.5) -> Satoshi; hoechstens 8 Nachkommastellen
pub fn parse_amount(v: &Value) -> Option<u64> {
    let s = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let (whole, frac) = s.split_once('.').unwrap_or((&s, ""));
    if whole.is_empty() && frac.is_empty() || !whole.bytes().all(|c| c.is_ascii_digit()) || !frac.bytes().all(|c| c.is_ascii_digit()) || frac.len() > 8 || whole.len() > 10 {
        return None;
    }
    let w: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let f: u64 = if frac.is_empty() { 0 } else { format!("{frac:0<8}").parse().ok()? };
    w.checked_mul(COIN)?.checked_add(f)
}

/// Die Nutzlast einer Uebertragung (OP_RETURN-Daten): "arcd" + AnyData-Kopf + "INSC" 01 02 + txid, so wie sie angezeigt
/// wird (die Arcades schreiben bytes.fromhex(txid), nicht umgedreht)
pub fn transfer_data(txid: &str) -> Option<Vec<u8>> {
    let t = hex::decode(txid).ok().filter(|b| b.len() == 32)?;
    let mut d = MARKER.to_vec();
    d.extend_from_slice(&0u16.to_be_bytes());
    d.extend_from_slice(&ANYDATA.to_be_bytes());
    d.extend_from_slice(INSC);
    d.extend_from_slice(&[1, 2]);
    d.extend_from_slice(&t);
    Some(d)
}

/// Die Ausgaben, die eine Bitte verlangt (fund() haengt das Wechselgeld an den Absender an)
pub fn outputs(kind: &str, to_address: &str, sats: u64, inscription: &str) -> Result<Vec<(u64, String)>, String> {
    let k = crate::utxo::key_of(to_address).ok_or("not a Pepecoin testnet address")?;
    let to = crate::utxo::script_of(&k);
    match kind {
        "coins" => Ok(vec![(sats, to)]),
        "inscription" => Ok(vec![(0, crate::op_return_script(&transfer_data(inscription).ok_or("not an inscription id")?)), (DUST, to)]),
        _ => Err("a page may ask for coins or an inscription".into()),
    }
}

fn resolve_to(app: &App, to: &str) -> Result<String, String> {
    let t = to.trim();
    if let Some(tag) = t.strip_prefix('@') {
        let tag = tag.to_ascii_lowercase();
        return app.index.lock().unwrap().tags.get(&tag).map(|x| x.0.clone()).ok_or_else(|| format!("nobody holds @{tag} on this chain"));
    }
    // nur Pepecoin-Testnet: andere Netze (Mainnet) haben andere Versionsbytes und sind hier unlesbar
    crate::utxo::key_of(t).map(|_| t.to_string()).ok_or_else(|| "not a Pepecoin testnet address (mainnet is refused)".to_string())
}

// ------------------------------------------------------------------ die Seite bittet (r.rs)

/// POST /r/send: die Bitte des Betrachters `t` (geprueft: Ticket, freigeschaltet, Zustimmung)
pub fn file(app: &App, t: &crate::viewer::Ticket, b: &Value) -> (u16, Value) {
    if !b.is_object() {
        return (400, json!({"error": "send a JSON object"}));
    }
    let r#ref = match &b["ref"] {
        Value::Null => String::new(),
        Value::String(s) if !s.is_empty() && s.chars().count() <= MAX_REF => s.clone(),
        _ => return (400, json!({"error": format!("ref must be a string of 1 to {MAX_REF} characters, the page's own name for this send (asked again with the same ref, it is not sent twice)")})),
    };
    if b["chain"].as_str().is_some_and(|c| !c.trim().is_empty() && c.trim() != "test") {
        return (400, json!({"error": "this arcade sends on Pepecoin testnet only"}));
    }
    if !r#ref.is_empty() {
        if let Some(first) = with(app, |s| s.rows.values().find(|a| a.account == t.account && a.r#ref == r#ref).cloned()) {
            let mut v = told(&first, None);
            v["repeat"] = json!(true);
            return (200, v);
        }
    }
    let kind = b["kind"].as_str().unwrap_or("").to_string();
    if kind != "coins" && kind != "inscription" {
        return (400, json!({"error": "a page may ask for coins or an inscription"}));
    }
    let to = b["to"].as_str().unwrap_or("").trim().to_string();
    if to.is_empty() {
        return (400, json!({"error": "say who to pay and how much"}));
    }
    let to_address = match resolve_to(app, &to) {
        Ok(a) => a,
        Err(e) => return (400, json!({"error": e})),
    };
    if to_address == t.address {
        return (400, json!({"error": "that is the player's own address"}));
    }
    let (mut amount, mut sats, mut inscription, mut name) = ("1".to_string(), 0u64, String::new(), String::new());
    if kind == "coins" {
        let Some(s) = parse_amount(&b["amount"]) else { return (400, json!({"error": "say who to pay and how much (amount in coins, e.g. \"1.5\")"})) };
        if s < DUST {
            return (400, json!({"error": "at least 0.01 coins"}));
        }
        sats = s;
        amount = match &b["amount"] { Value::String(x) => x.trim().to_string(), v => v.to_string() };
    } else {
        let key = b["inscription"].as_str().unwrap_or("").trim().to_ascii_lowercase();
        let key = key.strip_prefix("/content/").unwrap_or(&key).to_string();
        let found = if !key.is_empty() && key.bytes().all(|c| c.is_ascii_digit()) {
            key.parse().ok().and_then(crate::appindex::by_number)
        } else {
            crate::appindex::inscription(&key).map(|i| (key.clone(), i))
        };
        let Some((txid, piece)) = found else { return (400, json!({"error": "there is no such inscription"})) };
        if piece.owner != t.address {
            return (400, json!({"error": "that inscription is not yours to send"}));
        }
        let title = serde_json::from_str::<Value>(&piece.json).ok().and_then(|j| j["name"].as_str().map(|s| s.chars().take(60).collect::<String>()));
        name = format!("#{}", piece.number) + &title.map(|n| format!(" ({n})")).unwrap_or_default();
        inscription = txid;
    }
    let clip = |k: &str, n: usize| b[k].as_str().unwrap_or("").chars().filter(|c| !c.is_control()).take(n).collect::<String>();
    with(app, |s| {
        let waiting = s.rows.values().filter(|a| a.account == t.account && a.status == "pending").count();
        if waiting >= MAX_WAITING {
            return (429, json!({"error": format!("{MAX_WAITING} requests are already waiting")}));
        }
        if s.rows.len() >= MAX_ROWS {
            return (503, json!({"error": "too many requests on this arcade, try again later"}));
        }
        let mut idb = [0u8; 12];
        getrandom::getrandom(&mut idb).ok();
        let id = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, idb);
        let a = Ask {
            id: id.clone(), account: t.account.clone(), address: t.address.clone(), card: t.card.clone(), page: t.page.clone(),
            kind, to: to.chars().take(120).collect(), to_address, amount: amount.chars().take(40).collect(), sats, inscription, name,
            label: clip("label", 60), note: clip("note", 140), r#ref, status: "pending".into(), txid: String::new(), error: String::new(),
            created: now(), offer: String::new(),
        };
        let v = told(&a, None);
        s.rows.insert(id, a);
        save(s);
        (202, v)
    })
}

/// GET /r/send/<id>: wie die Bitte steht (mit Bestaetigungen, wenn gesendet)
pub fn status(app: &App, id: &str) -> (u16, Value) {
    let Some(a) = with(app, |s| s.rows.get(id).cloned()) else { return (404, json!({"error": "no such request"})) };
    let conf = if a.status == "sent" && !a.txid.is_empty() {
        crate::rpc(&app.cfg, "getrawtransaction", json!([a.txid, 1])).ok().map(|v| v["confirmations"].as_u64().unwrap_or(0))
    } else {
        None
    };
    (200, told(&a, conf))
}

// ------------------------------------------------------------------ das Fenster antwortet (public.rs, mit Anmeldung)

/// Die offenen Bitten dieser App an dieses Konto
pub fn waiting(app: &App, account: &str, card: &str) -> Value {
    let mut rows: Vec<Ask> = with(app, |s| s.rows.values().filter(|a| a.account == account && a.card == card && a.status == "pending").cloned().collect());
    rows.sort_by_key(|a| a.created);
    json!({"requests": rows.iter().map(|a| for_window(app, a)).collect::<Vec<_>>()})
}

fn mine(app: &App, account: &str, id: &str) -> Result<Ask, (u16, String)> {
    let a = with(app, |s| s.rows.get(id).cloned()).ok_or((404, "no such request".to_string()))?;
    if a.account != account {
        return Err((404, "no such request".into()));
    }
    if a.status != "pending" {
        return Err((409, format!("this request is {}", a.status)));
    }
    if !crate::viewer::wallet_allowed(app, account, &a.card) || crate::viewer::enabled_app(app, &a.card).is_none() {
        return Err((403, "this app has no wallet access any more".into()));
    }
    Ok(a)
}

/// Die unsignierte Transaktion zur Bitte (Coins nur von der Adresse des Kontos): {offer, tx, fee}
pub fn build(app: &App, account: &str, id: &str) -> Result<Value, (u16, String)> {
    let a = mine(app, account, id)?;
    if a.kind == "inscription" && crate::appindex::inscription(&a.inscription).is_none_or(|i| i.owner != a.address) {
        fail(app, id, "that inscription is not yours to send any more");
        return Err((409, "that inscription is not yours to send any more".into()));
    }
    let outs = outputs(&a.kind, &a.to_address, a.sats, &a.inscription).map_err(|e| (400, e))?;
    let mut v = crate::fund(app, &a.address, outs, "#App-Senden")?;
    let offer = v["offer"].as_str().unwrap_or("").to_string();
    with(app, |s| {
        if let Some(x) = s.rows.get_mut(id) {
            x.offer = offer;
        }
    });
    v["request"] = for_window(app, &a);
    Ok(v)
}

/// Nach dem Ja: die im Browser signierte Transaktion senden (nur genau das gebaute Angebot)
pub fn send(app: &App, account: &str, id: &str, b: &Value) -> Result<Value, (u16, String)> {
    let a = mine(app, account, id)?;
    let offer = with(app, |s| s.rows.get(id).map(|x| x.offer.clone())).unwrap_or_default();
    if offer.is_empty() || b["offer"].as_str() != Some(offer.as_str()) {
        return Err((400, "not the transaction built for this request - nothing sent".into()));
    }
    if a.kind == "inscription" && crate::appindex::inscription(&a.inscription).is_none_or(|i| i.owner != a.address) {
        fail(app, id, "that inscription is not yours to send any more");
        return Err((409, "that inscription is not yours to send any more".into()));
    }
    set(app, id, |x| x.status = "approved".into());
    match crate::send_offer(app, &a.address, b) {
        Ok((txid, _)) => {
            set(app, id, |x| {
                x.status = "sent".into();
                x.txid = txid.clone();
            });
            crate::log(&format!("App-Bitte {} gesendet: {txid}", a.kind));
            Ok(json!({"ok": true, "txid": txid}))
        }
        Err((c, e)) => {
            // nicht gesendet: die Bitte wartet weiter (z. B. falsches Passwort, neu signieren)
            set(app, id, |x| x.status = "pending".into());
            Err((c, e))
        }
    }
}

/// Nein
pub fn deny(app: &App, account: &str, id: &str) -> Result<Value, (u16, String)> {
    mine(app, account, id)?;
    set(app, id, |x| {
        x.status = "denied".into();
        x.error = "the player said no".into();
    });
    Ok(json!({"ok": true}))
}

fn set(app: &App, id: &str, f: impl FnOnce(&mut Ask)) {
    with(app, |s| {
        if let Some(x) = s.rows.get_mut(id) {
            f(x);
        }
        save(s);
    });
}

fn fail(app: &App, id: &str, why: &str) {
    set(app, id, |x| {
        x.status = "failed".into();
        x.error = why.into();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts() {
        assert_eq!(parse_amount(&json!("1.5")), Some(150_000_000));
        assert_eq!(parse_amount(&json!(2)), Some(200_000_000));
        assert_eq!(parse_amount(&json!("0.00000001")), Some(1));
        assert_eq!(parse_amount(&json!(".5")), Some(50_000_000));
        assert_eq!(parse_amount(&json!("1.000000001")), None);
        assert_eq!(parse_amount(&json!("-1")), None);
        assert_eq!(parse_amount(&json!("1e3")), None);
        assert_eq!(parse_amount(&json!("")), None);
        assert_eq!(parse_amount(&json!(".")), None);
        assert_eq!(parse_amount(&json!(null)), None);
    }

    #[test]
    fn transfer_payload_names_the_shown_txid() {
        let txid = format!("ff{}", "00".repeat(31));
        let d = transfer_data(&txid).unwrap();
        assert_eq!(&d[..14], b"arcd\x00\x00\x00\xc8INSC\x01\x02");
        assert_eq!(d.len(), 14 + 32);
        assert_eq!(d[14], 0xff); // wie angezeigt (bytes.fromhex), nicht umgedreht
        assert!(transfer_data("xyz").is_none());
    }

    #[test]
    fn mainnet_and_foreign_addresses_refused() {
        // Pepecoin-Mainnet P2PKH (Version 56) und Bitcoin: unlesbar fuer key_of
        let main = crate::b58check(56, &[7u8; 20]);
        assert!(outputs("coins", &main, 100, "").is_err());
        assert!(outputs("coins", "1BoatSLRHtKNngkdXEeobR76b53LETtpyT", 100, "").is_err());
        let test = crate::b58check(crate::PKH, &[7u8; 20]);
        assert_eq!(outputs("coins", &test, 100, "").unwrap().len(), 1);
    }
}
