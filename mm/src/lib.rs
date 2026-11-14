//! Gemeinsame Teile von Merge-Mining-Server (arcadeos-mm) und GPU-Miner (mm-gpu): Hashes, Ziele,
//! Serialisierung, Merkle-Baeume, AuxPoW-Platzierung, Adressen und JSON-RPC an die Knoten.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub type H256 = [u8; 32];

// ---------------------------------------------------------------- Hashes

pub fn sha256d(data: &[u8]) -> H256 {
    let a = Sha256::digest(data);
    Sha256::digest(a).into()
}

/// scrypt(N=1024, r=1, p=1) ueber einen 80-Byte-Kopf: Proof of Work von Litecoin, Dogecoin, Pepecoin.
pub fn scrypt_pow(header: &[u8]) -> H256 {
    let params = scrypt::Params::new(10, 1, 1, 32).expect("scrypt-Parameter");
    let mut out = [0u8; 32];
    scrypt::scrypt(header, header, &params, &mut out).expect("scrypt");
    out
}

/// Hash zweier Merkle-Knoten (interne Bytefolge).
pub fn hash_pair(a: &H256, b: &H256) -> H256 {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(a);
    buf[32..].copy_from_slice(b);
    sha256d(&buf)
}

// ---------------------------------------------------------------- Ziele (256 Bit, little endian wie im Hash)

/// Kompakte Schwierigkeit (nBits) als 256-Bit-Ziel, little endian.
pub fn bits_to_target(bits: u32) -> H256 {
    let mut t = [0u8; 32];
    let exp = (bits >> 24) as usize;
    let mant = bits & 0x007f_ffff;
    if bits & 0x0080_0000 != 0 || mant == 0 {
        return t;
    }
    for i in 0..3 {
        let byte = ((mant >> (8 * i)) & 0xff) as u8;
        // Byte i der Mantisse steht an Position exp-3+i
        let pos = exp as isize - 3 + i as isize;
        if (0..32).contains(&pos) {
            t[pos as usize] = byte;
        }
    }
    t
}

/// hash <= target, beide als little-endian-Zahlen.
pub fn meets(hash: &H256, target: &H256) -> bool {
    for i in (0..32).rev() {
        if hash[i] != target[i] {
            return hash[i] < target[i];
        }
    }
    true
}

/// Share-Ziel fuer eine Stratum-Schwierigkeit (scrypt-Konvention: Schwierigkeit 1 = 0xffff * 2^224).
pub fn diff_to_target(diff: f64) -> H256 {
    let diff = if diff.is_finite() && diff > 0.0 { diff } else { 1.0 };
    let v = 65535.0 * 2f64.powi(224) / diff;
    f64_to_target(v)
}

fn f64_to_target(v: f64) -> H256 {
    let mut t = [0u8; 32];
    if v >= 2f64.powi(256) {
        return [0xff; 32];
    }
    if v < 1.0 {
        return t;
    }
    let e = v.log2().floor() as i32; // 2^e <= v
    let shift = e - 52;
    let m = (v / 2f64.powi(shift)) as u64; // 53 Bit
    // t = m << shift (shift kann negativ sein)
    if shift >= 0 {
        let s = shift as usize;
        for bit in 0..64 {
            if m >> bit & 1 == 1 {
                let p = bit + s;
                if p < 256 {
                    t[p / 8] |= 1 << (p % 8);
                }
            }
        }
    } else {
        let mm = m >> (-shift) as u32;
        t[..8].copy_from_slice(&mm.to_le_bytes());
    }
    t
}

/// Ungefaehre Schwierigkeit eines Hashes (fuer Statistik/Log).
pub fn hash_diff(hash: &H256) -> f64 {
    let mut v = 0f64;
    for i in (0..32).rev() {
        v = v * 256.0 + hash[i] as f64;
    }
    if v <= 0.0 {
        return f64::INFINITY;
    }
    65535.0 * 2f64.powi(224) / v
}

// ---------------------------------------------------------------- Serialisierung

