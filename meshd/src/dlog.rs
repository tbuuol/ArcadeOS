//! Debug-Log: eine JSON-Zeile pro Ereignis in einer Datei (Standard /state/mesh/debug.log), damit
//! sich spaeter nachlesen laesst, was im Mesh passiert ist. Rotiert bei MAX_BYTES auf .1 und .2
//! (hoechstens ~3 × MAX_BYTES). Zusaetzlich eine kurze Zeile auf stderr (landet im Syslog).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

struct Log {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    started: Instant,
}

static LOG: OnceLock<Mutex<Log>> = OnceLock::new();

/// Datei festlegen (ohne Aufruf: nur stderr)
pub fn init(path: PathBuf) {
    let file = open(&path);
    let written = file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
    let _ = LOG.set(Mutex::new(Log { path, file, written, started: Instant::now() }));
}

fn open(path: &PathBuf) -> Option<File> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// UTC-Zeit als ISO 8601 ohne externe Bibliothek (Algorithmus "civil from days")
pub fn iso_now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// Ereignis schreiben. `fields` muss ein JSON-Objekt sein (sonst wird es unter "data" abgelegt).
pub fn ev(level: &str, event: &str, fields: Value) {
    let mut m = Map::new();
    m.insert("t".into(), Value::from(iso_now()));
    m.insert("lvl".into(), Value::from(level));
    m.insert("ev".into(), Value::from(event));
    match fields {
        Value::Object(o) => m.extend(o),
        Value::Null => {}
        other => {
            m.insert("data".into(), other);
        }
    }
    let line = Value::Object(m).to_string();
    if level != "debug" {
        eprintln!("meshd: {event} {line}");
    }
    let Some(lock) = LOG.get() else { return };
    let mut lg = lock.lock().unwrap();
    write_line(&mut lg, &line);
}

fn write_line(lg: &mut Log, line: &str) {
    if lg.written > MAX_BYTES {
        lg.file = None;
        let p1 = lg.path.with_extension("log.1");
        let p2 = lg.path.with_extension("log.2");
        let _ = std::fs::rename(&p1, &p2);
        let _ = std::fs::rename(&lg.path, &p1);
        lg.file = open(&lg.path);
        lg.written = 0;
    }
    let up = lg.started.elapsed().as_secs();
    if let Some(f) = lg.file.as_mut() {
        // "up" (Sekunden seit Start) haengt am Ende, damit Zeilen auch ohne gestellte Uhr ordnen
        let full = format!("{},\"up\":{}}}\n", &line[..line.len() - 1], up);
        if f.write_all(full.as_bytes()).is_ok() {
            lg.written += full.len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_format() {
        let s = iso_now();
        assert_eq!(s.len(), 24);
        assert!(s.starts_with("20") && s.ends_with('Z') && &s[10..11] == "T");
    }

    #[test]
    fn writes_json_lines_and_rotates() {
        let dir = std::env::temp_dir().join(format!("dlog-test-{}", std::process::id()));
        let p = dir.join("debug.log");
        let mut lg = Log { path: p.clone(), file: open(&p), written: MAX_BYTES + 1, started: Instant::now() };
        std::fs::write(&p, "alt\n").unwrap();
        write_line(&mut lg, r#"{"ev":"x"}"#);
        let now = std::fs::read_to_string(&p).unwrap();
        let v: Value = serde_json::from_str(now.trim()).unwrap();
        assert_eq!(v["ev"], "x");
        assert!(v["up"].is_u64());
        assert_eq!(std::fs::read_to_string(p.with_extension("log.1")).unwrap(), "alt\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
