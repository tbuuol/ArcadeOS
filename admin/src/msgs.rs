//! Messenger (arcm, Pepecoin-Testnet): sammelt ab ACTIVATION jede arcm-Nutzlast aus Class C (OP_RETURN "arcd") und
//! Class B (Marker-Output + 1-aus-n-Multisig, mit der Absender-Adresse verschleiert) und fuehrt das Verzeichnis
//! der angekuendigten X25519-Schluessel. Entschluesselt wird NUR im Browser (Probe-Entschluesselung): die Box sieht
//! keine Inhalte, keine Empfaenger und keine geheimen Schluessel.
//!
//! Format wie dogecoin-arcade (envelope.py, encoding.py), damit Nachrichten zwischen den Arcades gehen:
//!   Nutzlast = AnyData (00 00 00 c8) || "arcm" 01 <Typ> ...
//!   Typ 1: clen(2, BE) || crypto_box_seal(Empfaenger, Absender-pk || nonce || crypto_box("arcm" 01 01 || Text))
//!   Typ 3: X25519-pk(32) [ 01 hash160(20) len name ] [02 hash160] [03 len tag] ...
use super::utxo::{batch, internal, read_block, shown, Tx, Txid};
use super::*;

const MARKER_SEED: &[u8] = b"DogecoinArcade Class B marker v1"; // config.py MARKER_SEED
const ANYDATA_HEAD: [u8; 4] = [0, 0, 0, 0xc8];
const ARCM: &[u8] = b"arcm";
const PACKET: usize = 31; // 1 Folgenummer + 30 Daten
const MAX_PACKETS: usize = 255;
const BATCH: u32 = 200;

pub struct Item {
    pub h: u32,
    pub txid: String,
    pub from: String,
    pub p: Vec<u8>, // "arcm" ... (Class-B-Polster haengt noch dran)
}

#[derive(Default)]
pub struct Msgs {
    height: u32,
    hash: Txid,
    pub items: Vec<Item>,
    /// Adresse -> (pubkey, Hoehe, txid, Name, selbst angekuendigt?)
    pub keys: HashMap<String, (String, u32, String, String, bool)>,
}

fn marker_script() -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend_from_slice(&hash160(MARKER_SEED));
    s.extend_from_slice(&[0x88, 0xac]);
    s
}

/// Letzter Push eines scriptSig (bei P2PKH der Pubkey, bei P2SH das Redeem-Script)
fn last_push(sig: &[u8]) -> Option<&[u8]> {
    let (mut p, mut last) = (0usize, None);
    while p < sig.len() {
        let op = sig[p];
        p += 1;
        let n = match op {
            1..=75 => op as usize,
            0x4c => { let n = *sig.get(p)? as usize; p += 1; n }
            0x4d => { let n = u16::from_le_bytes(sig.get(p..p + 2)?.try_into().ok()?) as usize; p += 2; n }
            0 | 0x4f | 0x51..=0x60 => { last = Some(&sig[p..p]); continue; }
            _ => return None,
        };
        last = Some(sig.get(p..p + n)?);
        p += n;
    }
    last
}

/// Absender wie die Arcades: Adresse des ersten Inputs
fn sender(tx: &Tx) -> Option<String> {
    let i = tx.ins.first()?;
    if i.prev.0 == [0; 32] {
        return None;
    }
    let push = last_push(i.sig)?;
    Some(if push.len() == 33 || push.len() == 65 { b58check(PKH, &hash160(push)) } else { b58check(P2SH, &hash160(push)) })
}

/// Pubkeys eines Multisig-Outputs (OP_m <pk>... OP_n OP_CHECKMULTISIG)
fn multisig_keys(spk: &[u8]) -> Option<Vec<&[u8]>> {
    if spk.len() < 37 || !(0x51..=0x53).contains(&spk[0]) || *spk.last()? != 0xae {
        return None;
    }
    let (mut p, mut keys) = (1usize, Vec::new());
    while p < spk.len() - 2 {
        let n = spk[p] as usize;
        if n != 33 && n != 65 {
            return None;
        }
        keys.push(spk.get(p + 1..p + 1 + n)?);
        p += 1 + n;
    }
    Some(keys)
}

