//! Eigener UTXO-Index, so schlank wie moeglich: Adresse -> unverbrauchte Coins (nur P2PKH/P2SH), im Speicher,
//! ab Block 1 aus den rohen Bloecken des Knotens gebaut. Damit hat jede Adresse sofort ihr Guthaben – ohne
//! importaddress und ohne Rescan im Knoten-Wallet.
//!
//! Verbrauchte Coins: der letzte Push des scriptSig ist bei P2PKH der Pubkey, bei P2SH das Redeem-Script; sein
//! hash160 ist also die Adresse des ausgegebenen Coins. So braucht der Index keine Tabelle Outpoint -> Adresse.
//! (Nur fuer ein scriptSig mit Nicht-Push-Befehlen fragt er den Knoten nach der Vor-Transaktion.)
//! Reorgs: Rueckgaengig-Daten fuer die letzten Bloecke. Mempool: wird bei jeder Runde nachgezogen.
//! Schnappschuss: <state>/utxo.idx (alle 60 Bloecke); passt er nicht mehr zur Kette, baut er neu.
use super::*;
use std::collections::{HashSet, VecDeque};
use std::io::{BufReader, BufWriter};

type Key = [u8; 21]; // 0 = P2PKH / 1 = P2SH, dann hash160
pub type Txid = [u8; 32]; // interne Reihenfolge (wie im Block)

#[derive(Clone, Copy)]
struct Coin {
    txid: Txid,
    vout: u32,
    value: u64,
    height: u32,
}

struct Undo {
    prev: Txid,
    added: Vec<(Key, Txid, u32)>,
    removed: Vec<(Key, Coin)>,
}

struct MemTx {
    spends: Vec<(Txid, u32)>,
    pays: Vec<(Key, u32, u64)>,
}

#[derive(Default)]
pub struct Utxo {
    height: u32,
    hash: Txid,
    tip: u32,
    saved: u32,
    coins: HashMap<Key, Vec<Coin>>,
    undo: VecDeque<Undo>,
    mempool: HashMap<Txid, MemTx>,
    mem_spent: HashSet<(Txid, u32)>,
}

const KEEP_UNDO: u32 = 100;
const BATCH: u32 = 100;
const MAGIC: &[u8; 8] = b"PEPUTXO1";

// ------------------------------------------------------------------ Lesen roher Daten
pub struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.b.get(self.p..self.p + n).ok_or("Block zu kurz")?;
        self.p += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn var(&mut self) -> Result<usize, String> {
        Ok(match self.take(1)?[0] {
            0xfd => u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as usize,
            0xfe => self.u32()? as usize,
            0xff => self.u64()? as usize,
            n => n as usize,
        })
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.var()?;
        self.take(n)
    }
}

pub struct TxIn<'a> {
    pub prev: (Txid, u32),
    pub sig: &'a [u8],
}

pub struct Tx<'a> {
    pub txid: Txid,
    pub ins: Vec<TxIn<'a>>,
    pub outs: Vec<(u64, &'a [u8])>,
}

fn read_tx<'a>(r: &mut Rd<'a>) -> Result<Tx<'a>, String> {
    let start = r.p;
    r.take(4)?;
    let mut ins = Vec::new();
    for _ in 0..r.var()? {
        let prev: Txid = r.take(32)?.try_into().unwrap();
        let n = r.u32()?;
        let sig = r.bytes()?;
        r.take(4)?;
        ins.push(TxIn { prev: (prev, n), sig });
    }
    let mut outs = Vec::new();
    for _ in 0..r.var()? {
        let v = r.u64()?;
        outs.push((v, r.bytes()?));
    }
    r.take(4)?;
    Ok(Tx { txid: dsha(&r.b[start..r.p]), ins, outs })
}

/// Kopf (80 Byte, bei Merged Mining + AuxPoW) und Transaktionen
pub fn read_block(b: &[u8]) -> Result<(Txid, Txid, Vec<Tx<'_>>), String> {
    let mut r = Rd { b, p: 0 };
    let head = r.take(80)?;
    let prev: Txid = head[4..36].try_into().unwrap();
    if u32::from_le_bytes(head[0..4].try_into().unwrap()) & 0x100 != 0 {
        read_tx(&mut r)?; // Coinbase des Elternblocks
        r.take(32)?;
        for _ in 0..2 {
            let n = r.var()?;
            r.take(n * 32 + 4)?;
        }
        r.take(80)?;
    }
    let mut txs = Vec::new();
    for _ in 0..r.var()? {
        txs.push(read_tx(&mut r)?);
    }
    Ok((dsha(head), prev, txs))
}

