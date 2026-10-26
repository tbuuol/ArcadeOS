//! Link-Schicht des Arcade-Mesh-Protokolls v2: Rahmen, HELLO, Handshake, Verschluesselung.
//! Spezifikation: docs/mesh-protocol.md

use std::fmt;
use std::time::Duration;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use x25519_dalek::{EphemeralSecret, PublicKey};

pub const MAGIC: &[u8; 4] = b"ARCM";
pub const VERSION: u8 = 2;
pub const MAX_FRAME: usize = 64 * 1024;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const AUTH_CONTEXT: &[u8] = b"arcade-mesh-auth-v1";
/// Ein JSON-Kopf von 65535 Byte passt nie in einen Rahmen: Kennung fuer binaere Raum-Rahmen.
pub const BIN_PREFIX: [u8; 2] = [0xff, 0xff];

#[derive(Debug)]
pub enum LinkError {
    Io(std::io::Error),
    Timeout,
    Protocol(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Io(e) => write!(f, "io: {e}"),
            LinkError::Timeout => write!(f, "Zeitlimit"),
            LinkError::Protocol(s) => write!(f, "{s}"),
        }
    }
}

impl From<std::io::Error> for LinkError {
    fn from(e: std::io::Error) -> Self {
        LinkError::Io(e)
    }
}

fn proto<T>(msg: impl Into<String>) -> Result<T, LinkError> {
    Err(LinkError::Protocol(msg.into()))
}

/// HELLO: "ARCM" | Version | Laenge Netzname | Netzname | X25519-Public-Key (32)
pub struct Hello {
    pub network: String,
    pub ephemeral: [u8; 32],
}

impl Hello {
    pub fn encode(&self) -> Vec<u8> {
        let net = self.network.as_bytes();
        let mut v = Vec::with_capacity(6 + net.len() + 32);
        v.extend_from_slice(MAGIC);
        v.push(VERSION);
        v.push(net.len() as u8);
        v.extend_from_slice(net);
        v.extend_from_slice(&self.ephemeral);
        v
    }

    pub fn decode(raw: &[u8]) -> Result<Hello, LinkError> {
        if raw.len() < 6 || &raw[..4] != MAGIC {
            return proto("kein Arcade-Mesh-Knoten (HELLO fehlt)");
        }
        if raw[4] != VERSION {
            return proto(format!("Mesh-Version {}, dieser Knoten spricht {VERSION}", raw[4]));
        }
        let n = raw[5] as usize;
        if raw.len() != 6 + n + 32 {
            return proto("HELLO falsch aufgebaut");
        }
        let mut eph = [0u8; 32];
        eph.copy_from_slice(&raw[6 + n..]);
        Ok(Hello { network: String::from_utf8_lossy(&raw[6..6 + n]).into_owned(), ephemeral: eph })
    }
}

