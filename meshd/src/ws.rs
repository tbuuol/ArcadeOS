//! WebSocket-Transport fuer Mesh-Verbindungen (Knoten hinter nicht oeffnenbaren Routern, die per
//! Tunnel unter wss://…/mesh erreichbar sind). Die Arcades behandeln WebSocket-Nachrichten als
//! Bytestrom: jede Nachricht traegt die naechsten Bytes der normalen Mesh-Rahmen (inkl. 4-Byte-Laenge).
//! Nur was das Mesh braucht (RFC 6455): binaere Nachrichten, Maskierung als Client, Ping/Pong, Close.

use std::sync::Arc;

use base64::Engine;
use rand::RngCore;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

pub const MAX_MESSAGE: usize = 128 * 1024;
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Beliebiger Bytestrom (TCP oder TLS ueber TCP)
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
type Boxed = Box<dyn Stream>;

pub struct WsUrl {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub tls: bool,
}

impl WsUrl {
    /// ws://host[:port][/pfad] oder wss://… (Pfad Standard /mesh); nur Hostnamen aus [A-Za-z0-9.-]
    pub fn parse(url: &str) -> Option<WsUrl> {
        let (tls, rest) = if let Some(r) = url.strip_prefix("wss://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("ws://") {
            (false, r)
        } else {
            return None;
        };
        let (hostport, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/mesh"),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().ok()?),
            None => (hostport, if tls { 443 } else { 80 }),
        };
        let ok_host = !host.is_empty() && host.len() <= 253 && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        let ok_path = path.len() <= 65 && path.chars().all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c));
        if !ok_host || !ok_path || port == 0 {
            return None;
        }
        Some(WsUrl { host: host.to_string(), port, path: path.to_string(), tls })
    }
}

fn accept_key(key: &str) -> String {
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

async fn tls_connect(tcp: TcpStream, host: &str) -> std::io::Result<Boxed> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| std::io::Error::other("ungueltiger Hostname"))?;
    let s = tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(name, tcp).await?;
    Ok(Box::new(s))
}

/// Verbindet sich, macht das HTTP-Upgrade und liefert Lese- und Schreibseite als Bytestrom.
pub async fn connect(url: &WsUrl) -> std::io::Result<(WsReader, WsWriter)> {
    let tcp = TcpStream::connect((url.host.as_str(), url.port)).await?;
    tcp.set_nodelay(true)?;
    let mut s: Boxed = if url.tls { tls_connect(tcp, &url.host).await? } else { Box::new(tcp) };
    let mut raw = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let key = base64::engine::general_purpose::STANDARD.encode(raw);
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: arcadeos-meshd\r\n\r\n",
        url.path, url.host, key
    );
    s.write_all(req.as_bytes()).await?;
    // Antwortkopf byteweise bis \r\n\r\n lesen (danach beginnen bereits WebSocket-Rahmen)
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 {
            return Err(std::io::Error::other("Antwortkopf zu lang"));
        }
        let mut b = [0u8; 1];
        s.read_exact(&mut b).await?;
        head.push(b[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let status = lines.next().unwrap_or("");
    if !status.starts_with("HTTP/1.1 101") {
        return Err(std::io::Error::other(format!("kein WebSocket: {}", status.chars().take(60).collect::<String>())));
    }
    let accept = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("sec-websocket-accept"))
        .map(|(_, v)| v.trim().to_string());
    if accept.as_deref() != Some(accept_key(&key).as_str()) {
        return Err(std::io::Error::other("WebSocket-Antwort passt nicht"));
    }
    let (r, w) = tokio::io::split(s);
    let w = Arc::new(Mutex::new(w));
    Ok((WsReader { r, w: w.clone(), buf: Vec::new(), pos: 0 }, WsWriter { w }))
}