fn out_key(spk: &[u8]) -> Option<Key> {
    let mut k = [0u8; 21];
    if spk.len() == 25 && spk[..3] == [0x76, 0xa9, 0x14] && spk[23..] == [0x88, 0xac] {
        k[1..].copy_from_slice(&spk[3..23]);
    } else if spk.len() == 23 && spk[..2] == [0xa9, 0x14] && spk[22] == 0x87 {
        k[0] = 1;
        k[1..].copy_from_slice(&spk[2..22]);
    } else {
        return None;
    }
    Some(k)
}

/// hash160 des letzten Pushs; None, wenn das scriptSig nicht nur aus Pushs besteht
fn last_push(sig: &[u8]) -> Option<[u8; 20]> {
    let mut r = Rd { b: sig, p: 0 };
    let mut last: &[u8] = &[];
    while r.p < sig.len() {
        let op = r.take(1).ok()?[0];
        last = match op {
            1..=75 => r.take(op as usize).ok()?,
            0x4c => { let n = r.take(1).ok()?[0] as usize; r.take(n).ok()? }
            0x4d => { let n = u16::from_le_bytes(r.take(2).ok()?.try_into().ok()?) as usize; r.take(n).ok()? }
            0x4e => { let n = r.u32().ok()? as usize; r.take(n).ok()? }
            0 | 0x4f | 0x51..=0x60 => &[],
            _ => return None,
        };
    }
    Some(hash160(last))
}

pub fn key_of(addr: &str) -> Option<Key> {
    let a = bs58::decode(addr).into_vec().ok()?;
    if a.len() != 25 || dsha(&a[..21])[..4] != a[21..] {
        return None;
    }
    let mut k = [0u8; 21];
    k[0] = match a[0] {
        PKH => 0,
        P2SH => 1,
        _ => return None,
    };
    k[1..].copy_from_slice(&a[1..21]);
    Some(k)
}

fn script_of(k: &Key) -> String {
    if k[0] == 0 {
        format!("76a914{}88ac", hex::encode(&k[1..]))
    } else {
        format!("a914{}87", hex::encode(&k[1..]))
    }
}

pub fn shown(t: &Txid) -> String {
    let mut x = *t;
    x.reverse();
    hex::encode(x)
}

pub fn internal(s: &str) -> Result<Txid, String> {
    let mut t: Txid = hex::decode(s).ok().and_then(|v| v.try_into().ok()).ok_or("Hash unlesbar")?;
    t.reverse();
    Ok(t)
}

// ------------------------------------------------------------------ Index
impl Utxo {
    fn take_coin(&mut self, k: &Key, prev: &(Txid, u32)) -> Option<Coin> {
        let v = self.coins.get_mut(k)?;
        let i = v.iter().position(|c| c.txid == prev.0 && c.vout == prev.1)?;
        let c = v.swap_remove(i);
        if v.is_empty() {
            self.coins.remove(k);
        }
        Some(c)
    }

    fn apply(&mut self, cfg: &Cfg, raw: &[u8], height: u32) -> Result<bool, String> {
        let (hash, prev, txs) = read_block(raw)?;
        if prev != self.hash {
            return Ok(false);
        }
        let mut u = Undo { prev, added: Vec::new(), removed: Vec::new() };
        for tx in &txs {
            for i in &tx.ins {
                if i.prev.0 == [0; 32] {
                    continue; // Coinbase
                }
                let keys: Vec<Key> = match last_push(i.sig) {
                    Some(h) => [0u8, 1].iter().map(|t| { let mut k = [*t; 21]; k[1..].copy_from_slice(&h); k }).collect(),
                    None => prev_key(cfg, &i.prev).into_iter().collect(),
                };
                for k in keys {
                    if let Some(c) = self.take_coin(&k, &i.prev) {
                        u.removed.push((k, c));
                        break;
                    }
                }
            }
            for (n, (value, spk)) in tx.outs.iter().enumerate() {
                if let Some(k) = out_key(spk) {
                    self.coins.entry(k).or_default().push(Coin { txid: tx.txid, vout: n as u32, value: *value, height });
                    u.added.push((k, tx.txid, n as u32));
                }
            }
        }
        self.height = height;
        self.hash = hash;
        if height + KEEP_UNDO > self.tip {
            self.undo.push_back(u);
            if self.undo.len() > KEEP_UNDO as usize {
                self.undo.pop_front();
            }
        }
        Ok(true)
    }