pub async fn read_frame(r: &mut OwnedReadHalf) -> Result<Vec<u8>, LinkError> {
    let mut head = [0u8; 4];
    r.read_exact(&mut head).await?;
    let size = u32::from_be_bytes(head) as usize;
    if size > MAX_FRAME {
        return proto(format!("Rahmen mit {size} Byte ueber dem Limit"));
    }
    let mut body = vec![0u8; size];
    r.read_exact(&mut body).await?;
    Ok(body)
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + body.len());
    v.extend_from_slice(&(body.len() as u32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn blake2b32(data: &[u8], personal: Option<&[u8]>) -> [u8; 32] {
    let mut p = blake2b_simd::Params::new();
    p.hash_length(32);
    let mut label = [0u8; 16];
    if let Some(l) = personal {
        let n = l.len().min(16);
        label[..n].copy_from_slice(&l[..n]);
        p.personal(&label);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(p.hash(data).as_bytes());
    out
}

fn nonce(n: u64) -> XNonce {
    let mut b = [0u8; 24];
    b[16..].copy_from_slice(&n.to_be_bytes());
    *XNonce::from_slice(&b)
}

/// Senderichtung einer bestehenden Verbindung (Zaehler = Nonce)
pub struct Sender {
    w: OwnedWriteHalf,
    cipher: XChaCha20Poly1305,
    n: u64,
}

/// Empfangsrichtung
pub struct Receiver {
    r: OwnedReadHalf,
    cipher: XChaCha20Poly1305,
    n: u64,
}

/// Inhalt eines Rahmens: JSON-Kopf + Anhang, oder ein binaerer Raum-Rahmen (roh)
pub enum Body {
    Json(Value, Vec<u8>),
    BinaryPub(Vec<u8>),
}

impl Sender {
    pub async fn send_raw(&mut self, plain: &[u8]) -> Result<(), LinkError> {
        let sealed = self
            .cipher
            .encrypt(&nonce(self.n), plain)
            .map_err(|_| LinkError::Protocol("Verschluesselung fehlgeschlagen".into()))?;
        self.n += 1;
        self.w.write_all(&frame(&sealed)).await?;
        Ok(())
    }

    /// JSON-Rahmen: 2 Byte Kopflaenge | kompaktes JSON | Anhang
    pub async fn send(&mut self, header: &Value, blob: &[u8]) -> Result<(), LinkError> {
        let head = serde_json::to_vec(header).expect("JSON");
        if head.len() >= 0xffff {
            return proto("Kopf zu gross");
        }
        let mut plain = Vec::with_capacity(2 + head.len() + blob.len());
        plain.extend_from_slice(&(head.len() as u16).to_be_bytes());
        plain.extend_from_slice(&head);
        plain.extend_from_slice(blob);
        self.send_raw(&plain).await
    }
}

impl Receiver {
    pub async fn recv_raw(&mut self) -> Result<Vec<u8>, LinkError> {
        let sealed = read_frame(&mut self.r).await?;
        let plain = self
            .cipher
            .decrypt(&nonce(self.n), sealed.as_slice())
            .map_err(|_| LinkError::Protocol("Rahmen nicht authentisch".into()))?;
        self.n += 1;
        Ok(plain)
    }

    pub async fn recv(&mut self) -> Result<Body, LinkError> {
        let plain = self.recv_raw().await?;
        if plain.len() < 2 {
            return proto("leerer Rahmen");
        }
        if plain[..2] == BIN_PREFIX {
            return Ok(Body::BinaryPub(plain));
        }
        let n = u16::from_be_bytes([plain[0], plain[1]]) as usize;
        if plain.len() < 2 + n {
            return proto("Rahmen kuerzer als sein Kopf");
        }
        let header: Value =
            serde_json::from_slice(&plain[2..2 + n]).map_err(|_| LinkError::Protocol("Kopf ist kein JSON".into()))?;
        if !header.is_object() {
            return proto("Kopf ist kein Objekt");
        }
        Ok(Body::Json(header, plain[2 + n..].to_vec()))
    }
}

/// Ergebnis eines erfolgreichen Handshakes
pub struct Link {
    pub peer_key: [u8; 32],
    pub peer_listen: Option<u16>,
    pub tx: Sender,
    pub rx: Receiver,
}

impl Link {
    pub fn peer_id(&self) -> String {
        hex::encode(self.peer_key)
    }
}

/// Handshake auf einer frischen TCP-Verbindung. `dialer`: wer die Rolle des Waehlenden hat
/// (bei gleichzeitigem Oeffnen: die kleinere Knoten-ID).
pub async fn handshake(
    stream: TcpStream,
    key: &SigningKey,
    network: &str,
    dialer: bool,
    listen_port: Option<u16>,
) -> Result<Link, LinkError> {
    stream.set_nodelay(true)?;
    let (mut r, mut w) = stream.into_split();
    let secret = EphemeralSecret::random_from_rng(OsRng);
    let mine = Hello { network: network.to_string(), ephemeral: PublicKey::from(&secret).to_bytes() };
    w.write_all(&frame(&mine.encode())).await?;
    let raw = timeout(HANDSHAKE_TIMEOUT, read_frame(&mut r)).await.map_err(|_| LinkError::Timeout)??;
    let theirs = Hello::decode(&raw)?;
    if theirs.network != network {
        return proto(format!("Knoten ist in Netz {:?}, dieser in {network:?}", theirs.network));
    }
    let shared = secret.diffie_hellman(&PublicKey::from(theirs.ephemeral));
    let (first, second) = if dialer { (&mine, &theirs) } else { (&theirs, &mine) };
    let mut t = first.encode();
    t.extend_from_slice(&second.encode());
    let transcript = blake2b32(&t, None);
    let mut material = shared.as_bytes().to_vec();
    material.extend_from_slice(&transcript);
    let k_out = blake2b32(&material, Some(b"arcm-dialer"));
    let k_in = blake2b32(&material, Some(b"arcm-listener"));
    let (send_key, recv_key) = if dialer { (k_out, k_in) } else { (k_in, k_out) };

    let mut tx = Sender { w, cipher: XChaCha20Poly1305::new((&send_key).into()), n: 0 };
    let mut rx = Receiver { r, cipher: XChaCha20Poly1305::new((&recv_key).into()), n: 0 };

    let role: &[u8] = if dialer { b"D" } else { b"L" };
    let mut msg = AUTH_CONTEXT.to_vec();
    msg.extend_from_slice(role);
    msg.extend_from_slice(&transcript);
    let proof = key.sign(&msg);
    tx.send(
        &json!({"t": "auth", "key": hex::encode(key.verifying_key().to_bytes()),
                "sig": hex::encode(proof.to_bytes()), "listen": listen_port}),
        b"",
    )
    .await?;

    let header = match timeout(HANDSHAKE_TIMEOUT, rx.recv()).await.map_err(|_| LinkError::Timeout)?? {
        Body::Json(h, _) => h,
        Body::BinaryPub(_) => return proto("auth erwartet"),
    };
    if header.get("t").and_then(Value::as_str) != Some("auth") {
        return proto("auth erwartet");
    }
    let peer_key: [u8; 32] = header
        .get("key")
        .and_then(Value::as_str)
        .and_then(|s| hex::decode(s).ok())
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| LinkError::Protocol("auth ohne gueltigen Key".into()))?;
    let sig_bytes: [u8; 64] = header
        .get("sig")
        .and_then(Value::as_str)
        .and_then(|s| hex::decode(s).ok())
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| LinkError::Protocol("auth ohne gueltige Signatur".into()))?;
    let their_role: &[u8] = if dialer { b"L" } else { b"D" };
    let mut their_msg = AUTH_CONTEXT.to_vec();
    their_msg.extend_from_slice(their_role);
    their_msg.extend_from_slice(&transcript);
    let vk = VerifyingKey::from_bytes(&peer_key).map_err(|_| LinkError::Protocol("ungueltiger Key".into()))?;
    vk.verify_strict(&their_msg, &Signature::from_bytes(&sig_bytes))
        .map_err(|_| LinkError::Protocol("Gegenseite konnte ihren Key nicht beweisen".into()))?;
    if peer_key == key.verifying_key().to_bytes() {
        return proto("mit sich selbst verbunden");
    }
    let peer_listen = header
        .get("listen")
        .and_then(Value::as_u64)
        .filter(|p| *p > 0 && *p < 65536)
        .map(|p| p as u16);
    Ok(Link { peer_key, peer_listen, tx, rx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let h = Hello { network: "arcade-main".into(), ephemeral: [7u8; 32] };
        let raw = h.encode();
        assert_eq!(&raw[..4], b"ARCM");
        assert_eq!(raw[4], 2);
        assert_eq!(raw.len(), 6 + 11 + 32);
        let d = Hello::decode(&raw).unwrap();
        assert_eq!(d.network, "arcade-main");
        assert_eq!(d.ephemeral, [7u8; 32]);
        assert!(Hello::decode(b"XXXX").is_err());
    }

    #[test]
    fn nonce_is_big_endian_24_bytes() {
        let n = nonce(0x0102);
        assert_eq!(n.len(), 24);
        assert_eq!(n[22], 1);
        assert_eq!(n[23], 2);
        assert!(n[..22].iter().all(|b| *b == 0));
    }

    #[test]
    fn blake2b_personal_matches_python() {
        // Python: hashlib.blake2b(b"abc", digest_size=32, person=b"arcm-dialer".ljust(16, b"\0")).hexdigest()
        let h = blake2b32(b"abc", Some(b"arcm-dialer"));
        assert_eq!(hex::encode(h), "bc16380537676f2bf24443a30ba8e879c1e5f2755a48cf52c1e1f10c4d465d41");
    }

    #[test]
    fn handshake_between_two_meshd() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let lst = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = lst.local_addr().unwrap();
            let ka = SigningKey::generate(&mut OsRng);
            let kb = SigningKey::generate(&mut OsRng);
            let kb2 = kb.clone();
            let server = tokio::spawn(async move {
                let (s, _) = lst.accept().await.unwrap();
                handshake(s, &kb2, "arcade-main", false, None).await
            });
            let c = TcpStream::connect(addr).await.unwrap();
            let mut a = handshake(c, &ka, "arcade-main", true, Some(8421)).await.unwrap();
            let mut b = server.await.unwrap().unwrap();
            assert_eq!(a.peer_key, kb.verifying_key().to_bytes());
            assert_eq!(b.peer_listen, Some(8421));
            a.tx.send(&json!({"t": "ping", "n": "abcd"}), b"").await.unwrap();
            match b.rx.recv().await.unwrap() {
                Body::Json(h, _) => assert_eq!(h["n"], "abcd"),
                _ => panic!("JSON erwartet"),
            }
        });
    }
}
