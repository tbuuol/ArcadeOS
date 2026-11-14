//! Where chaind keeps a chain, in one directory:
//!
//!   headers.dat   the best chain's headers, 112 bytes each at height * 112: block id (32) + header (80)
//!   blocks.dat    raw blocks as they came, appended; blkpos.dat: 12 bytes per height (offset u64, length u32)
//!   undo.dat      per block the coins it spent (to take it back in a reorg); undopos.dat like blkpos.dat
//!   chain.lmdb    LMDB: `coins` (outpoint -> coin), `txi` (txid prefix + height + position -> where the tx is),
//!                 `hidx` (block id -> height, best chain), `meta` (the tip the coins belong to)
//!
//! LMDB is a memory-mapped file: what of it is in RAM is page cache the kernel takes back when it needs the
//! memory. Nothing about the chain stays in this process' own memory except what is in flight.

use arcade_chain::{Hash256, Header};
use heed::types::Bytes;
use heed::{Database, Env, EnvOpenOptions};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

pub const HREC: u64 = 112;

/// A coin: an unspent output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coin {
    pub value: u64,
    pub height: u32,
    pub coinbase: bool,
    pub script: Vec<u8>,
    /// a Litecoin MWEB peg-out (a HogEx output after the first): it matures too, sooner
    pub pegout: bool,
}

const PEGOUT_BIT: u64 = 1 << 31;

/// The record format of this directory: 1 = coins as they are, tx index with positions (the first chaind);
/// 2 = coins compressed as Core does it, tx index of txid prefix + height only. Set once, when the directory is new.
pub static FORMAT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);
pub fn fmt() -> u8 {
    FORMAT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Core's CompressAmount: trailing zeros as an exponent (1.00000000 coins -> a byte or two)
pub fn compress_amount(mut n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut e = 0;
    while n % 10 == 0 && e < 9 {
        n /= 10;
        e += 1;
    }
    if e < 9 {
        let d = n % 10;
        n /= 10;
        1 + (n * 9 + d - 1) * 10 + e
    } else {
        1 + (n - 1) * 10 + 9
    }
}

pub fn decompress_amount(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let mut x = x - 1;
    let e = x % 10;
    x /= 10;
    let mut n = if e < 9 {
        let d = x % 9 + 1;
        x /= 9;
        x * 10 + d
    } else {
        x + 1
    };
    for _ in 0..e {
        n *= 10;
    }
    n
}

/// Standard scripts as a tag and their hash or key (as Core's ScriptCompression, with SegWit, Taproot and MWEB
/// programs added); anything else whole after 0xff.
pub fn compress_script(s: &[u8], v: &mut Vec<u8>) {
    let tag = match s {
        [0x76, 0xa9, 20, .., 0x88, 0xac] if s.len() == 25 => Some((0u8, &s[3..23])),
        [0xa9, 20, .., 0x87] if s.len() == 23 => Some((1, &s[2..22])),
        [33, p, .., 0xac] if s.len() == 35 && (*p == 2 || *p == 3) => Some((*p, &s[2..34])),
        [0x00, 20, ..] if s.len() == 22 => Some((4, &s[2..22])),
        [0x00, 32, ..] if s.len() == 34 => Some((5, &s[2..34])),
        [0x51, 32, ..] if s.len() == 34 => Some((6, &s[2..34])),
        [0x58, 32, ..] if s.len() == 34 => Some((7, &s[2..34])),
        [0x59, 32, ..] if s.len() == 34 => Some((8, &s[2..34])),
        _ => None,
    };
    match tag {
        Some((t, d)) => {
            v.push(t);
            v.extend_from_slice(d);
        }
        None => {
            v.push(0xff);
            v.extend_from_slice(s);
        }
    }
}

pub fn decompress_script(b: &[u8]) -> Option<Vec<u8>> {
    let (t, d) = b.split_first()?;
    let mut s = Vec::with_capacity(d.len() + 6);
    match (*t, d.len()) {
        (0, 20) => { s.extend([0x76, 0xa9, 20]); s.extend_from_slice(d); s.extend([0x88, 0xac]); }
        (1, 20) => { s.extend([0xa9, 20]); s.extend_from_slice(d); s.push(0x87); }
        (2 | 3, 32) => { s.extend([33, *t]); s.extend_from_slice(d); s.push(0xac); }
        (4, 20) => { s.extend([0x00, 20]); s.extend_from_slice(d); }
        (5, 32) => { s.extend([0x00, 32]); s.extend_from_slice(d); }
        (6, 32) => { s.extend([0x51, 32]); s.extend_from_slice(d); }
        (7, 32) => { s.extend([0x58, 32]); s.extend_from_slice(d); }
        (8, 32) => { s.extend([0x59, 32]); s.extend_from_slice(d); }
        (0xff, _) => s.extend_from_slice(d),
        _ => return None,
    }
    Some(s)
}

impl Coin {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.script.len() + 12);
        put_varint(&mut v, ((self.height as u64) | if self.pegout { PEGOUT_BIT } else { 0 }) << 1 | self.coinbase as u64);
        if fmt() >= 2 {
            put_varint(&mut v, compress_amount(self.value));
            compress_script(&self.script, &mut v);
        } else {
            put_varint(&mut v, self.value);
            v.extend_from_slice(&self.script);
        }
        v
    }
    pub fn decode(b: &[u8]) -> Option<Coin> {
        let (hc, n1) = get_varint(b)?;
        let (value, n2) = get_varint(&b[n1..])?;
        let h = hc >> 1;
        let (value, script) = if fmt() >= 2 { (decompress_amount(value), decompress_script(&b[n1 + n2..])?) } else { (value, b[n1 + n2..].to_vec()) };
        Some(Coin { value, height: (h & !PEGOUT_BIT) as u32, coinbase: hc & 1 == 1, script, pegout: h & PEGOUT_BIT != 0 })
    }
}

