//! Apps dieser Arcade: welche die Nutzer sehen und starten koennen. Standard: ALLE aus - ausser unserer eigenen
//! App LiteWorldsQuest (nicht eingeschrieben, laeuft auf dieser Box), die als Sonderfall an ist. Der Betreiber
//! schaltet in der Admin-Oberflaeche um (signiert mit seinem Konto; <state>/apps.json).
//!
//! Eingeschriebene Apps (App-Karten auf der Chain, appindex.rs) sind jede zuerst aus; ihr Schalter haengt an
//! Ersteller und Name (app:<Ersteller>:<name>), so gilt er auch fuer neue Versionen derselben App.

use serde_json::{json, Map, Value};

use crate::App;

pub struct BuiltIn {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub about: &'static str,
    /// an, solange der Betreiber nichts anderes sagt
    pub default_on: bool,
}

pub const BUILTIN: &[BuiltIn] = &[BuiltIn {
    id: "lwq",
    name: "LiteWorldsQuest",
    url: "/lwq-beta/client/",
    about: "Das Rollenspiel dieser Arcade (läuft hier auf der Box, nicht eingeschrieben).",
    default_on: true,
}];

fn path(app: &App) -> std::path::PathBuf {
    app.cfg.state.join("apps.json")
}

fn settings(app: &App) -> Value {
    std::fs::read_to_string(path(app)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
}

fn is_on(v: &Value, id: &str, default_on: bool) -> bool {
    v["apps"][id].as_bool().unwrap_or(default_on)
}

/// Alle Schalter, die es gibt: eigene Apps und je eingeschriebene App (Ersteller+Name) einer
fn known_ids() -> Vec<String> {
    let mut v: Vec<String> = BUILTIN.iter().map(|b| b.id.to_string()).collect();
    for c in crate::appindex::cards() {
        v.push(crate::appindex::app_key(&c.creator, c.game["name"].as_str().unwrap_or("")));
    }
    v
}

/// Fuer die Admin-Oberflaeche: alle Apps mit Schalter
pub fn status(app: &App) -> Value {
    let v = settings(app);
    let mut list: Vec<Value> = BUILTIN
        .iter()
        .map(|b| json!({"id": b.id, "name": b.name, "url": b.url, "about": b.about, "builtin": true, "on": is_on(&v, b.id, b.default_on)}))
        .collect();
    let names = app.index.lock().unwrap().by_addr.clone();
    for c in crate::appindex::cards() {
        let g = &c.game;
        let id = crate::appindex::app_key(&c.creator, g["name"].as_str().unwrap_or(""));
        list.push(json!({"id": id, "name": g["name"], "about": g["description"], "version": g["version"], "genre": g["genre"],
            "creator": c.creator, "tag": names.get(&c.creator), "txid": c.txid, "height": c.h,
            "wallet": g["wallet"], "media": g["media"], "multiplayer": g["multiplayer"], "builtin": false, "on": is_on(&v, &id, false)}));
    }
    let (h, n, _) = crate::appindex::progress();
    json!({"apps": list, "index": {"height": h, "inscriptions": n},
           "inscribed_note": "Eingeschriebene Apps laufen auf dieser Arcade noch nicht: der Schalter bestimmt, welche später angeboten werden."})
}

/// Fuer die Nutzer-Seite und das App-Fenster: nur die eingeschalteten
pub fn public_list(app: &App) -> Value {
    let v = settings(app);
    let mut list: Vec<Value> = BUILTIN.iter().filter(|b| is_on(&v, b.id, b.default_on)).map(|b| json!({"id": b.id, "name": b.name, "url": b.url, "about": b.about})).collect();
    // eingeschriebene, die der Betreiber eingeschaltet hat: Karte und die Seite, die Spielen oeffnet (play, sonst die Karte)
    let names = app.index.lock().unwrap().by_addr.clone();
    for c in crate::appindex::cards() {
        let g = &c.game;
        let id = crate::appindex::app_key(&c.creator, g["name"].as_str().unwrap_or(""));
        if !is_on(&v, &id, false) {
            continue;
        }
        let page = g["play"].as_str().filter(|p| crate::appindex::inscription(p).is_some_and(|i| i.creator == c.creator)).unwrap_or(&c.txid).to_string();
        list.push(json!({"id": id, "name": g["name"], "about": g["description"], "card": c.txid, "page": page, "url": format!("/app/?id={}", c.txid),
            "by": names.get(&c.creator), "wallet": g["wallet"], "media": g["media"], "version": g["version"]}));
    }
    json!({"apps": list})
}

/// Was der Betreiber signiert: "arcadeos-apps\n<Knoten>\n<Zufallszahl>\napps <id>=1|0 ..." ueber die gesendeten
/// Schalter, nach id sortiert (so baut die Seite sie auch)
pub fn message(node: &str, nonce: &str, apps: &Map<String, Value>) -> String {
    let parts: Vec<String> = apps.iter().map(|(k, v)| format!("{k}={}", if v.as_bool() == Some(true) { 1 } else { 0 })).collect();
    format!("arcadeos-apps\n{node}\n{nonce}\napps {}", parts.join(" "))
}

pub fn save(app: &App, apps: &Map<String, Value>) -> Result<(), String> {
    let known = known_ids();
    if let Some(k) = apps.keys().find(|k| !known.contains(k)) {
        return Err(format!("unbekannte App {k}"));
    }
    let mut v = settings(app);
    if !v["apps"].is_object() {
        v = json!({"apps": {}});
    }
    for (k, on) in apps {
        v["apps"][k] = json!(on.as_bool() == Some(true));
    }
    let p = path(app);
    let tmp = p.with_extension("new");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}