    /// Letzten Block zuruecknehmen; false, wenn keine Rueckgaengig-Daten mehr da sind
    fn unapply(&mut self) -> bool {
        let Some(u) = self.undo.pop_back() else { return false };
        for (k, t, n) in u.added.iter().rev() {
            self.take_coin(k, &(*t, *n));
        }
        for (k, c) in u.removed {
            self.coins.entry(k).or_default().push(c);
        }
        self.height -= 1;
        self.hash = u.prev;
        true
    }

    pub fn ready(&self) -> bool {
        self.tip > 0 && self.height >= self.tip
    }

    pub fn progress(&self) -> (u32, u32) {
        (self.height, self.tip)
    }

    /// Unverbrauchte Coins einer Adresse, Mempool eingerechnet (Bestaetigungen 0)
    fn list(&self, k: &Key) -> Vec<Value> {
        let script = script_of(k);
        let row = |t: &Txid, n: u32, v: u64, conf: u32| json!({"txid": shown(t), "vout": n, "sats": v, "confirmations": conf, "scriptPubKey": script});
        let mine = self.coins.get(k).map(|v| v.as_slice()).unwrap_or(&[]);
        let mut out: Vec<Value> = mine.iter().filter(|c| !self.mem_spent.contains(&(c.txid, c.vout)))
            .map(|c| row(&c.txid, c.vout, c.value, self.height + 1 - c.height)).collect();
        for (t, m) in &self.mempool {
            for (pk, n, v) in &m.pays {
                if pk == k && !self.mem_spent.contains(&(*t, *n)) && !mine.iter().any(|c| c.txid == *t && c.vout == *n) {
                    out.push(row(t, *n, *v, 0));
                }
            }
        }
        out
    }

    fn save(&mut self, path: &PathBuf) -> std::io::Result<()> {
        let tmp = path.with_extension("new");
        let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w.write_all(&self.height.to_le_bytes())?;
        w.write_all(&self.hash)?;
        w.write_all(&(self.coins.len() as u32).to_le_bytes())?;
        for (k, v) in &self.coins {
            w.write_all(k)?;
            w.write_all(&(v.len() as u32).to_le_bytes())?;
            for c in v {
                w.write_all(&c.txid)?;
                w.write_all(&c.vout.to_le_bytes())?;
                w.write_all(&c.value.to_le_bytes())?;
                w.write_all(&c.height.to_le_bytes())?;
            }
        }
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, path)?;
        self.saved = self.height;
        Ok(())
    }

    fn load(path: &PathBuf) -> std::io::Result<Utxo> {
        let mut r = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
        let mut b4 = [0u8; 4];
        let mut b8 = [0u8; 8];
        let mut u = Utxo::default();
        r.read_exact(&mut b8)?;
        if &b8 != MAGIC {
            return Err(std::io::Error::other("falsches Format"));
        }
        r.read_exact(&mut b4)?;
        u.height = u32::from_le_bytes(b4);
        r.read_exact(&mut u.hash)?;
        r.read_exact(&mut b4)?;
        let keys = u32::from_le_bytes(b4);
        u.coins.reserve(keys as usize);
        for _ in 0..keys {
            let mut k = [0u8; 21];
            r.read_exact(&mut k)?;
            r.read_exact(&mut b4)?;
            let n = u32::from_le_bytes(b4) as usize;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                let mut c = Coin { txid: [0; 32], vout: 0, value: 0, height: 0 };
                r.read_exact(&mut c.txid)?;
                r.read_exact(&mut b4)?;
                c.vout = u32::from_le_bytes(b4);
                r.read_exact(&mut b8)?;
                c.value = u64::from_le_bytes(b8);
                r.read_exact(&mut b4)?;
                c.height = u32::from_le_bytes(b4);
                v.push(c);
            }
            u.coins.insert(k, v);
        }
        u.saved = u.height;
        Ok(u)
    }
}

