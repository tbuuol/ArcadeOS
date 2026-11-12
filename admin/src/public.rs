//! Oeffentlicher Teil: Konten fuer Nutzer der Arcade (arcade.<domain>), auf einem eigenen Port, den Caddy aus dem
//! Internet erreicht (127.0.0.1:8091). Wie in den Arcades (dogecoin-arcade accounts.py, wallet_js.js, signin.js):
//!   * Ein Konto ist ein oeffentlicher Ed25519-Anmeldeschluessel (aus den 12 Woertern, 24946'/1'/0'). Anmelden heisst:
//!     eine Zufallszahl der Box damit signieren. Kein Passwort auf der Box.
//!   * Das Wallet (die 12 Woerter, im Browser mit dem Passwort verschluesselt, Arcade-Format) liegt hier unter dem
//!     @name, damit "@name + Passwort" reicht. Die Box kann es nicht oeffnen.
//!   * Kennt die Box einen @name nicht, fragt sie die bekannten Arcades (GET /signin/<name>?here=1) und nimmt das
//!     Wallet nur, wenn laut Chain genau diese Adresse den @name haelt.
//!   * Startguthaben (Faucet) aus dem Wallet des Knotens, einmal je Konto, mit Grenzen je Tag und Adresse.
//!   * @name beanspruchen: Box baut unsigniert, Browser signiert, Box sendet nur genau diese Transaktion.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signature as EdSig, Verifier, VerifyingKey as EdKey};
use serde_json::{json, Value};

use super::*;

const ACCOUNT_ROUNDS_MIN: u64 = 600_000; // accounts.py MIN_SEAL_ROUNDS
const SESSION_DAYS: u64 = 30;

pub struct Pub {
    accounts: Mutex<Value>, // { pubkey: {tag, address, coinpub, blob, made, source, faucet, pending} }
    sessions: Mutex<HashMap<String, (String, u64)>>, // sha256(token) -> (pubkey, Ablauf)
    hits: Mutex<HashMap<String, Vec<u64>>>,
    elsewhere: Mutex<HashMap<String, (u64, Value)>>,
    host: String,
    peers: Vec<String>,
    faucet_amount: f64,
    faucet_daily: usize,
    sessions_path: std::path::PathBuf,
}

fn conf(app: &App, key: &str, default: &str) -> String {
    let s = std::fs::read_to_string(app.cfg.state.join("config")).unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(&format!("{key}="))).map(|v| v.trim().to_string()).unwrap_or_else(|| default.to_string())
}

