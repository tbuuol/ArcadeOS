//! Apps dieser Arcade: welche die Nutzer sehen und starten koennen. Standard: ALLE aus - ausser unserer eigenen
//! App LiteWorldsQuest (nicht eingeschrieben, laeuft auf dieser Box), die als Sonderfall an ist. Der Betreiber
//! schaltet in der Admin-Oberflaeche um (signiert mit seinem Konto; <state>/apps.json).
//!
//! Eingeschriebene Apps anderer Arcades (Inschriften auf der Chain) kennt diese Arcade noch nicht; sobald ihr Index
//! sie fuehrt, erscheinen sie hier, jede zuerst aus.

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
    url: "/lwq-beta/",
    about: "Das Rollenspiel dieser Arcade (läuft hier auf der Box, nicht eingeschrieben).",
    default_on: true,
}];

fn path(app: &App) -> std::path::PathBuf {
    app.cfg.state.join("apps.json")
}

fn settings(app: &App) -> Value {
    std::fs::read_to_string(path(app)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
}

fn is_on(v: &Value, b: &BuiltIn) -> bool {
    v["apps"][b.id].as_bool().unwrap_or(b.default_on)
}

/// Fuer die Admin-Oberflaeche: alle Apps mit Schalter
pub fn status(app: &App) -> Value {
    let v = settings(app);
    let list: Vec<Value> = BUILTIN
        .iter()
        .map(|b| json!({"id": b.id, "name": b.name, "url": b.url, "about": b.about, "builtin": true, "on": is_on(&v, b)}))
        .collect();
    json!({"apps": list, "inscribed": [], "inscribed_note": "Eingeschriebene Apps werden angezeigt, sobald der App-Index dieser Arcade sie führt."})
}

/// Fuer die Nutzer-Seite: nur die eingeschalteten
pub fn public_list(app: &App) -> Value {
    let v = settings(app);
    let list: Vec<Value> = BUILTIN.iter().filter(|b| is_on(&v, b)).map(|b| json!({"id": b.id, "name": b.name, "url": b.url, "about": b.about})).collect();
    json!({"apps": list})
}

/// Was der Betreiber signiert: "arcadeos-apps\n<Knoten>\n<Zufallszahl>\napps <id>=1|0 ..." (Reihenfolge BUILTIN)
pub fn message(node: &str, nonce: &str, apps: &Map<String, Value>) -> String {
    let parts: Vec<String> = BUILTIN.iter().map(|b| format!("{}={}", b.id, if apps.get(b.id).and_then(Value::as_bool) == Some(true) { 1 } else { 0 })).collect();
    format!("arcadeos-apps\n{node}\n{nonce}\napps {}", parts.join(" "))
}

pub fn save(app: &App, apps: &Map<String, Value>) -> Result<(), String> {
    let mut m = Map::new();
    for b in BUILTIN {
        m.insert(b.id.into(), json!(apps.get(b.id).and_then(Value::as_bool) == Some(true)));
    }
    let p = path(app);
    let tmp = p.with_extension("new");
    std::fs::write(&tmp, serde_json::to_string_pretty(&json!({"apps": m})).unwrap()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}