/// Fuer ein scriptSig mit Nicht-Push-Befehlen (selten): Adresse aus der Vor-Transaktion (txindex=1)
fn prev_key(cfg: &Cfg, prev: &(Txid, u32)) -> Option<Key> {
    let hex = rpc(cfg, "getrawtransaction", json!([shown(&prev.0), 0])).ok()?;
    let raw = hex::decode(hex.as_str()?).ok()?;
    let tx = read_tx(&mut Rd { b: &raw, p: 0 }).ok()?;
    out_key(tx.outs.get(prev.1 as usize)?.1)
}

/// Mehrere RPC-Aufrufe in einer Anfrage (JSON-RPC-Batch); Ergebnisse in Aufruf-Reihenfolge
pub fn batch(cfg: &Cfg, method: &str, params: impl Iterator<Item = Value>) -> Result<Vec<Value>, String> {
    let calls: Vec<Value> = params.enumerate().map(|(i, p)| json!({"jsonrpc": "1.0", "id": i, "method": method, "params": p})).collect();
    if calls.is_empty() {
        return Ok(Vec::new());
    }
    let v = rpc_post(cfg, &Value::Array(calls).to_string())?;
    let mut rows = v.as_array().cloned().ok_or("RPC: keine Liste")?;
    rows.sort_by_key(|r| r["id"].as_u64().unwrap_or(0));
    rows.into_iter().map(|r| if r["error"].is_null() { Ok(r["result"].clone()) } else { Err(format!("{method}: {}", r["error"]["message"])) }).collect()
}