/// encoding.py prepare_obfuscated_hashes: SHA256(Adresse), dann SHA256(HEX GROSS des vorigen)
fn obfuscation(seed: &str, count: usize) -> Vec<[u8; 32]> {
    let mut out = Vec::with_capacity(count);
    let mut input = seed.as_bytes().to_vec();
    for _ in 0..count {
        let d = sha256(&input);
        out.push(d);
        input = hex::encode_upper(d).into_bytes();
    }
    out
}

fn decode_class_b(from: &str, outs: &[(u64, &[u8])]) -> Option<Vec<u8>> {
    let mut packets: Vec<&[u8]> = Vec::new();
    for (_, spk) in outs {
        if let Some(keys) = multisig_keys(spk) {
            packets.extend(keys.into_iter().skip(1));
        }
    }
    if packets.is_empty() || packets.len() > MAX_PACKETS {
        return None;
    }
    let hashes = obfuscation(from, packets.len());
    let mut data = Vec::with_capacity(packets.len() * 30);
    for (i, k) in packets.iter().enumerate() {
        let x: Vec<u8> = k.get(1..1 + PACKET)?.iter().zip(&hashes[i][..PACKET]).map(|(a, b)| a ^ b).collect();
        if x[0] as usize != i + 1 {
            return None;
        }
        data.extend_from_slice(&x[1..]);
    }
    Some(data)
}

/// arcm-Nutzlast einer Transaktion (Class C oder B), mit Absender
fn extract(tx: &Tx, marker: &[u8]) -> Option<(String, Vec<u8>)> {
    let mut payload = None;
    for (_, spk) in &tx.outs {
        if let Some(d) = op_return_data(spk) {
            if let Some(rest) = d.strip_prefix(MARKER) {
                payload = Some(rest.to_vec());
                break;
            }
        }
    }
    let from = sender(tx)?;
    if payload.is_none() && tx.outs.iter().any(|(_, s)| *s == marker) {
        payload = decode_class_b(&from, &tx.outs);
    }
    let data = payload?.strip_prefix(&ANYDATA_HEAD)?.to_vec();
    data.starts_with(ARCM).then_some((from, data))
}

/// Schluessel-Ankuendigung: (pubkey, behauptete Adresse, Name)
fn announcement(p: &[u8]) -> Option<(String, Option<String>, String)> {
    let key = p.get(6..38)?;
    if p.get(38) != Some(&1) {
        return Some((hex::encode(key), None, String::new()));
    }
    let h = p.get(39..59)?;
    let n = *p.get(59)? as usize;
    let name = p.get(60..60 + n).map(|s| String::from_utf8_lossy(s).chars().filter(|c| !c.is_control()).take(64).collect()).unwrap_or_default();
    Some((hex::encode(key), Some(b58check(PKH, h)), name))
}

impl Msgs {
    fn add(&mut self, h: u32, txid: String, from: String, p: Vec<u8>) {
        if p.get(4) == Some(&1) && p.get(5) == Some(&3) {
            if let Some((key, claimed, name)) = announcement(&p) {
                // Selbst angekuendigt (Absender = Adresse) gewinnt immer; nur behauptet gilt, solange nichts Eigenes da ist
                let own = claimed.as_deref().map(|c| c == from).unwrap_or(true);
                let who = claimed.unwrap_or_else(|| from.clone());
                let keep = self.keys.get(&who).map(|k| k.4 && !own).unwrap_or(false);
                if !keep {
                    self.keys.insert(who, (key, h, txid.clone(), name, own));
                }
            }
        }
        self.items.push(Item { h, txid, from, p });
    }

