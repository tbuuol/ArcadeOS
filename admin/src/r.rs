//! Lese-Schnittstellen fuer Apps (Stufe B der App-Laufzeit): was eingeschriebene Seiten unter /r/... fragen, in
//! der Form der Arcades (dogecoin-arcade web/app.py r_*, content.py describe). Nur Lesen, nur oeffentliche Daten der
//! Chain: Inschriften, @names, Blockhoehe. Ohne Anmeldung; jede Seite darf fragen (CORS *).
//!
//! Noch nicht: Profile (Bio, Bild: nur @name und Adresse), Sammlungen (collection/edition: null),
//! Tokens, Maerkte, Ords (dafuer fehlt ein Token-Ledger). Besitz: Uebertragungen, Tausch und Buendel (appindex.rs).

use serde_json::{json, Value};

use crate::appindex::{self, Insc};
use crate::{rpc, App};

fn q<'a>(query: &'a str, k: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
}

fn num(query: &str, k: &str, d: i64) -> i64 {
    q(query, k).and_then(|v| v.parse().ok()).unwrap_or(d)
}

/// Eine Inschrift, wie eine Seite sie sieht (content.py describe); nie die Bytes
fn describe(app: &App, txid: &str, i: &Insc) -> Value {
    let names = &app.index.lock().unwrap().by_addr;
    let parsed: Value = serde_json::from_str(&i.json).unwrap_or(Value::Null);
    json!({
        "id": txid, "number": i.number, "creator": i.creator, "owner": i.owner,
        "creatortag": names.get(&i.creator), "ownertag": names.get(&i.owner),
        "block": i.h, "contenttype": i.ctype, "length": i.total, "sha256": hex::encode(i.sha),
        "transactions": i.pieces.len(), "held": false,
        "json": if i.json.is_empty() { Value::Null } else { parsed }, "rawjson": i.json,
        "collection": null, "edition": null,
    })
}

fn find(key: &str) -> Option<(String, Insc)> {
    let k = key.trim().to_ascii_lowercase();
    let k = k.strip_prefix("/content/").unwrap_or(&k).to_string();
    if !k.is_empty() && k.bytes().all(|c| c.is_ascii_digit()) {
        return appindex::by_number(k.parse().ok()?);
    }
    appindex::inscription(&k).map(|i| (k, i))
}

fn list(app: &App, query: &str, owner: Option<&str>, default_limit: i64) -> Value {
    let limit = num(query, "limit", default_limit).clamp(1, 1000) as usize;
    let offset = num(query, "offset", 0).max(0) as usize;
    let after = num(query, "after", -1);
    let creator = q(query, "creator").filter(|s| !s.is_empty());
    let owner = owner.or(q(query, "owner").filter(|s| !s.is_empty()));
    let rows: Vec<Value> = appindex::all_newest_first()
        .into_iter()
        .filter(|(_, i)| creator.is_none_or(|c| i.creator == c) && owner.is_none_or(|o| i.owner == o) && i.number as i64 > after)
        .skip(offset)
        .take(limit)
        .map(|(t, i)| describe(app, &t, &i))
        .collect();
    Value::Array(rows)
}

/// GET /r/wallet: die Wallet, die diese Seite anschaut (Stufe E1, viewer.rs), im Format der Arcades (app.py
/// _wallet_answer). Der Betrachter ist allein das Konto, das das Ticket der Seite nennt, und nur solange es dieser
/// App zugestimmt hat und die App freigeschaltet ist. Sonst "off", nie die Wallet des Betreibers.
/// Der Betrachter dieser Anfrage, wenn er der App die Wallet zeigt und die App freigeschaltet ist; sonst 403 "off"
fn allowed_viewer(app: &App, query: &str, referer: &str) -> Result<crate::viewer::Ticket, (u16, Value)> {
    let Some(t) = crate::viewer::viewer_of(app, query, referer) else {
        return Err((403, json!({"wallet": "off", "error": "nobody signed in is looking at this page"})));
    };
    if crate::viewer::enabled_app(app, &t.card).is_none_or(|(_, page)| page != t.page) {
        return Err((403, json!({"wallet": "off", "error": "this app is not enabled on this arcade"})));
    }
    if !crate::viewer::wallet_allowed(app, &t.account, &t.card) {
        return Err((403, json!({"wallet": "off", "error": "the player has not given this app wallet access (ask arcade.access.wallet() first)"})));
    }
    Ok(t)
}

/// POST /r/send (sendreq.rs): eine Bitte, nie eine Aktion
pub fn post(app: &App, path: &str, query: &str, referer: &str, body: &Value) -> Option<(u16, Value)> {
    if path != "/r/send" {
        return None;
    }
    Some(match allowed_viewer(app, query, referer) {
        Ok(t) => crate::sendreq::file(app, &t, body),
        Err(e) => e,
    })
}

