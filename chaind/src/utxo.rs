//! The coin set as Core 0.14 sums it up (`gettxoutsetinfo`, hash_serialized): the same bytes hashed in the same
//! order, so one comparison with a Core node of the same chain at the same block proves both hold exactly the
//! same coins. (Pepecoin and Dogecoin are Core 0.14 underneath; per txid: the txid, then each unspent output as
//! VARINT(n+1) + CTxOut, then VARINT(0); before everything the best block's hash.)

use crate::store::{Coin, Store};
use arcade_chain::Hash256;
use sha2::{Digest, Sha256};

/// Bitcoin's VARINT (serialize.h WriteVarInt): base 128, high bit = more, each further byte minus one.
fn btc_varint(h: &mut Sha256, mut n: u64) {
    let mut tmp = [0u8; 10];
    let mut len = 0;
    loop {
        tmp[len] = (n & 0x7f) as u8 | if len > 0 { 0x80 } else { 0 };
        if n <= 0x7f {
            break;
        }
        n = (n >> 7) - 1;
        len += 1;
    }
    for i in (0..=len).rev() {
        h.update([tmp[i]]);
    }
}

fn compact(h: &mut Sha256, n: u64) {
    match n {
        0..=0xfc => h.update([n as u8]),
        0xfd..=0xffff => {
            h.update([0xfd]);
            h.update((n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            h.update([0xfe]);
            h.update((n as u32).to_le_bytes());
        }
        _ => {
            h.update([0xff]);
            h.update(n.to_le_bytes());
        }
    }
}

pub struct Stats {
    pub height: u32,
    pub best: Hash256,
    pub transactions: u64,
    pub txouts: u64,
    pub total: u128,
    pub hash: Hash256,
}

/// Core 0.15+ (Bitcoin/Litecoin 0.21: hash_serialized_2), per txid: the txid, VARINT(1) (Core's operator-precedence
/// slip makes "height*2+coinbase ? 1 : 0" always 1), then per output VARINT(n+1), the script, VARINT(value), then
/// VARINT(0).
pub fn stats_v2(st: &Store) -> Result<Stats, String> {
    let r = st.env.read_txn().map_err(|e| e.to_string())?;
    let height = st.meta.get(&r, b"tip").map_err(|e| e.to_string())?.map(|v| u32::from_le_bytes(v.try_into().unwrap())).unwrap_or(0);
    let best = st.id_at(height).ok_or("no tip")?;
    let mut h = Sha256::new();
    h.update(best.0);
    let (mut transactions, mut txouts, mut total) = (0u64, 0u64, 0u128);
    let mut cur: Option<[u8; 32]> = None;
    for item in st.coins.iter(&r).map_err(|e| e.to_string())? {
        let (k, v) = item.map_err(|e| e.to_string())?;
        let txid: [u8; 32] = k[..32].try_into().unwrap();
        let vout = u32::from_be_bytes(k[32..36].try_into().unwrap());
        if cur != Some(txid) {
            if cur.is_some() {
                btc_varint(&mut h, 0);
            }
            h.update(txid);
            btc_varint(&mut h, 1);
            transactions += 1;
            cur = Some(txid);
        }
        let c = Coin::decode(v).ok_or("bad coin record")?;
        btc_varint(&mut h, vout as u64 + 1);
        compact(&mut h, c.script.len() as u64);
        h.update(&c.script);
        btc_varint(&mut h, c.value);
        txouts += 1;
        total += c.value as u128;
    }
    if cur.is_some() {
        btc_varint(&mut h, 0);
    }
    let first = h.finalize();
    Ok(Stats { height, best, transactions, txouts, total, hash: Hash256(Sha256::digest(first).into()) })
}

pub fn stats(st: &Store) -> Result<Stats, String> {
    let r = st.env.read_txn().map_err(|e| e.to_string())?;
    let height = st.meta.get(&r, b"tip").map_err(|e| e.to_string())?.map(|v| u32::from_le_bytes(v.try_into().unwrap())).unwrap_or(0);
    let best = st.id_at(height).ok_or("no tip")?;
    let mut h = Sha256::new();
    h.update(best.0);
    let (mut transactions, mut txouts, mut total) = (0u64, 0u64, 0u128);
    let mut cur: Option<[u8; 32]> = None;
    for item in st.coins.iter(&r).map_err(|e| e.to_string())? {
        let (k, v) = item.map_err(|e| e.to_string())?;
        let txid: [u8; 32] = k[..32].try_into().unwrap();
        let vout = u32::from_be_bytes(k[32..36].try_into().unwrap());
        if cur != Some(txid) {
            if cur.is_some() {
                btc_varint(&mut h, 0);
            }
            h.update(txid);
            transactions += 1;
            cur = Some(txid);
        }
        let c = Coin::decode(v).ok_or("bad coin record")?;
        btc_varint(&mut h, vout as u64 + 1);
        h.update(c.value.to_le_bytes());
        compact(&mut h, c.script.len() as u64);
        h.update(&c.script);
        txouts += 1;
        total += c.value as u128;
    }
    if cur.is_some() {
        btc_varint(&mut h, 0);
    }
    let first = h.finalize();
    let hash = Hash256(Sha256::digest(first).into());
    Ok(Stats { height, best, transactions, txouts, total, hash })
}
