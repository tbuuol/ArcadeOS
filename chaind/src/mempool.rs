//! Unconfirmed transactions: checked like a block's (the coins they spend exist and are not spent twice,
//! amounts, maturity, lock time, scripts with the next block's flags) plus a minimum fee; dropped when a
//! block confirms them or spends what they spend.

use crate::node::Shared;
use crate::rules::{self, Spent};
use crate::store::{outkey, Coin, OutKey};
use arcade_chain::{Hash256, Tx};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

/// Pepecoin Core's relay fee: 0.001 coins per 1000 bytes
pub const MIN_FEE_PER_KB: u64 = 100_000;

pub struct MemTx {
    pub tx: Tx,
    pub fee: u64,
    pub time: u64,
}

#[derive(Default)]
pub struct Mempool {
    pub txs: HashMap<Hash256, MemTx>,
    /// outpoint -> the mempool tx spending it
    spent_by: HashMap<OutKey, Hash256>,
    pub order: Vec<Hash256>,
}

impl Mempool {
    pub fn coin(&self, k: &OutKey) -> Option<Coin> {
        let txid = Hash256(k[..32].try_into().unwrap());
        let vout = u32::from_be_bytes(k[32..].try_into().unwrap());
        let t = self.txs.get(&txid)?;
        let o = t.tx.outputs.get(vout as usize)?;
        Some(Coin { value: o.value, height: u32::MAX, coinbase: false, script: o.script_pubkey.clone(), pegout: false, time: t.tx.time.unwrap_or(0) })
    }

    pub fn spends(&self, k: &OutKey) -> Option<Hash256> {
        self.spent_by.get(k).copied()
    }

    fn remove(&mut self, txid: &Hash256) {
        if let Some(t) = self.txs.remove(txid) {
            for i in &t.tx.inputs {
                self.spent_by.remove(&outkey(&i.prevout.txid, i.prevout.vout));
            }
            self.order.retain(|x| x != txid);
            // whatever spent its outputs goes too
            for v in 0..t.tx.outputs.len() as u32 {
                if let Some(child) = self.spent_by.get(&outkey(txid, v)).copied() {
                    self.remove(&child);
                }
            }
        }
    }

    /// A block came: its transactions are confirmed, and what spends the same coins as they do is void.
    pub fn block_connected(&mut self, txs: &[Tx]) {
        for t in txs {
            self.remove(&t.txid);
            if !t.is_coinbase() {
                for i in &t.inputs {
                    if let Some(c) = self.spent_by.get(&outkey(&i.prevout.txid, i.prevout.vout)).copied() {
                        self.remove(&c);
                    }
                }
            }
        }
    }

    pub fn clear(&mut self) {
        *self = Mempool::default();
    }
}

/// Check a transaction against the chain and the mempool, and take it in. Ok(txid) or Core's reason.
pub fn accept(sh: &Shared, raw: &[u8]) -> Result<Hash256, String> {
    let p = sh.params;
    let tx = Tx::parse_with(raw, &p.format).map_err(|_| "TX decode failed".to_string())?;
    if tx.is_coinbase() {
        return Err("coinbase".into());
    }
    if tx.inputs.is_empty() || tx.outputs.is_empty() {
        return Err("bad-txns-vin-empty / vout-empty".into());
    }
    if raw.len() > 100_000 {
        return Err("tx-size".into());
    }
    let tip = sh.ctip.load(Ordering::SeqCst);
    if tip + 6 < sh.htip.load(Ordering::SeqCst) {
        return Err("still syncing".into());
    }
    let height = tip + 1;
    let st = sh.store;
    let mut mp = sh.mempool.lock().unwrap();
    if mp.txs.contains_key(&tx.txid) {
        return Err("txn-already-in-mempool".into());
    }
    let r = st.env.read_txn().map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    let mut coins = Vec::with_capacity(tx.inputs.len());
    for i in &tx.inputs {
        let k = outkey(&i.prevout.txid, i.prevout.vout);
        if !seen.insert(k) {
            return Err("bad-txns-inputs-duplicate".into());
        }
        if mp.spends(&k).is_some() {
            return Err("txn-mempool-conflict".into());
        }
        let c = match mp.coin(&k) {
            Some(c) => c,
            None => st.coins.get(&r, &k).ok().flatten().and_then(Coin::decode).ok_or("bad-txns-inputs-missingorspent")?,
        };
        coins.push(c);
    }
    drop(r);
    let vr = p.value_rules.ok_or("no coin rules")?;
    let (mut vin, mut vout) = (0u128, 0u128);
    for c in &coins {
        vin += c.value as u128;
        if c.coinbase && c.height != u32::MAX && height.saturating_sub(c.height) < (vr.maturity)(c.height) {
            return Err("bad-txns-premature-spend-of-coinbase".into());
        }
    }
    for o in &tx.outputs {
        if o.value > vr.max_money {
            return Err("bad-txns-vout-toolarge".into());
        }
        vout += o.value as u128;
    }
    if vin < vout {
        return Err("bad-txns-in-belowout".into());
    }
    let fee = (vin - vout) as u64;
    let need = MIN_FEE_PER_KB * raw.len() as u64 / 1000;
    if fee < need {
        return Err(format!("min relay fee not met: {fee} < {need} (66: insufficient priority)"));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if !arcade_chain::sigops::is_final(&tx, height, now as u32) {
        return Err("non-final".into());
    }
    if let Some(flags) = rules::script_flags(p, height, true) {
        let sp: Vec<Spent> = coins.iter().map(|c| Spent { value: c.value, script: &c.script, height: c.height.min(tip), coinbase: c.coinbase, pegout: c.pegout }).collect();
        rules::tx_scripts(p, &tx, &sp, height, flags).map_err(|e| format!("mandatory-script-verify-flag-failed ({})", e.why))?;
    }
    let txid = tx.txid;
    for i in &tx.inputs {
        mp.spent_by.insert(outkey(&i.prevout.txid, i.prevout.vout), txid);
    }
    mp.order.push(txid);
    mp.txs.insert(txid, MemTx { tx, fee, time: now });
    Ok(txid)
}