    fn save(&self, path: &PathBuf) {
        let items: Vec<Value> = self.items.iter().map(|i| json!([i.h, i.txid, i.from, hex::encode(&i.p)])).collect();
        let v = json!({"height": self.height, "hash": hex::encode(self.hash), "items": items});
        let tmp = path.with_extension("new");
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    fn load(path: &PathBuf) -> Msgs {
        let mut m = Msgs { height: ACTIVATION as u32 - 1, ..Default::default() };
        let Some(v) = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { return m };
        let (Some(h), Some(hash)) = (v["height"].as_u64(), v["hash"].as_str().and_then(|s| hex::decode(s).ok()).and_then(|b| b.try_into().ok())) else { return m };
        m.height = h as u32;
        m.hash = hash;
        for i in v["items"].as_array().cloned().unwrap_or_default() {
            let p = hex::decode(i[3].as_str().unwrap_or("")).unwrap_or_default();
            m.add(i[0].as_u64().unwrap_or(0) as u32, i[1].as_str().unwrap_or("").into(), i[2].as_str().unwrap_or("").into(), p);
        }
        m
    }

    /// Alles ab Hoehe `h` vergessen (Reorg) und dort neu lesen
    fn rewind(&mut self, cfg: &Cfg, h: u32) -> Result<(), String> {
        let items = std::mem::take(&mut self.items);
        self.keys.clear();
        for i in items.into_iter().filter(|i| i.h < h) {
            self.add(i.h, i.txid, i.from, i.p);
        }
        self.height = h - 1;
        self.hash = internal(rpc(cfg, "getblockhash", json!([self.height]))?.as_str().unwrap_or(""))?;
        Ok(())
    }
}

fn step(app: &App, path: &PathBuf, marker: &[u8]) -> Result<bool, String> {
    let cfg = &app.cfg;
    let tip = rpc(cfg, "getblockcount", json!([]))?.as_u64().ok_or("getblockcount")? as u32;
    let height = {
        let mut m = app.msgs.lock().unwrap();
        if m.hash == [0; 32] {
            let h = m.height;
            m.hash = internal(rpc(cfg, "getblockhash", json!([h]))?.as_str().unwrap_or(""))?;
        }
        if tip < m.height {
            let back = tip.saturating_sub(10).max(ACTIVATION as u32);
            m.rewind(cfg, back)?;
        }
        m.height
    };
    if tip <= height {
        return Ok(false);
    }
    let to = tip.min(height + BATCH);
    let hashes = batch(cfg, "getblockhash", (height + 1..=to).map(|h| json!([h])))?;
    let blocks = batch(cfg, "getblock", hashes.iter().map(|h| json!([h, false])))?;
    let mut m = app.msgs.lock().unwrap();
    if m.height != height {
        return Ok(true);
    }
    let mut found = false;
    for (i, b) in blocks.iter().enumerate() {
        let raw = hex::decode(b.as_str().unwrap_or("")).map_err(|e| e.to_string())?;
        let (hash, prev, txs) = read_block(&raw)?;
        let h = height + 1 + i as u32;
        if prev != m.hash {
            log(&format!("Messenger: Reorg bei Block {h}, lese die letzten Bloecke neu"));
            m.rewind(cfg, h.saturating_sub(20).max(ACTIVATION as u32))?;
            return Ok(true);
        }
        for tx in &txs {
            if let Some((from, p)) = extract(tx, marker) {
                m.add(h, shown(&tx.txid), from, p);
                found = true;
            }
        }
        m.height = h;
        m.hash = hash;
    }
    if found || m.height == tip || m.height % 10_000 < BATCH {
        m.save(path);
    }
    Ok(m.height < tip)
}

pub fn run(app: Arc<App>) {
    let path = app.cfg.state.join("arcm.json");
    let marker = marker_script();
    *app.msgs.lock().unwrap() = Msgs::load(&path);
    let mut logged = false;
    loop {
        match step(&app, &path, &marker) {
            Ok(true) => continue,
            Ok(false) => {
                if !logged {
                    let m = app.msgs.lock().unwrap();
                    log(&format!("Messenger: bis Block {} gelesen, {} Nutzlasten, {} Schluessel", m.height, m.items.len(), m.keys.len()));
                    logged = true;
                }
                std::thread::sleep(Duration::from_secs(5));
            }
            Err(e) => {
                log(&format!("Messenger: {e}"));
                std::thread::sleep(Duration::from_secs(15));
            }
        }
    }
}

// ------------------------------------------------------------------ Senden: Class B / Class C bauen

/// Gueltiger komprimierter Punkt aus 31 verschleierten Bytes (encoding.py _make_fake_pubkey)
fn fake_pubkey(obf: &[u8]) -> Option<Vec<u8>> {
    let mut r = [0u8; 1];
    getrandom::getrandom(&mut r).ok();
    for off in 0..=255u8 {
        let mut k = vec![0x02];
        k.extend_from_slice(obf);
        k.push(r[0].wrapping_add(off));
        if k256::PublicKey::from_sec1_bytes(&k).is_ok() {
            return Some(k);
        }
    }
    None
}

/// Outputs einer Class-B-Nachricht von `from` (Adresse, Pubkey): Marker + Multisig, je 0,01 PEP
pub fn class_b_outputs(from: &str, pubkey: &[u8], arcm: &[u8]) -> Result<Vec<(u64, String)>, String> {
    let mut payload = ANYDATA_HEAD.to_vec();
    payload.extend_from_slice(arcm);
    let n = payload.len().div_ceil(30);
    if n > MAX_PACKETS {
        return Err(format!("Nachricht zu lang ({} Bytes, hoechstens {})", arcm.len(), MAX_PACKETS * 30 - 4));
    }
    let hashes = obfuscation(from, n);
    let mut fakes = Vec::new();
    for i in 0..n {
        let mut plain = vec![(i + 1) as u8];
        plain.extend_from_slice(&payload[i * 30..((i + 1) * 30).min(payload.len())]);
        plain.resize(PACKET, 0);
        let obf: Vec<u8> = plain.iter().zip(&hashes[i][..PACKET]).map(|(a, b)| a ^ b).collect();
        fakes.push(fake_pubkey(&obf).ok_or("kein gueltiger Punkt")?);
    }
    let mut outs = vec![(DUST, hex::encode(marker_script()))];
    for pair in fakes.chunks(2) {
        let mut s = vec![0x51, 33];
        s.extend_from_slice(pubkey);
        for k in pair {
            s.push(33);
            s.extend_from_slice(k);
        }
        s.push(0x51 + pair.len() as u8);
        s.push(0xae);
        outs.push((DUST, hex::encode(s)));
    }
    Ok(outs)
}

/// OP_RETURN einer Schluessel-Ankuendigung: "arcd" || AnyData || "arcm" 01 03 pk [01 hash160 0] [03 len tag]
pub fn announce_output(pubkey: &[u8; 32], addr_h160: &[u8], tag: Option<&str>) -> String {
    let mut d = MARKER.to_vec();
    d.extend_from_slice(&ANYDATA_HEAD);
    d.extend_from_slice(ARCM);
    d.extend_from_slice(&[1, 3]);
    d.extend_from_slice(pubkey);
    d.push(1);
    d.extend_from_slice(addr_h160);
    d.push(0);
    if let Some(t) = tag.filter(|t| d.len() + 2 + t.len() <= 80) {
        d.push(3);
        d.push(t.len() as u8);
        d.extend_from_slice(t.as_bytes());
    }
    format!("6a{:02x}{}", d.len(), hex::encode(&d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_b_roundtrip() {
        let from = "nfqSKD76CiBuoAMZNkUBuiHYSaPxG2KidZ";
        let pk = k256::SecretKey::from_slice(&[7u8; 32]).unwrap().public_key();
        let pkb = k256::elliptic_curve::sec1::ToEncodedPoint::to_encoded_point(&pk, true).as_bytes().to_vec();
        let msg: Vec<u8> = b"arcm\x01\x01\x00\x05hello, this is a message that spans several packets of thirty bytes".to_vec();
        let outs = class_b_outputs(from, &pkb, &msg).unwrap();
        let scripts: Vec<Vec<u8>> = outs.iter().map(|o| hex::decode(&o.1).unwrap()).collect();
        let pairs: Vec<(u64, &[u8])> = scripts.iter().map(|s| (DUST, s.as_slice())).collect();
        let data = decode_class_b(from, &pairs).unwrap();
        assert_eq!(&data[..4], &ANYDATA_HEAD);
        assert_eq!(&data[4..4 + msg.len()], &msg[..]);
        assert!(data[4 + msg.len()..].iter().all(|b| *b == 0));
        assert_eq!(obfuscation(from, 2)[1], sha256(hex::encode_upper(sha256(from.as_bytes())).as_bytes()));
    }
}