// ------------------------------------------------------------------ Faden: Kette + Mempool nachziehen
fn step(app: &App, path: &PathBuf) -> Result<bool, String> {
    let cfg = &app.cfg;
    let tip = rpc(cfg, "getblockcount", json!([]))?.as_u64().ok_or("getblockcount")? as u32;
    let height = {
        let mut u = app.utxo.lock().unwrap();
        u.tip = tip;
        if tip < u.height && !u.unapply() {
            *u = Utxo { tip, ..Default::default() };
        }
        u.height
    };
    if height == 0 && app.utxo.lock().unwrap().hash == [0; 32] {
        let g = internal(rpc(cfg, "getblockhash", json!([0]))?.as_str().unwrap_or(""))?;
        app.utxo.lock().unwrap().hash = g;
    }
    if tip > height {
        let to = tip.min(height + BATCH);
        let hashes = batch(cfg, "getblockhash", (height + 1..=to).map(|h| json!([h])))?;
        let blocks = batch(cfg, "getblock", hashes.iter().map(|h| json!([h, false])))?;
        let raws: Vec<Vec<u8>> = blocks.iter().map(|b| hex::decode(b.as_str().unwrap_or("")).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
        let mut u = app.utxo.lock().unwrap();
        if u.height != height {
            return Ok(true);
        }
        for (i, raw) in raws.iter().enumerate() {
            if !u.apply(cfg, raw, height + 1 + i as u32)? {
                // Reorg: einen Block zuruecknehmen, dann neu holen
                if !u.unapply() {
                    log("UTXO-Index: Reorg tiefer als die Rueckgaengig-Daten, baue neu");
                    *u = Utxo { tip, ..Default::default() };
                }
                return Ok(true);
            }
        }
        if u.height == tip || u.height - u.saved >= 250_000 {
            if u.height == tip && u.saved == 0 {
                log(&format!("UTXO-Index: Kette eingelesen bis {}, {} Adressen", u.height, u.coins.len()));
            }
            if u.height - u.saved >= 60 || u.saved == 0 {
                if let Err(e) = u.save(path) {
                    log(&format!("UTXO-Index: Speichern: {e}"));
                }
            }
        }
        return Ok(u.height < tip);
    }
    mempool(app)?;
    Ok(false)
}

fn mempool(app: &App) -> Result<(), String> {
    let ids: HashSet<Txid> = rpc(&app.cfg, "getrawmempool", json!([]))?.as_array().cloned().unwrap_or_default()
        .iter().filter_map(|t| internal(t.as_str()?).ok()).collect();
    let new: Vec<Txid> = {
        let mut u = app.utxo.lock().unwrap();
        u.mempool.retain(|t, _| ids.contains(t));
        ids.into_iter().filter(|t| !u.mempool.contains_key(t)).take(2000).collect()
    };
    let hexes = batch(&app.cfg, "getrawtransaction", new.iter().map(|t| json!([shown(t), 0])))
        .unwrap_or_default(); // eine inzwischen bestaetigte Tx: naechste Runde
    let mut u = app.utxo.lock().unwrap();
    for h in hexes {
        let Some(raw) = h.as_str().and_then(|s| hex::decode(s).ok()) else { continue };
        let Ok(tx) = read_tx(&mut Rd { b: &raw, p: 0 }) else { continue };
        let m = MemTx {
            spends: tx.ins.iter().map(|i| i.prev).collect(),
            pays: tx.outs.iter().enumerate().filter_map(|(n, (v, s))| Some((out_key(s)?, n as u32, *v))).collect(),
        };
        u.mempool.insert(tx.txid, m);
    }
    let spent: HashSet<(Txid, u32)> = u.mempool.values().flat_map(|m| m.spends.iter().copied()).collect();
    u.mem_spent = spent;
    Ok(())
}

pub fn run(app: Arc<App>) {
    let path = app.cfg.state.join("utxo.idx");
    match Utxo::load(&path) {
        Ok(mut u) => {
            // erst pruefen, wenn der Knoten antwortet (beim Booten startet er nach uns)
            let found = loop {
                match rpc(&app.cfg, "getblockhash", json!([u.height])) {
                    Ok(h) => break h.as_str().and_then(|s| internal(s).ok()),
                    Err(e) if e.contains("out of range") => break None,
                    Err(_) => std::thread::sleep(Duration::from_secs(10)),
                }
            };
            if found == Some(u.hash) {
                log(&format!("UTXO-Index: Schnappschuss bei Block {} geladen ({} Adressen)", u.height, u.coins.len()));
                u.tip = u.height;
                *app.utxo.lock().unwrap() = u;
            } else {
                log("UTXO-Index: Schnappschuss passt nicht (mehr) zur Kette, baue neu");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => log("UTXO-Index: baue ab Block 1"),
        Err(e) => log(&format!("UTXO-Index: Schnappschuss unlesbar ({e}), baue neu")),
    }
    loop {
        match step(&app, &path) {
            Ok(true) => continue,
            Ok(false) => std::thread::sleep(Duration::from_secs(3)),
            Err(e) => {
                log(&format!("UTXO-Index: {e}"));
                std::thread::sleep(Duration::from_secs(15));
            }
        }
    }
}

/// Unverbrauchte Coins einer Adresse (wie listunspent, Betrag in "sats")
pub fn coins(app: &App, addr: &str) -> Result<Vec<Value>, String> {
    let k = key_of(addr).ok_or("Adresse unlesbar")?;
    let u = app.utxo.lock().unwrap();
    if !u.ready() {
        let (h, t) = u.progress();
        return Err(format!("Guthaben-Index baut noch auf (Block {h} von {t})"));
    }
    Ok(u.list(&k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_hash() {
        // P2PKH-scriptSig: <sig> <pubkey>
        let pk = [2u8; 33];
        let mut s = vec![3, 1, 2, 3, 33];
        s.extend_from_slice(&pk);
        assert_eq!(last_push(&s), Some(hash160(&pk)));
        assert_eq!(last_push(&[0x76]), None);
    }

    #[test]
    fn keys() {
        let h = [7u8; 20];
        let a = b58check(PKH, &h);
        let k = key_of(&a).unwrap();
        assert_eq!(k[0], 0);
        assert_eq!(out_key(&hex::decode(script_of(&k)).unwrap()), Some(k));
        let s = b58check(P2SH, &h);
        let k = key_of(&s).unwrap();
        assert_eq!(out_key(&hex::decode(script_of(&k)).unwrap()), Some(k));
    }
}