/// Outpoint as a key: txid (internal byte order) + vout big-endian, so a txid's outputs sort together and in order.
pub type OutKey = [u8; 36];

pub fn outkey(txid: &Hash256, vout: u32) -> OutKey {
    let mut k = [0u8; 36];
    k[..32].copy_from_slice(&txid.0);
    k[32..].copy_from_slice(&vout.to_be_bytes());
    k
}

/// LEB128-style varint (7 bits a byte, low first): only for chaind's own records.
pub fn put_varint(v: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            v.push(b);
            return;
        }
        v.push(b | 0x80);
    }
}

pub fn get_varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut n = 0u64;
    for (i, &x) in b.iter().enumerate().take(10) {
        n |= ((x & 0x7f) as u64) << (7 * i);
        if x & 0x80 == 0 {
            return Some((n, i + 1));
        }
    }
    None
}

pub struct Store {
    pub env: Env,
    pub meta: Database<Bytes, Bytes>,
    pub hidx: Database<Bytes, Bytes>,
    pub coins: Database<Bytes, Bytes>,
    pub txi: Database<Bytes, Bytes>,
    headers: File,
    blocks: File,
    blkpos: File,
    undo: File,
    undopos: File,
    pub blocks_len: std::sync::atomic::AtomicU64,
    pub undo_len: std::sync::atomic::AtomicU64,
    /// block id (first 8 bytes) -> height on the best chain; rebuilt from headers.dat at start (a random-key
    /// B-tree on disk churns pages for nothing)
    ids: RwLock<HashMap<u64, u32>>,
}

