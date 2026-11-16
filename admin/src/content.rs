//! Inschriften ausliefern: GET /content/<txid> (Stufe A der App-Laufzeit). Eine Inschrift wird aus ihren Stuecken
//! zusammengesetzt (die Transaktionen kennt appindex.rs), gegen den sha256 ihres Manifests geprueft und auf der
//! Platte zwischengespeichert (<state>/content/, unveraenderlich: eine txid ist immer derselbe Inhalt).
//!
//! Ausgeliefert wie bei den Arcades (dogecoin-arcade web content.py CONTENT_HEADERS): in einer Sandbox ohne
//! eigenen Ursprung - die Seite hat keine Cookies, keinen Zugriff auf die Seite um sie herum und keinen auf den
//! Browser-Speicher dieser Arcade (wo die verschluesselten Woerter der Nutzer liegen) -, laden darf sie nur von
//! dieser Arcade, nichts von fremden Servern. Eingebettet werden darf sie nur von Seiten dieser Arcade.
//!
//! Eigener Server auf 127.0.0.1 (ADMIN_CONTENT_LISTEN, Standard :8098) mit mehreren Arbeitern, damit das Holen von
//! Stuecken den Rest nicht aufhaelt. Caddy reicht /content/* der Arcade-Seite hierher.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::{log, rpc, App};

const WORKERS: usize = 4;

/// Die Kopfzeilen jeder Inschrift (wie die Arcades, dazu frame-ancestors: nur Seiten dieser Arcade betten ein)
const CSP: &str = "sandbox allow-scripts allow-pointer-lock; default-src 'self' data: blob:; \
    script-src 'self' 'unsafe-inline' 'unsafe-eval' data: blob:; style-src 'self' 'unsafe-inline' data:; \
    img-src 'self' data: blob:; media-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; \
    frame-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";

fn cache_dir(app: &App) -> std::path::PathBuf {
    app.cfg.state.join("content")
}

/// Der Inhalt einer Inschrift: (Inhaltstyp, Bytes) oder (HTTP-Status, Grund)
pub fn get(app: &App, txid: &str) -> Result<(String, Vec<u8>), (u16, String)> {
    if txid.len() != 64 || !txid.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
        return Err((400, "not an inscription id".into()));
    }
    let ins = crate::appindex::inscription(txid).ok_or((404, "no such inscription".to_string()))?;
    let file = cache_dir(app).join(txid);
    if let Ok(b) = std::fs::read(&file) {
        if <[u8; 32]>::from(Sha256::digest(&b)) == ins.sha {
            return Ok((ins.ctype, b));
        }
    }
    let marker = crate::msgs::marker();
    let mut stream = Vec::new();
    for t in &ins.pieces {
        let hexs = rpc(&app.cfg, "getrawtransaction", serde_json::json!([t, 0])).map_err(|e| (503, format!("piece {t}: {e}")))?;
        let raw = hex::decode(hexs.as_str().unwrap_or("")).map_err(|_| (502, "piece is not hex".to_string()))?;
        let tx = crate::utxo::parse_tx(&raw).map_err(|e| (502, e))?;
        let (from, data) = crate::msgs::anydata(&tx, &marker).ok_or((502, "piece carries no data".to_string()))?;
        if from != ins.creator {
            return Err((502, "piece by somebody else".into()));
        }
        let (_, _, body) = crate::appindex::chunk(&data).ok_or((502, "piece is not a chunk".to_string()))?;
        stream.extend_from_slice(body);
    }
    let (total, sha, _, _, rest) = crate::appindex::manifest(&stream).ok_or((502, "no manifest".to_string()))?;
    let total = total as usize;
    if rest.len() < total || <[u8; 32]>::from(Sha256::digest(&rest[..total])) != sha {
        return Err((502, "content does not match its hash".into()));
    }
    let content = rest[..total].to_vec();
    let _ = std::fs::create_dir_all(cache_dir(app));
    let tmp = file.with_extension("new");
    if std::fs::write(&tmp, &content).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
    Ok((ins.ctype, content))
}

/// Inhaltstypen, die als Seite oder Bild/Ton gezeigt werden duerfen, gekuerzt; alles andere als Download
fn safe_type(ct: &str) -> String {
    let ct: String = ct.chars().filter(|c| !c.is_control()).take(120).collect();
    if ct.is_empty() { "application/octet-stream".into() } else { ct }
}

pub fn serve(app: Arc<App>, listen: String) {
    if !listen.starts_with("127.0.0.1:") {
        return log("content: nur auf 127.0.0.1");
    }
    let server = match tiny_http::Server::http(&listen) {
        Ok(s) => Arc::new(s),
        Err(e) => return log(&format!("content: kann nicht auf {listen} lauschen: {e}")),
    };
    log(&format!("content auf {listen}"));
    for _ in 0..WORKERS {
        let (server, app) = (server.clone(), app.clone());
        std::thread::spawn(move || loop {
            let Ok(req) = server.recv() else { return };
            let url = req.url().to_string();
            let path = url.split('?').next().unwrap_or("");
            let h = |s: &str| s.parse::<tiny_http::Header>().unwrap();
            // Lese-Schnittstellen fuer Apps (r.rs)
            if req.method().as_str() == "GET" {
                let query = url.split_once('?').map(|x| x.1).unwrap_or("");
                if let Some((code, v)) = crate::r::handle(&app, path, query) {
                    let _ = req.respond(tiny_http::Response::from_string(v.to_string()).with_status_code(code)
                        .with_header(h("Content-Type: application/json; charset=utf-8"))
                        .with_header(h("Access-Control-Allow-Origin: *"))
                        .with_header(h("Cache-Control: no-store")));
                    continue;
                }
            }
            let resp = match (req.method().as_str(), path.strip_prefix("/content/")) {
                ("GET", Some(t)) | ("HEAD", Some(t)) => match get(&app, t) {
                    Ok((ct, b)) => tiny_http::Response::from_data(b)
                        .with_header(h(&format!("Content-Type: {}", safe_type(&ct))))
                        .with_header(h(&format!("Content-Security-Policy: {CSP}")))
                        .with_header(h("X-Content-Type-Options: nosniff"))
                        .with_header(h("Referrer-Policy: same-origin"))
                        .with_header(h("Access-Control-Allow-Origin: *"))
                        .with_header(h("Cache-Control: public, max-age=31536000, immutable")),
                    Err((code, e)) => tiny_http::Response::from_data(e.into_bytes()).with_status_code(code)
                        .with_header(h("Content-Type: text/plain; charset=utf-8")).with_header(h("Cache-Control: no-store")),
                },
                _ => tiny_http::Response::from_data(b"not found".to_vec()).with_status_code(404),
            };
            let _ = req.respond(resp);
        });
    }
}