impl Pub {
    pub fn new(app: &App) -> Pub {
        let accounts = std::fs::read_to_string(app.cfg.state.join("accounts.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({}));
        Pub {
            accounts: Mutex::new(accounts),
            sessions: Mutex::new(load_sessions(&app.cfg.state.join("sessions.json"))),
            hits: Default::default(),
            elsewhere: Default::default(),
            host: conf(app, "PUBLIC_HOST", "arcade.liteworlds.quest"),
            peers: conf(app, "ARCADE_PEERS", "https://pepecoinarcade.com,https://app.dogecoinarcade.com")
                .split(',')
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| s.starts_with("https://"))
                .collect(),
            faucet_amount: conf(app, "FAUCET_AMOUNT", "1").parse().unwrap_or(1.0),
            faucet_daily: conf(app, "FAUCET_DAILY", "20").parse().unwrap_or(20),
            sessions_path: app.cfg.state.join("sessions.json"),
        }
    }
    fn save(&self, app: &App) {
        let p = app.cfg.state.join("accounts.json");
        let tmp = p.with_extension("new");
        if std::fs::write(&tmp, serde_json::to_string(&*self.accounts.lock().unwrap()).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
    /// Hoechstens `n` Anfragen je `secs` Sekunden fuer diesen Schluessel
    fn limit(&self, key: &str, n: usize, secs: u64) -> bool {
        let mut h = self.hits.lock().unwrap();
        let t = now();
        let v = h.entry(key.to_string()).or_default();
        v.retain(|x| t - *x < secs);
        if v.len() >= n {
            return false;
        }
        v.push(t);
        if h.len() > 20000 {
            h.retain(|_, v| v.last().map(|x| t - *x < 3600).unwrap_or(false));
        }
        true
    }
    fn by_tag(&self, tag: &str) -> Option<(String, Value)> {
        let a = self.accounts.lock().unwrap();
        a.as_object()?.iter().find(|(_, v)| v["tag"] == tag).map(|(k, v)| (k.clone(), v.clone()))
    }
    fn session(&self, cookie: &str) -> Option<String> {
        let tok = cookie.split(';').find_map(|c| c.trim().strip_prefix("arcade="))?;
        let h = hex::encode(sha256(tok.as_bytes()));
        let s = self.sessions.lock().unwrap();
        s.get(&h).filter(|(_, exp)| *exp > now()).map(|(pk, _)| pk.clone())
    }
    fn new_session(&self, pubkey: &str) -> String {
        let mut b = [0u8; 32];
        getrandom::getrandom(&mut b).ok();
        let tok = hex::encode(b);
        let mut s = self.sessions.lock().unwrap();
        s.retain(|_, (_, exp)| *exp > now());
        s.insert(hex::encode(sha256(tok.as_bytes())), (pubkey.to_string(), now() + SESSION_DAYS * 86400));
        self.save_sessions(&s);
        tok
    }
    fn end_session(&self, cookie: &str) {
        let Some(tok) = cookie.split(';').find_map(|c| c.trim().strip_prefix("arcade=")) else { return };
        let mut s = self.sessions.lock().unwrap();
        if s.remove(&hex::encode(sha256(tok.as_bytes()))).is_some() {
            self.save_sessions(&s);
        }
    }
    /// Nur sha256(Token) -> (Konto, Ablauf): wer die Datei liest, kann sich damit nicht anmelden
    fn save_sessions(&self, s: &HashMap<String, (String, u64)>) {
        let tmp = self.sessions_path.with_extension("new");
        if std::fs::write(&tmp, serde_json::to_string(s).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.sessions_path);
        }
    }
}

fn load_sessions(path: &std::path::Path) -> HashMap<String, (String, u64)> {
    let mut s: HashMap<String, (String, u64)> = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    s.retain(|_, (_, exp)| *exp > now());
    s
}

struct Req<'a> {
    method: &'a str,
    path: &'a str,
    query: &'a str,
    body: &'a Value,
    ip: String,
    cookie: String,
}

type Out = (u16, Value, Option<String>); // Status, Antwort, Set-Cookie

fn bad(code: u16, msg: impl Into<String>) -> Out {
    (code, json!({"error": msg.into()}), None)
}
fn ok(v: Value) -> Out {
    (200, v, None)
}

fn ed_verify(pubkey_hex: &str, message: &str, sig_hex: &str) -> bool {
    let (Ok(pk), Ok(sig)) = (hex::decode(pubkey_hex), hex::decode(sig_hex)) else { return false };
    let (Ok(pk), Ok(sig)): (Result<[u8; 32], _>, Result<[u8; 64], _>) = (pk.try_into(), sig.try_into()) else { return false };
    let Ok(key) = EdKey::from_bytes(&pk) else { return false };
    key.verify(message.as_bytes(), &EdSig::from_bytes(&sig)).is_ok()
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Ein Wallet im Arcade-Format, sonst nichts (accounts.check_sealed + Form)
fn blob_ok(b: &Value) -> bool {
    b["kdf"] == "PBKDF2-SHA512"
        && b["iterations"].as_u64().map(|r| (ACCOUNT_ROUNDS_MIN..=10_000_000).contains(&r)).unwrap_or(false)
        && b["salt"].as_str().map(|s| is_hex(s, 32)).unwrap_or(false)
        && b["nonce"].as_str().map(|s| is_hex(s, 24)).unwrap_or(false)
        && b["sealed"].as_str().map(|s| s.len() >= 64 && s.len() <= 2048 && s.len() % 2 == 0 && s.bytes().all(|c| c.is_ascii_hexdigit())).unwrap_or(false)
}

fn login_message(p: &Pub, nonce: &str) -> String {
    format!("arcadeos-login\n{}\n{nonce}", p.host)
}

fn take_nonce(app: &App, nonce: &str) -> bool {
    app.nonces.lock().unwrap().remove(nonce).is_some()
}

/// Wallet von einer bekannten Arcade (wie dogecoin-arcade _vault_elsewhere), 10 min gemerkt
fn elsewhere(app: &App, p: &Pub, tag: &str) -> Option<Value> {
    if let Some((t, v)) = p.elsewhere.lock().unwrap().get(tag) {
        if now() - t < 600 {
            return Some(v.clone());
        }
    }
    let holder = app.index.lock().unwrap().tags.get(tag).map(|e| e.0.clone());
    for peer in &p.peers {
        let out = std::process::Command::new("/usr/bin/curl")
            .args(["-fsS", "-m", "8", "--max-filesize", "65536", "-H", "Accept: application/json"])
            .arg(format!("{peer}/signin/{tag}?here=1"))
            .output();
        let Ok(out) = out else { continue };
        if !out.status.success() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { continue };
        let (Some(pk), Some(addr)) = (v["pubkey"].as_str(), v["address"].as_str()) else { continue };
        if v["tag"] != tag || !is_hex(pk, 64) || !blob_ok(&v["blob"]) {
            continue;
        }
        // Haelt laut Chain jemand den Namen, dann nur dessen Wallet
        if holder.as_deref().map(|h| h != addr).unwrap_or(false) {
            continue;
        }
        let w = json!({"tag": tag, "pubkey": pk, "address": addr, "blob": v["blob"], "from": peer});
        p.elsewhere.lock().unwrap().insert(tag.to_string(), (now(), w.clone()));
        return Some(w);
    }
    None
}

fn signin(app: &App, p: &Pub, r: &Req, tag: &str) -> Out {
    let Ok(tag) = validate_tag(tag) else { return bad(404, "kein Konto mit diesem Namen") };
    if !p.limit(&format!("signin:{}", r.ip), 20, 300) {
        return bad(429, "zu viele Anmeldungen von hier, bitte kurz warten");
    }
    if !p.limit(&format!("signin-tag:{tag}"), 30, 3600) {
        return bad(429, "dieses Konto wurde in dieser Stunde zu oft abgefragt");
    }
    if let Some((pk, a)) = p.by_tag(&tag) {
        return ok(json!({"tag": tag, "pubkey": pk, "address": a["address"], "blob": a["blob"]}));
    }
    if r.query.contains("here=1") {
        return bad(404, "kein Konto mit diesem Namen hier");
    }
    match elsewhere(app, p, &tag) {
        Some(w) => ok(w),
        None => bad(404, "Kein Konto mit diesem Namen gefunden (weder hier noch bei den bekannten Arcades)."),
    }
}

fn signup(app: &App, p: &Pub, r: &Req) -> Out {
    if !p.limit(&format!("signup:{}", r.ip), 5, 3600) {
        return bad(429, "zu viele neue Konten von hier, bitte spaeter");
    }
    let b = r.body;
    let (Some(tag), Some(pk), Some(addr), Some(coinpub), Some(nonce), Some(sig)) = (
        b["tag"].as_str(), b["pubkey"].as_str(), b["address"].as_str(), b["coinpub"].as_str(), b["nonce"].as_str(), b["sig"].as_str(),
    ) else {
        return bad(400, "Angaben fehlen");
    };
    let tag = match validate_tag(tag) {
        Ok(t) => t,
        Err(e) => return bad(400, e),
    };
    if !is_hex(pk, 64) || !blob_ok(&b["blob"]) {
        return bad(400, "Konto oder Wallet nicht im erwarteten Format");
    }
    let Ok(cp) = hex::decode(coinpub) else { return bad(400, "coinpub") };
    if cp.len() != 33 || b58check(PKH, &hash160(&cp)) != addr {
        return bad(400, "Adresse passt nicht zum Schluessel");
    }
    if !take_nonce(app, nonce) || !ed_verify(pk, &login_message(p, nonce), sig) {
        return bad(400, "Anmeldesignatur passt nicht (Seite neu laden)");
    }
    if app.index.lock().unwrap().tags.contains_key(&tag) {
        return bad(409, format!("@{tag} ist schon vergeben"));
    }
    if p.by_tag(&tag).is_some() || p.accounts.lock().unwrap().get(pk).is_some() {
        return bad(409, "Dieses Konto oder dieser Name ist hier schon angelegt - bitte anmelden.");
    }
    p.accounts.lock().unwrap()[pk] = json!({"tag": tag, "address": addr, "coinpub": coinpub, "blob": b["blob"],
        "made": now(), "source": "here", "faucet": null, "pending": null});
    p.save(app);
    log(&format!("neues Konto @{tag} ({addr})"));
    let tok = p.new_session(pk);
    (200, json!({"ok": true}), Some(tok))
}

fn login(app: &App, p: &Pub, r: &Req) -> Out {
    if !p.limit(&format!("login:{}", r.ip), 30, 300) {
        return bad(429, "zu viele Anmeldungen von hier, bitte kurz warten");
    }
    let b = r.body;
    let (Some(pk), Some(nonce), Some(sig)) = (b["pubkey"].as_str(), b["nonce"].as_str(), b["sig"].as_str()) else {
        return bad(400, "Angaben fehlen");
    };
    if !is_hex(pk, 64) || !take_nonce(app, nonce) || !ed_verify(pk, &login_message(p, nonce), sig) {
        return bad(400, "Anmeldesignatur passt nicht (Seite neu laden)");
    }
    let known = p.accounts.lock().unwrap().get(pk).is_some();
    if !known {
        // Konto einer anderen Arcade: nur mit dem Wallet, das wir vorher selbst (signin) von dort geholt haben
        let tag = b["tag"].as_str().and_then(|t| validate_tag(t).ok()).unwrap_or_default();
        let w = p.elsewhere.lock().unwrap().get(&tag).map(|x| x.1.clone());
        let Some(w) = w.filter(|w| w["pubkey"] == pk) else { return bad(404, "Dieses Konto ist hier nicht bekannt.") };
        let addr = w["address"].as_str().unwrap_or("").to_string();
        let holder = app.index.lock().unwrap().tags.get(&tag).map(|e| e.0.clone());
        if holder.as_deref().map(|h| h != addr).unwrap_or(false) || p.by_tag(&tag).is_some() {
            return bad(409, "Der Name gehoert laut Chain jemand anderem.");
        }
        p.accounts.lock().unwrap()[pk] = json!({"tag": tag, "address": addr, "coinpub": null, "blob": w["blob"],
            "made": now(), "source": w["from"], "faucet": null, "pending": null});
        p.save(app);
        log(&format!("Konto @{tag} von {} uebernommen", w["from"].as_str().unwrap_or("?")));
    }
    let tok = p.new_session(pk);
    (200, json!({"ok": true}), Some(tok))
}

fn me(app: &App, p: &Pub, pk: &str) -> Out {
    let a = p.accounts.lock().unwrap().get(pk).cloned().unwrap_or(Value::Null);
    let addr = a["address"].as_str().unwrap_or("").to_string();
    let held = app.index.lock().unwrap().by_addr.get(&addr).cloned();
    let (conf, unconf) = match utxos(app, &addr) {
        Ok(u) => (
            u.iter().filter(|x| x["confirmations"].as_u64().unwrap_or(0) > 0).map(|x| x["sats"].as_u64().unwrap_or(0)).sum::<u64>(),
            u.iter().filter(|x| x["confirmations"].as_u64().unwrap_or(0) == 0).map(|x| x["sats"].as_u64().unwrap_or(0)).sum::<u64>(),
        ),
        Err(_) => (0, 0),
    };
    ok(json!({"pubkey": pk, "tag": a["tag"], "held": held, "address": addr, "confirmed": conf, "unconfirmed": unconf,
        "faucet": a["faucet"], "pending": a["pending"], "source": a["source"],
        "faucet_amount": p.faucet_amount, "names_ready": chain_tip(app).map(|t| app.index.lock().unwrap().height + 1 >= t).unwrap_or(false)}))
}

fn faucet(app: &App, p: &Pub, r: &Req, pk: &str) -> Out {
    let addr = {
        let a = p.accounts.lock().unwrap();
        if !a[pk]["faucet"].is_null() {
            return bad(409, "Das Startguthaben gibt es einmal je Konto.");
        }
        a[pk]["address"].as_str().unwrap_or("").to_string()
    };
    if !p.limit(&format!("faucet:{}", r.ip), 1, 86400) {
        return bad(429, "Von hier wurde heute schon Startguthaben geholt.");
    }
    if !p.limit("faucet:alle", p.faucet_daily, 86400) {
        return bad(429, "Fuer heute ist das Startguthaben aufgebraucht, bitte morgen wieder.");
    }
    match rpc(&app.cfg, "sendtoaddress", json!([addr, p.faucet_amount])) {
        Ok(txid) => {
            p.accounts.lock().unwrap()[pk]["faucet"] = json!({"txid": txid, "at": now(), "amount": p.faucet_amount});
            p.save(app);
            log(&format!("Startguthaben {} PEP an {addr}", p.faucet_amount));
            ok(json!({"txid": txid, "amount": p.faucet_amount}))
        }
        Err(e) => bad(503, format!("Die Faucet ist gerade leer oder nicht bereit ({e}).")),
    }
}

fn claim_public(app: &App, p: &Pub, pk: &str, tag: &str) -> Out {
    let addr = p.accounts.lock().unwrap()[pk]["address"].as_str().unwrap_or("").to_string();
    match build_claim(app, &addr, tag) {
        Ok(v) => ok(v),
        Err((c, e)) => bad(c, e),
    }
}

fn send_public(app: &App, p: &Pub, pk: &str, b: &Value) -> Out {
    let addr = p.accounts.lock().unwrap()[pk]["address"].as_str().unwrap_or("").to_string();
    match send_offer(app, &addr, b) {
        Ok((txid, tag)) => {
            p.accounts.lock().unwrap()[pk]["pending"] = json!({"tag": tag, "txid": txid, "at": now()});
            p.accounts.lock().unwrap()[pk]["tag"] = json!(tag);
            p.save(app);
            ok(json!({"txid": txid, "tag": tag}))
        }
        Err((c, e)) => bad(c, e),
    }
}

fn route(app: &App, p: &Pub, r: &Req) -> Out {
    if let Some(tag) = r.path.strip_prefix("/signin/") {
        if r.method == "GET" {
            return signin(app, p, r, tag);
        }
    }
    let me_pk = p.session(&r.cookie).filter(|pk| p.accounts.lock().unwrap().get(pk).is_some());
    match (r.method, r.path) {
        ("GET", "/api/challenge") => {
            if !p.limit(&format!("challenge:{}", r.ip), 60, 300) {
                return bad(429, "zu viele Anfragen");
            }
            let (c, v) = challenge(app);
            if c != 200 {
                return (c, v, None);
            }
            let nonce = v["nonce"].as_str().unwrap_or("").to_string();
            ok(json!({"nonce": nonce, "message": login_message(p, &nonce)}))
        }
        ("GET", "/api/name") => {
            let q = r.query.split('&').find_map(|kv| kv.strip_prefix("tag=")).unwrap_or("");
            // "gehoert dir" nur fuer das angemeldete Konto, nie fuer den Betreiber der Box
            let mine = me_pk.as_ref().and_then(|pk| p.accounts.lock().unwrap()[pk.as_str()]["address"].as_str().map(String::from));
            ok(name_check_for(app, &q.replace("%40", "@"), mine.as_deref()))
        }
        ("POST", "/api/signup") => signup(app, p, r),
        ("POST", "/api/login") => login(app, p, r),
        ("POST", "/api/logout") => {
            p.end_session(&r.cookie);
            (200, json!({"ok": true}), Some(String::new()))
        }
        (_, path) if path.starts_with("/api/") => {
            let Some(pk) = me_pk else { return bad(401, "bitte anmelden") };
            match (r.method, path) {
                ("GET", "/api/me") => me(app, p, &pk),
                ("POST", "/api/faucet") => faucet(app, p, r, &pk),
                ("POST", "/api/claim") => claim_public(app, p, &pk, r.body["tag"].as_str().unwrap_or("")),
                ("POST", "/api/send") => send_public(app, p, &pk, r.body),
                _ => bad(404, "unbekannt"),
            }
        }
        _ => bad(404, "unbekannt"),
    }
}

pub fn serve(app: Arc<App>, listen: String) {
    let p = Pub::new(&app);
    let server = match tiny_http::Server::http(&listen) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("oeffentlich: kann nicht auf {listen} lauschen: {e}"));
            return;
        }
    };
    log(&format!("oeffentlich auf {listen} (Host {}, Arcades {:?})", p.host, p.peers));
    for mut req in server.incoming_requests() {
        let mut body = String::new();
        let _ = req.as_reader().take(64 * 1024).read_to_string(&mut body);
        let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let hdr = |name: &'static str| {
            req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string()).unwrap_or_default()
        };
        // Caddy setzt X-Forwarded-For (fremde Angaben des Clients verwirft es); direkt kommt hier niemand hin
        let ip = hdr("X-Forwarded-For").split(',').next().unwrap_or("").trim().to_string();
        let cookie = hdr("Cookie");
        let url = req.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
        let r = Req { method: req.method().as_str(), path, query, body: &body, ip: if ip.is_empty() { "lokal".into() } else { ip }, cookie };
        let (code, v, set) = route(&app, &p, &r);
        let mut resp = tiny_http::Response::from_string(v.to_string())
            .with_status_code(code)
            .with_header("Content-Type: application/json; charset=utf-8".parse::<tiny_http::Header>().unwrap())
            .with_header("Cache-Control: no-store".parse::<tiny_http::Header>().unwrap());
        if let Some(tok) = set {
            let c = if tok.is_empty() {
                "Set-Cookie: arcade=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0".to_string()
            } else {
                format!("Set-Cookie: arcade={tok}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={}", SESSION_DAYS * 86400)
            };
            resp = resp.with_header(c.parse::<tiny_http::Header>().unwrap());
        }
        let _ = req.respond(resp);
    }
}