fn open(dir: &Path, name: &str) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(name))
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store, String> {
        std::fs::create_dir_all(dir.join("chain.lmdb")).map_err(|e| e.to_string())?;
        // The map is address space, not memory: 64 GB leaves room for any of the chains chaind is meant for.
        // WRITE_MAP: LMDB writes straight into the mapped file instead of into pages it mallocs (and keeps as a
        // free list after a big write: hundreds of MB of the process' own memory). What is dirty is page cache.
        let env = unsafe {
            let mut o = EnvOpenOptions::new();
            o.map_size(64 << 30).max_dbs(8).flags(heed::EnvFlags::WRITE_MAP);
            o.open(dir.join("chain.lmdb"))
        }
            .map_err(|e| format!("lmdb: {e}"))?;
        let mut w = env.write_txn().map_err(|e| e.to_string())?;
        let meta: Database<Bytes, Bytes> = env.create_database(&mut w, Some("meta")).map_err(|e| e.to_string())?;
        let hidx = env.create_database(&mut w, Some("hidx")).map_err(|e| e.to_string())?;
        let coins = env.create_database(&mut w, Some("coins")).map_err(|e| e.to_string())?;
        let txi = env.create_database(&mut w, Some("txi")).map_err(|e| e.to_string())?;
        // the record format: what the directory has, 2 for a new one
        let f_now = match meta.get(&w, b"fmt").map_err(|e| e.to_string())? {
            Some(v) => v.first().copied().unwrap_or(1),
            None if meta.get(&w, b"tip").map_err(|e| e.to_string())?.is_some() => 1,
            None => {
                let want: u8 = std::env::var("CHAIND_FORMAT").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
                meta.put(&mut w, b"fmt", &[want]).map_err(|e| e.to_string())?;
                want
            }
        };
        FORMAT.store(f_now, std::sync::atomic::Ordering::Relaxed);
        w.commit().map_err(|e| e.to_string())?;
        let f = |n| open(dir, n).map_err(|e| format!("{n}: {e}"));
        let (blocks, undo) = (f("blocks.dat")?, f("undo.dat")?);
        let bl = blocks.metadata().map_err(|e| e.to_string())?.len();
        let ul = undo.metadata().map_err(|e| e.to_string())?.len();
        Ok(Store {
            env,
            meta,
            hidx,
            coins,
            txi,
            headers: f("headers.dat")?,
            blocks,
            blkpos: f("blkpos.dat")?,
            undo,
            undopos: f("undopos.dat")?,
            blocks_len: bl.into(),
            undo_len: ul.into(),
            ids: RwLock::new(HashMap::new()),
        })
    }

    // ---- headers.dat ----
    pub fn put_header(&self, height: u32, id: &Hash256, h: &Header) {
        let mut rec = [0u8; HREC as usize];
        rec[..32].copy_from_slice(&id.0);
        rec[32..].copy_from_slice(&h.encode());
        self.headers.write_all_at(&rec, height as u64 * HREC).expect("headers.dat write");
    }
    pub fn header(&self, height: u32) -> Option<(Hash256, Header)> {
        let mut rec = [0u8; HREC as usize];
        self.headers.read_exact_at(&mut rec, height as u64 * HREC).ok()?;
        let id = Hash256(rec[..32].try_into().unwrap());
        Some((id, Header::parse(rec[32..].try_into().unwrap())))
    }
    pub fn id_at(&self, height: u32) -> Option<Hash256> {
        let mut id = [0u8; 32];
        self.headers.read_exact_at(&mut id, height as u64 * HREC).ok()?;
        Some(Hash256(id))
    }

    // ---- blocks.dat / undo.dat ----
    fn append(file: &File, len: &std::sync::atomic::AtomicU64, pos: &File, height: u32, data: &[u8]) {
        let off = len.fetch_add(data.len() as u64, std::sync::atomic::Ordering::SeqCst);
        file.write_all_at(data, off).expect("append");
        let mut p = [0u8; 12];
        p[..8].copy_from_slice(&off.to_le_bytes());
        p[8..].copy_from_slice(&(data.len() as u32).to_le_bytes());
        pos.write_all_at(&p, height as u64 * 12).expect("pos write");
    }
    fn read(file: &File, pos: &File, height: u32) -> Option<Vec<u8>> {
        let mut p = [0u8; 12];
        pos.read_exact_at(&mut p, height as u64 * 12).ok()?;
        let off = u64::from_le_bytes(p[..8].try_into().unwrap());
        let n = u32::from_le_bytes(p[8..].try_into().unwrap()) as usize;
        if n == 0 {
            return None;
        }
        let mut v = vec![0u8; n];
        file.read_exact_at(&mut v, off).ok()?;
        Some(v)
    }
    pub fn put_block(&self, height: u32, raw: &[u8]) {
        Self::append(&self.blocks, &self.blocks_len, &self.blkpos, height, raw)
    }
    pub fn block(&self, height: u32) -> Option<Vec<u8>> {
        Self::read(&self.blocks, &self.blkpos, height)
    }
    /// A piece of a stored block (a transaction), without reading the whole block.
    pub fn block_part(&self, height: u32, from: u32, len: u32) -> Option<Vec<u8>> {
        let mut p = [0u8; 12];
        self.blkpos.read_exact_at(&mut p, height as u64 * 12).ok()?;
        let off = u64::from_le_bytes(p[..8].try_into().unwrap());
        let mut v = vec![0u8; len as usize];
        self.blocks.read_exact_at(&mut v, off + from as u64).ok()?;
        Some(v)
    }
    pub fn put_undo(&self, height: u32, data: &[u8]) {
        Self::append(&self.undo, &self.undo_len, &self.undopos, height, data)
    }
    pub fn undo(&self, height: u32) -> Option<Vec<u8>> {
        Self::read(&self.undo, &self.undopos, height)
    }

    /// Everything written to the files reaches the disk (before LMDB says the coins are at that tip).
    pub fn sync_files(&self) {
        for f in [&self.headers, &self.blocks, &self.blkpos, &self.undo, &self.undopos] {
            let _ = f.sync_data();
        }
    }

    // ---- meta ----
    pub fn get_meta_u32(&self, key: &str) -> Option<u32> {
        let r = self.env.read_txn().ok()?;
        let v = self.meta.get(&r, key.as_bytes()).ok()??;
        Some(u32::from_le_bytes(v.try_into().ok()?))
    }

    pub fn height_of(&self, id: &Hash256) -> Option<u32> {
        let h = *self.ids.read().unwrap().get(&pfx(id))?;
        (self.id_at(h) == Some(*id)).then_some(h)
    }
    pub fn index_id(&self, id: &Hash256, height: u32) {
        self.ids.write().unwrap().insert(pfx(id), height);
    }
    pub fn unindex_id(&self, id: &Hash256) {
        self.ids.write().unwrap().remove(&pfx(id));
    }
    /// Read the best chain's ids from headers.dat (up to `htip`) into the index.
    pub fn load_ids(&self, htip: u32) {
        let mut m = HashMap::with_capacity(htip as usize + 1 + 100_000);
        let mut buf = vec![0u8; HREC as usize * 8192];
        let mut h = 0u32;
        while h <= htip {
            let n = ((htip - h + 1) as usize).min(8192);
            let b = &mut buf[..n * HREC as usize];
            if self.headers.read_exact_at(b, h as u64 * HREC).is_err() {
                break;
            }
            for i in 0..n {
                let id: [u8; 8] = b[i * HREC as usize..i * HREC as usize + 8].try_into().unwrap();
                m.insert(u64::from_le_bytes(id), h + i as u32);
            }
            h += n as u32;
        }
        *self.ids.write().unwrap() = m;
    }
}