pub fn put_varint(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

/// Push von Daten in ein Script (Daten bis 75 Byte direkt, sonst OP_PUSHDATA1).
pub fn push_data(out: &mut Vec<u8>, data: &[u8]) {
    if data.len() <= 75 {
        out.push(data.len() as u8);
    } else {
        out.push(0x4c);
        out.push(data.len() as u8);
    }
    out.extend_from_slice(data);
}

/// Hoehe als CScriptNum-Push (BIP 34).
pub fn push_height(out: &mut Vec<u8>, height: u64) {
    let mut v = Vec::new();
    let mut h = height;
    while h > 0 {
        v.push((h & 0xff) as u8);
        h >>= 8;
    }
    if v.last().map_or(false, |b| b & 0x80 != 0) {
        v.push(0);
    }
    push_data(out, &v);
}

/// Hex in Anzeige-Reihenfolge (RPC) -> interne Bytefolge.
pub fn from_display(hex_str: &str) -> Result<H256, String> {
    let v = hex::decode(hex_str).map_err(|e| format!("hex {hex_str}: {e}"))?;
    if v.len() != 32 {
        return Err(format!("kein 32-Byte-Hash: {hex_str}"));
    }
    let mut h = [0u8; 32];
    for i in 0..32 {
        h[i] = v[31 - i];
    }
    Ok(h)
}

pub fn to_display(h: &H256) -> String {
    let mut r = *h;
    r.reverse();
    hex::encode(r)
}

// ---------------------------------------------------------------- Merkle

/// Merkle-Zweig fuer Blatt 0 (die Coinbase); leaves[0] ist ein Platzhalter.
pub fn merkle_branch_first(leaves: &[H256]) -> Vec<H256> {
    merkle_branch(leaves, 0)
}

/// Merkle-Zweig fuer Blatt `index` (Bitcoin-Art: ungerade Ebenen verdoppeln das letzte Element).
pub fn merkle_branch(leaves: &[H256], mut index: usize) -> Vec<H256> {
    let mut branch = Vec::new();
    let mut level: Vec<H256> = leaves.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        branch.push(level[index ^ 1]);
        level = level.chunks(2).map(|p| hash_pair(&p[0], &p[1])).collect();
        index >>= 1;
    }
    branch
}

pub fn merkle_root(leaves: &[H256]) -> H256 {
    let mut level: Vec<H256> = leaves.to_vec();
    if level.is_empty() {
        return [0; 32];
    }
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        level = level.chunks(2).map(|p| hash_pair(&p[0], &p[1])).collect();
    }
    level[0]
}

/// Wurzel aus Blatt, Zweig und Index (CheckMerkleBranch).
pub fn fold_branch(leaf: &H256, branch: &[H256], mut index: u32) -> H256 {
    let mut h = *leaf;
    for b in branch {
        h = if index & 1 == 1 { hash_pair(b, &h) } else { hash_pair(&h, b) };
        index >>= 1;
    }
    h
}

/// Platz einer Aux-Chain im Merge-Mining-Baum (getExpectedIndex aus auxpow.cpp).
pub fn aux_slot(nonce: u32, chain_id: u32, height: u32) -> u32 {
    let mut rand = nonce;
    rand = rand.wrapping_mul(1103515245).wrapping_add(12345);
    rand = rand.wrapping_add(chain_id);
    rand = rand.wrapping_mul(1103515245).wrapping_add(12345);
    rand % (1u32 << height)
}

// ---------------------------------------------------------------- Stratum-Arbeit (auf beiden Seiten gleich)

/// Kopf aus Stratum-Arbeit: version/nbits/ntime/nonce wie im Header (little endian), prevhash intern.
pub fn build_header(version: u32, prev: &H256, root: &H256, ntime: u32, bits: u32, nonce: u32) -> [u8; 80] {
    let mut h = [0u8; 80];
    h[0..4].copy_from_slice(&version.to_le_bytes());
    h[4..36].copy_from_slice(prev);
    h[36..68].copy_from_slice(root);
    h[68..72].copy_from_slice(&ntime.to_le_bytes());
    h[72..76].copy_from_slice(&bits.to_le_bytes());
    h[76..80].copy_from_slice(&nonce.to_le_bytes());
    h
}

/// prevhash im Stratum-Format (jedes 4-Byte-Wort der internen Folge umgedreht) und zurueck.
pub fn stratum_prevhash(prev: &H256) -> String {
    let mut v = [0u8; 32];
    for w in 0..8 {
        for b in 0..4 {
            v[w * 4 + b] = prev[w * 4 + 3 - b];
        }
    }
    hex::encode(v)
}

pub fn prevhash_from_stratum(s: &str) -> Result<H256, String> {
    let v = hex::decode(s).map_err(|e| e.to_string())?;
    if v.len() != 32 {
        return Err("prevhash".into());
    }
    let mut p = [0u8; 32];
    for w in 0..8 {
        for b in 0..4 {
            p[w * 4 + b] = v[w * 4 + 3 - b];
        }
    }
    Ok(p)
}

pub fn coinbase_root(coinb1: &[u8], en1: &[u8], en2: &[u8], coinb2: &[u8], branch: &[H256]) -> (Vec<u8>, H256) {
    let mut cb = Vec::with_capacity(coinb1.len() + en1.len() + en2.len() + coinb2.len());
    cb.extend_from_slice(coinb1);
    cb.extend_from_slice(en1);
    cb.extend_from_slice(en2);
    cb.extend_from_slice(coinb2);
    let txid = sha256d(&cb);
    let root = fold_branch(&txid, branch, 0);
    (cb, root)
}

// ---------------------------------------------------------------- Adressen (Litecoin)