fn wallet(app: &App, query: &str, referer: &str) -> (u16, Value) {
    let t = match allowed_viewer(app, query, referer) {
        Ok(t) => t,
        Err(e) => return e,
    };
    // Coins aus dem UTXO-Index (bestaetigt und unbestaetigt), in PEP; null solange der Index aufbaut
    let spendable = crate::utxo::coins(app, &t.address)
        .ok()
        .map(|c| c.iter().map(|x| x["sats"].as_u64().unwrap_or(0)).sum::<u64>() as f64 / crate::COIN as f64);
    let owned = appindex::all_newest_first().iter().filter(|(_, i)| i.owner == t.address).count();
    let tag = app.index.lock().unwrap().by_addr.get(&t.address).cloned();
    (200, json!({
        "network": "test", "mainnet": false, "addresses": [t.address], "tag": tag,
        "coin": {"spendable": spendable, "ticker": "tPEP"}, "spendable": spendable,
        "tokens": [], "inscriptions": owned, "wallet": "account",
    }))
}

/// (HTTP-Status, Antwort) fuer GET /r/...; None: keine dieser Schnittstellen
pub fn handle(app: &App, path: &str, query: &str, referer: &str) -> Option<(u16, Value)> {
    let p = path.strip_prefix("/r/")?;
    let parts: Vec<&str> = p.split('/').collect();
    Some(match parts.as_slice() {
        ["inscriptions"] => (200, list(app, query, None, 100)),
        ["inscriptions", "count"] => {
            let creator = q(query, "creator").filter(|s| !s.is_empty());
            let owner = q(query, "owner").filter(|s| !s.is_empty());
            let n = appindex::all_newest_first().iter().filter(|(_, i)| creator.is_none_or(|c| i.creator == c) && owner.is_none_or(|o| i.owner == o)).count();
            (200, json!({"count": n}))
        }
        ["inscriptions", address] => (200, list(app, query, Some(address), 200)),
        ["inscription", key] => match find(key) {
            Some((t, i)) => (200, describe(app, &t, &i)),
            None => (404, json!({"error": "no such inscription"})),
        },
        ["metadata", key] => match find(key) {
            Some((_, i)) => (200, serde_json::from_str(&i.json).unwrap_or(Value::String(i.json))),
            None => (404, json!({"error": "no such inscription"})),
        },
        ["tag", name] => {
            let t = name.trim().trim_start_matches('@').to_ascii_lowercase();
            let addr = app.index.lock().unwrap().tags.get(&t).map(|x| x.0.clone());
            (200, json!({"tag": t, "address": addr}))
        }
        ["address", address] => {
            let tag = app.index.lock().unwrap().by_addr.get(*address).cloned();
            (200, json!({"address": address, "tag": tag}))
        }
        ["profile", tag] => {
            let t = tag.trim().trim_start_matches('@').to_ascii_lowercase();
            match app.index.lock().unwrap().tags.get(&t).map(|x| x.0.clone()) {
                Some(addr) => (200, json!({"tag": t, "address": addr, "pending": false, "mainnet": "", "bio": "", "url": "", "picture": null, "content": ""})),
                None => (404, json!({"error": "nobody holds that tag on this chain"})),
            }
        }
        ["wallet"] => wallet(app, query, referer),
        ["send", id] => crate::sendreq::status(app, id),
        // Ist eine Transaktion im Block? (fuer Tausche: settled erst nach dem Block)
        ["tx", txid] if txid.len() == 64 && txid.bytes().all(|c| c.is_ascii_hexdigit()) => {
            match rpc(&app.cfg, "getrawtransaction", json!([txid.to_ascii_lowercase(), 1])) {
                Ok(v) => {
                    let c = v["confirmations"].as_u64().unwrap_or(0);
                    (200, json!({"txid": txid, "known": true, "confirmed": c > 0, "confirmations": c, "conflicted": false}))
                }
                Err(_) => (200, json!({"txid": txid, "known": false, "confirmed": false, "confirmations": 0, "conflicted": false})),
            }
        }
        ["blockheight"] => (200, json!(appindex::progress().0)),
        ["blocktime"] => {
            let t = rpc(&app.cfg, "getblockcount", json!([]))
                .and_then(|n| rpc(&app.cfg, "getblockhash", json!([n])))
                .and_then(|h| rpc(&app.cfg, "getblockheader", json!([h])))
                .map(|b| b["time"].clone())
                .unwrap_or(Value::Null);
            (200, t)
        }
        _ => (404, json!({"error": "not answered by this arcade yet"})),
    })
}