fn pfx(id: &Hash256) -> u64 {
    u64::from_le_bytes(id.0[..8].try_into().unwrap())
}

/// The key of a transaction in `txi`: the first 8 bytes of its txid and the block height (format 2: the block is
/// read to find the transaction in it), and in format 1 also its position.
pub fn txi_key(txid: &Hash256, height: u32, pos: u16) -> ([u8; 14], usize) {
    let mut k = [0u8; 14];
    k[..8].copy_from_slice(&txid.0[..8]);
    k[8..12].copy_from_slice(&height.to_be_bytes());
    k[12..].copy_from_slice(&pos.to_be_bytes());
    (k, if fmt() >= 2 { 12 } else { 14 })
}

/// Blocks of another chaind directory, read-only (to rebuild a chain without downloading it again).
pub struct Import {
    blocks: File,
    blkpos: File,
}

impl Import {
    pub fn open(dir: &Path) -> Result<Import, String> {
        let f = |n: &str| File::open(dir.join(n)).map_err(|e| format!("{}: {e}", dir.join(n).display()));
        Ok(Import { blocks: f("blocks.dat")?, blkpos: f("blkpos.dat")? })
    }
    pub fn block(&self, height: u32) -> Option<Vec<u8>> {
        Store::read(&self.blocks, &self.blkpos, height)
    }
}

/// Undo record of a block: the coins it spent, in input order.
pub fn encode_undo(spent: &[(OutKey, Coin)]) -> Vec<u8> {
    let mut v = Vec::new();
    put_varint(&mut v, spent.len() as u64);
    for (k, c) in spent {
        v.extend_from_slice(k);
        let e = c.encode();
        put_varint(&mut v, e.len() as u64);
        v.extend_from_slice(&e);
    }
    v
}

pub fn decode_undo(b: &[u8]) -> Option<Vec<(OutKey, Coin)>> {
    let (n, mut i) = get_varint(b)?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let k: OutKey = b.get(i..i + 36)?.try_into().ok()?;
        i += 36;
        let (l, m) = get_varint(&b[i..])?;
        i += m;
        out.push((k, Coin::decode(b.get(i..i + l as usize)?)?));
        i += l as usize;
    }
    Some(out)
}