/// Litecoin-Adresse -> scriptPubKey (Mainnet, Testnet, Regtest; Base58 und Bech32/Bech32m).
pub fn ltc_address_script(addr: &str) -> Result<Vec<u8>, String> {
    let lower = addr.to_ascii_lowercase();
    if lower.starts_with("ltc1") || lower.starts_with("tltc1") || lower.starts_with("rltc1") {
        let (_hrp, data, variant) = bech32::decode(addr).map_err(|e| format!("Adresse {addr}: {e}"))?;
        if data.is_empty() {
            return Err(format!("Adresse {addr}: leer"));
        }
        let ver = data[0].to_u8();
        let prog: Vec<u8> = bech32::FromBase32::from_base32(&data[1..]).map_err(|e| format!("Adresse {addr}: {e:?}"))?;
        let ok = match ver {
            0 => variant == bech32::Variant::Bech32 && (prog.len() == 20 || prog.len() == 32),
            1..=16 => variant == bech32::Variant::Bech32m && (2..=40).contains(&prog.len()),
            _ => false,
        };
        if !ok {
            return Err(format!("Adresse {addr}: kein gueltiges Witness-Programm"));
        }
        let mut s = vec![if ver == 0 { 0 } else { 0x50 + ver }];
        push_data(&mut s, &prog);
        return Ok(s);
    }
    let v = bs58::decode(addr).with_check(None).into_vec().map_err(|e| format!("Adresse {addr}: {e}"))?;
    if v.len() != 21 {
        return Err(format!("Adresse {addr}: falsche Laenge"));
    }
    let h = &v[1..];
    match v[0] {
        0x30 | 0x6f => {
            // P2PKH (L.../m,n...)
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(h);
            s.extend_from_slice(&[0x88, 0xac]);
            Ok(s)
        }
        0x32 | 0x05 | 0x3a | 0xc4 => {
            // P2SH (M.../3.../Q.../2...)
            let mut s = vec![0xa9, 0x14];
            s.extend_from_slice(h);
            s.push(0x87);
            Ok(s)
        }
        b => Err(format!("Adresse {addr}: unbekannte Version {b:#x}")),
    }
}

// ---------------------------------------------------------------- JSON-RPC an die Knoten

#[derive(Clone, Debug)]
pub enum Auth {
    /// user:passwort (rpcauth in der Knoten-Konfiguration)
    Pass(String),
    /// Cookie-Datei des Knotens (wird bei jedem Aufruf gelesen: der Knoten erneuert sie beim Start)
    Cookie(String),
}

#[derive(Clone, Debug)]
pub struct Rpc {
    pub addr: String,
    pub auth: Auth,
}

impl Rpc {
    pub fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        use base64::Engine;
        let cred = match &self.auth {
            Auth::Pass(p) => p.clone(),
            Auth::Cookie(f) => std::fs::read_to_string(f).map_err(|e| format!("Cookie {f}: {e}"))?.trim().to_string(),
        };
        let body = serde_json::json!({"jsonrpc": "1.0", "id": 1, "method": method, "params": params}).to_string();
        let mut s = TcpStream::connect(&self.addr).map_err(|e| format!("{} {method}: {e}", self.addr))?;
        s.set_read_timeout(Some(Duration::from_secs(60))).ok();
        s.set_nodelay(true).ok();
        let req = format!(
            "POST / HTTP/1.0\r\nHost: {}\r\nAuthorization: Basic {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            self.addr,
            base64::engine::general_purpose::STANDARD.encode(cred),
            body.len(),
            body
        );
        s.write_all(req.as_bytes()).map_err(|e| format!("{} {method}: {e}", self.addr))?;
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).map_err(|e| format!("{} {method}: {e}", self.addr))?;
        let split = resp.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| format!("{method}: keine HTTP-Antwort"))?;
        let status = String::from_utf8_lossy(&resp[..split]);
        if status.starts_with("HTTP/1.0 401") || status.starts_with("HTTP/1.1 401") {
            return Err(format!("{} {method}: RPC-Zugang abgelehnt", self.addr));
        }
        let v: serde_json::Value = serde_json::from_slice(&resp[split + 4..]).map_err(|e| format!("{method}: {e}"))?;
        if !v["error"].is_null() {
            return Err(format!("{method}: {}", v["error"]));
        }
        Ok(v["result"].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits() {
        // Bitcoin-Genesis 0x1d00ffff -> 0x00000000ffff0000...
        let t = bits_to_target(0x1d00ffff);
        assert_eq!(t[26], 0xff);
        assert_eq!(t[27], 0xff);
        assert!(t[28..].iter().all(|b| *b == 0));
    }

    #[test]
    fn diff1() {
        let t = diff_to_target(1.0);
        assert_eq!(&t[28..], &[0xff, 0xff, 0, 0]);
    }
}
