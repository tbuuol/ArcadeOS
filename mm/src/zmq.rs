//! Kleinster ZMQ-SUB-Client (ZMTP 3.0, Mechanismus NULL), nur fuer `hashblock` der Knoten: meldet jeden
//! neuen Block sofort. Faellt die Verbindung, wird nach 5 s neu verbunden; der Server fragt die Knoten
//! ausserdem regelmaessig selbst, ZMQ macht es nur schneller.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::Sender;
use std::time::Duration;

fn read_frame(s: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut f = [0u8; 1];
    s.read_exact(&mut f)?;
    let size = if f[0] & 0x02 != 0 {
        let mut b = [0u8; 8];
        s.read_exact(&mut b)?;
        u64::from_be_bytes(b) as usize
    } else {
        let mut b = [0u8; 1];
        s.read_exact(&mut b)?;
        b[0] as usize
    };
    if size > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "ZMQ-Frame zu gross"));
    }
    let mut body = vec![0u8; size];
    s.read_exact(&mut body)?;
    Ok((f[0], body))
}

fn session(addr: &str, topic: &str, tx: &Sender<String>, name: &str) -> std::io::Result<()> {
    let mut s = TcpStream::connect(addr)?;
    s.set_nodelay(true).ok();
    // Begruessung: Signatur, Version 3.0, Mechanismus NULL, kein Server
    let mut g = vec![0xffu8, 0, 0, 0, 0, 0, 0, 0, 0, 0x7f, 3, 0];
    let mut mech = b"NULL".to_vec();
    mech.resize(20, 0);
    g.extend_from_slice(&mech);
    g.push(0);
    g.resize(64, 0);
    s.write_all(&g)?;
    let mut peer = [0u8; 64];
    s.read_exact(&mut peer)?;
    // READY mit Socket-Type SUB
    let mut ready = vec![5u8];
    ready.extend_from_slice(b"READY");
    ready.push(11);
    ready.extend_from_slice(b"Socket-Type");
    ready.extend_from_slice(&3u32.to_be_bytes());
    ready.extend_from_slice(b"SUB");
    let mut cmd = vec![0x04, ready.len() as u8];
    cmd.extend_from_slice(&ready);
    s.write_all(&cmd)?;
    // Abo (ZMTP 3.0: Nachricht 0x01 + Thema)
    let mut sub = vec![0x00, (topic.len() + 1) as u8, 0x01];
    sub.extend_from_slice(topic.as_bytes());
    s.write_all(&sub)?;
    s.set_read_timeout(None).ok();
    let mut parts: Vec<Vec<u8>> = Vec::new();
    loop {
        let (flags, body) = read_frame(&mut s)?;
        if flags & 0x04 != 0 {
            continue; // Befehl (READY des Knotens u. a.)
        }
        parts.push(body);
        if flags & 0x01 == 0 {
            if parts.first().map_or(false, |t| t == topic.as_bytes()) {
                let _ = tx.send(name.to_string());
            }
            parts.clear();
        }
    }
}

/// Startet einen Faden, der bei jedem neuen Block `name` an `tx` schickt.
pub fn watch(addr: String, name: String, tx: Sender<String>) {
    std::thread::spawn(move || loop {
        if let Err(e) = session(&addr, "hashblock", &tx, &name) {
            eprintln!("zmq {name} {addr}: {e}, neuer Versuch in 5 s");
        }
        std::thread::sleep(Duration::from_secs(5));
    });
}