async fn send_message(w: &mut WriteHalf<Boxed>, opcode: u8, data: &[u8]) -> std::io::Result<()> {
    let n = data.len();
    let mut out = Vec::with_capacity(n + 14);
    out.push(0x80 | opcode);
    if n < 126 {
        out.push(0x80 | n as u8);
    } else if n < 1 << 16 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    let mut mask = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut mask);
    out.extend_from_slice(&mask);
    out.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    w.write_all(&out).await?;
    w.flush().await
}

pub struct WsWriter {
    w: Arc<Mutex<WriteHalf<Boxed>>>,
}

impl WsWriter {
    /// Eine binaere Nachricht (bei uns: ein ganzer Mesh-Rahmen)
    pub async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        send_message(&mut *self.w.lock().await, 0x2, data).await
    }
}

pub struct WsReader {
    r: ReadHalf<Boxed>,
    w: Arc<Mutex<WriteHalf<Boxed>>>,
    buf: Vec<u8>,
    pos: usize,
}

impl WsReader {
    /// Naechste ganze Datennachricht; Pings werden unterwegs beantwortet.
    async fn next_message(&mut self) -> std::io::Result<Vec<u8>> {
        let mut parts: Vec<u8> = Vec::new();
        loop {
            let mut h = [0u8; 2];
            self.r.read_exact(&mut h).await?;
            let (fin, opcode, masked) = (h[0] & 0x80 != 0, h[0] & 0x0f, h[1] & 0x80 != 0);
            let mut n = (h[1] & 0x7f) as usize;
            if n == 126 {
                let mut b = [0u8; 2];
                self.r.read_exact(&mut b).await?;
                n = u16::from_be_bytes(b) as usize;
            } else if n == 127 {
                let mut b = [0u8; 8];
                self.r.read_exact(&mut b).await?;
                n = u64::from_be_bytes(b).min(u64::MAX >> 1) as usize;
            }
            if n > MAX_MESSAGE || parts.len() + n > MAX_MESSAGE {
                return Err(std::io::Error::other("WebSocket-Nachricht ueber dem Limit"));
            }
            let mut mask = [0u8; 4];
            if masked {
                self.r.read_exact(&mut mask).await?;
            }
            let mut data = vec![0u8; n];
            self.r.read_exact(&mut data).await?;
            if masked {
                data.iter_mut().enumerate().for_each(|(i, b)| *b ^= mask[i % 4]);
            }
            match opcode {
                0x8 => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "WebSocket geschlossen")),
                0x9 => {
                    send_message(&mut *self.w.lock().await, 0xA, &data).await?;
                    continue;
                }
                0xA => continue,
                _ => {}
            }
            parts.extend_from_slice(&data);
            if fin {
                return Ok(parts);
            }
        }
    }

    /// `read_exact` ueber den Bytestrom der Nachrichten
    pub async fn read_exact(&mut self, out: &mut [u8]) -> std::io::Result<()> {
        let mut filled = 0;
        while filled < out.len() {
            if self.pos == self.buf.len() {
                self.buf = self.next_message().await?;
                self.pos = 0;
                continue;
            }
            let take = (self.buf.len() - self.pos).min(out.len() - filled);
            out[filled..filled + take].copy_from_slice(&self.buf[self.pos..self.pos + take]);
            self.pos += take;
            filled += take;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls() {
        let u = WsUrl::parse("wss://app.dogecoinarcade.com/mesh").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str(), u.tls), ("app.dogecoinarcade.com", 443, "/mesh", true));
        let u = WsUrl::parse("ws://127.0.0.1:9000").unwrap();
        assert_eq!((u.port, u.path.as_str(), u.tls), (9000, "/mesh", false));
        assert!(WsUrl::parse("https://x/mesh").is_none());
        assert!(WsUrl::parse("wss://a b/mesh").is_none());
        assert!(WsUrl::parse("wss://user@host/mesh").is_none());
    }

    #[test]
    fn accept_key_rfc6455_example() {
        // Beispiel aus RFC 6455, Abschnitt 1.3
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
