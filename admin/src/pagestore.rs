//! Was die Arcade fuer eine eingeschriebene Seite (App) aufbewahrt: arcade.storage (Stufe C der App-Laufzeit).
//! Eine Seite in der Sandbox hat keinen eigenen Browser-Speicher; das App-Fenster fragt hier fuer sie, je
//! angemeldetem Konto und Inschrift getrennt (<state>/pagestore/<sha256(Konto)[..16]>/<txid>.json). Die Seite selbst
//! erreicht das nie: nur das App-Fenster mit der Anmeldung des Nutzers (Cookie, SameSite=Strict; aus der Sandbox
//! geht kein Cookie mit).
//!
//! Grenzen wie die Arcades (dogecoin-arcade pagestore.py): Schluessel 256 Zeichen, Wert 64 KiB, je Seite 1 MiB und
//! 1000 Schluessel; je Schluessel die letzten 10 frueheren Werte (history).

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::App;

const MAX_KEY: usize = 256;
const MAX_VALUE: usize = 64 * 1024;
const MAX_TOTAL: usize = 1024 * 1024;
const MAX_KEYS: usize = 1000;
const HISTORY: usize = 10;

fn path(app: &App, account: &str, txid: &str) -> std::path::PathBuf {
    let who = hex::encode(&Sha256::digest(account.as_bytes())[..16]);
    app.cfg.state.join("pagestore").join(who).join(format!("{txid}.json"))
}

fn load(app: &App, account: &str, txid: &str) -> Value {
    std::fs::read_to_string(path(app, account, txid)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({"items": {}, "history": {}}))
}

fn save(app: &App, account: &str, txid: &str, v: &Value) -> Result<(), String> {
    let p = path(app, account, txid);
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("new");
    std::fs::write(&tmp, v.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

fn valid_page(txid: &str) -> Result<(), String> {
    if crate::appindex::inscription(txid).is_none() {
        return Err("no such inscription".into());
    }
    Ok(())
}

/// Alles fuer eine Seite
pub fn items(app: &App, account: &str, txid: &str) -> Result<Value, String> {
    valid_page(txid)?;
    Ok(load(app, account, txid)["items"].clone())
}

/// Eine Aenderung: set, remove, clear; oder history (fruehere Werte)
pub fn op(app: &App, account: &str, txid: &str, b: &Value) -> Result<Value, String> {
    valid_page(txid)?;
    let mut v = load(app, account, txid);
    let key = b["key"].as_str().unwrap_or("").to_string();
    let push_old = |v: &mut Value, k: &str| {
        if let Some(old) = v["items"].get(k).cloned() {
            let h = v["history"].as_object_mut().unwrap().entry(k.to_string()).or_insert(json!([]));
            let a = h.as_array_mut().unwrap();
            a.insert(0, json!({"value": old, "replaced": crate::now()}));
            a.truncate(HISTORY);
        }
    };
    match b["op"].as_str() {
        Some("set") => {
            let value = b["value"].as_str().map(String::from).unwrap_or_else(|| b["value"].to_string());
            if key.is_empty() || key.chars().count() > MAX_KEY {
                return Err(format!("a key is 1 to {MAX_KEY} characters"));
            }
            if value.chars().count() > MAX_VALUE {
                return Err(format!("a value is at most {MAX_VALUE} characters"));
            }
            let items = v["items"].as_object().cloned().unwrap_or_default();
            if !items.contains_key(&key) && items.len() >= MAX_KEYS {
                return Err(format!("at most {MAX_KEYS} keys per page"));
            }
            let total: usize = items.iter().filter(|(k, _)| **k != key).map(|(k, x)| k.len() + x.as_str().map_or(0, str::len)).sum::<usize>() + key.len() + value.len();
            if total > MAX_TOTAL {
                return Err("this page keeps at most 1 MiB".into());
            }
            push_old(&mut v, &key);
            v["items"][&key] = json!(value);
        }
        Some("remove") => {
            push_old(&mut v, &key);
            if let Some(o) = v["items"].as_object_mut() {
                o.remove(&key);
            }
        }
        Some("clear") => {
            let keys: Vec<String> = v["items"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
            for k in keys {
                push_old(&mut v, &k);
            }
            v["items"] = json!({});
        }
        Some("history") => {
            let h = v["history"].as_object().cloned().unwrap_or_default();
            let out: Map<String, Value> = if key.is_empty() { h } else { h.into_iter().filter(|(k, _)| *k == key).collect() };
            return Ok(json!({"history": out}));
        }
        _ => return Err("op must be set, remove, clear or history".into()),
    }
    save(app, account, txid, &v)?;
    Ok(json!({}))
}
