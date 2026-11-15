//! Kotia staking as a dry run (CHAIND_STAKE_WATCH=file of scriptPubKeys in hex, one per line): every 16-second
//! slot, each mature coin paying to one of those scripts is tried against the kernel, exactly as a staker would.
//! A hit is only logged: no block is built, nothing is signed, nothing is sent. No keys are needed.
//! When a block staked by a watched coin comes in, the log says whether the dry run had predicted it.

use crate::kotia::stake_target;
use crate::node::Shared;
use crate::store::{Coin, OutKey};
use arcade_chain::kotia_pos::{check_kernel, STAKE_TIMESTAMP_MASK};
use arcade_chain::{Block, Hash256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MATURITY: u32 = 20;

fn read_watch(path: &str) -> HashSet<Vec<u8>> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| hex::decode(l.trim()).ok())
        .filter(|s| !s.is_empty())
        .collect()
}

fn scan(sh: &Shared, watch: &HashSet<Vec<u8>>) -> Vec<(OutKey, Coin)> {
    let st = sh.store;
    let Ok(r) = st.env.read_txn() else { return Vec::new() };
    let Ok(it) = st.coins.iter(&r) else { return Vec::new() };
    it.filter_map(|e| e.ok())
        .filter_map(|(k, v)| {
            let c = Coin::decode(v)?;
            if !watch.contains(&c.script) { return None; }
            Some((k.try_into().ok()?, c))
        })
        .collect()
}

pub fn dry_run(sh: Arc<Shared>, path: String) {
    let st = sh.store;
    let mut watch = read_watch(&path);
    let mut watch_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut coins: Vec<(OutKey, Coin)> = Vec::new();
    let mut known: HashSet<OutKey> = HashSet::new();
    let mut scanned_at = u32::MAX;
    let mut last_slot = 0u32;
    let mut seen_tip = 0u32;
    // (height the hit was for) -> hits (slot, coin)
    let mut hits: HashMap<u32, Vec<(u32, OutKey, Result<crate::stakeblock::Built, String>)>> = HashMap::new();
    let (mut n_same, mut n_differ) = (0u64, 0u64);
    let (mut n_hits, mut n_taken, mut n_missed, mut n_lost, mut tried) = (0u64, 0u64, 0u64, 0u64, 0u64);
    sh.log(&format!("stake dry run: {} watched scripts from {path}; blocks are built unsigned to compare, nothing is signed or sent", watch.len()));
    match crate::stakeblock::self_test(sh.params) {
        Ok(()) => sh.log("stake dry run: self test with a throwaway key passed (P2PK and P2PKH coinstake scripts, block signature, shape)"),
        Err(e) => sh.log(&format!("stake dry run: SELF TEST FAILED: {e}")),
    }
    while !sh.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        let m = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if m != watch_mtime {
            watch_mtime = m;
            watch = read_watch(&path);
            scanned_at = u32::MAX;
            sh.log(&format!("stake dry run: watch list now {} scripts", watch.len()));
        }
        let tip = sh.flushed.load(Ordering::SeqCst);
        let synced = tip == sh.ctip.load(Ordering::SeqCst) && tip + 1 >= sh.htip.load(Ordering::SeqCst);
        if !synced || tip == 0 {
            continue;
        }
        // blocks that came since: were the ones our coins staked foreseen?
        if seen_tip == 0 {
            seen_tip = tip;
        }
        while seen_tip < tip {
            seen_tip += 1;
            let ours = st.block(seen_tip).and_then(|raw| Block::parse_with(&raw, &sh.params.format).ok()).and_then(|b| {
                let t = b.header.time;
                b.is_proof_of_stake().then(|| (t, b.txs[1].inputs[0].prevout, b.clone())).filter(|(_, o, _)| known.contains(&crate::store::outkey(&o.txid, o.vout)))
            });
            let mine = hits.remove(&seen_tip).unwrap_or_default();
            let was_ours = ours.is_some();
            match ours {
                Some((t, o, blk)) => {
                    let k = crate::store::outkey(&o.txid, o.vout);
                    if let Some((_, _, bres)) = mine.iter().find(|(s, c, _)| *s == t && *c == k) {
                        match bres {
                            Ok(bl) if bl.header.version == blk.header.version && bl.header.prev == blk.header.prev && bl.header.time == blk.header.time && bl.header.bits == blk.header.bits && bl.coinbase == blk.txs[0].raw && Some(&bl.coinstake_blank) == crate::stakeblock::blank_script_sig(&blk.txs[1]).as_ref() && blk.txs.len() == 2 => {
                                n_same += 1;
                                sh.log(&format!("stake dry run: block {seen_tip}: our built block is the same as the real one (header, coinbase, coinstake; only signatures left out)"));
                            }
                            Ok(bl) => {
                                n_differ += 1;
                                sh.log(&format!("stake dry run: block {seen_tip}: our built block DIFFERS: version {} prev {} time {} bits {} coinbase {} coinstake {} txs {}", bl.header.version == blk.header.version, bl.header.prev == blk.header.prev, bl.header.time == blk.header.time, bl.header.bits == blk.header.bits, bl.coinbase == blk.txs[0].raw, Some(&bl.coinstake_blank) == crate::stakeblock::blank_script_sig(&blk.txs[1]).as_ref(), blk.txs.len()));
                            }
                            Err(e) => sh.log(&format!("stake dry run: block {seen_tip}: not built: {e}")),
                        }
                        n_taken += 1;
                        sh.log(&format!("stake dry run: block {seen_tip} staked by a watched coin ({}:{} at {t}) - PREDICTED", o.txid.to_hex(), o.vout));
                    } else {
                        n_missed += 1;
                        sh.log(&format!("stake dry run: block {seen_tip} staked by a watched coin ({}:{} at {t}) - NOT PREDICTED ({} hits for this height)", o.txid.to_hex(), o.vout, mine.len()));
                    }
                }
                None if !mine.is_empty() => {
                    n_lost += 1;
                    sh.log(&format!("stake dry run: block {seen_tip} went to another staker although we had {} hit(s) (race or late)", mine.len()));
                }
                None => {}
            }
            if seen_tip % 100 == 0 || was_ours {
                sh.log(&format!("stake dry run: totals hits {n_hits}, watched blocks predicted {n_taken}, not predicted {n_missed}, hits lost {n_lost}, built same {n_same}, built different {n_differ}, kernels tried {tried}"));
            }
        }
        hits.retain(|h, _| *h > tip);
        if scanned_at != tip {
            coins = scan(&sh, &watch);
            known.extend(coins.iter().map(|c| c.0));
            if scanned_at == u32::MAX {
                let total: u64 = coins.iter().map(|c| c.1.value).sum();
                sh.log(&format!("stake dry run: {} coins ({} KOT) at tip {tip}", coins.len(), total as f64 / 1e8));
            }
            scanned_at = tip;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0);
        let slot = now & !STAKE_TIMESTAMP_MASK;
        if slot <= last_slot {
            continue;
        }
        last_slot = slot;
        let Some((modifier, bits, tip_time)) = stake_target(st, tip) else { continue };
        if slot <= tip_time {
            continue;
        }
        let height = tip + 1;
        for (k, c) in &coins {
            if height < c.height + MATURITY {
                continue;
            }
            tried += 1;
            let txid = Hash256(k[..32].try_into().unwrap());
            let vout = u32::from_be_bytes(k[32..].try_into().unwrap());
            if check_kernel(&modifier, bits, c.time, &txid, vout, c.value, slot) {
                n_hits += 1;
                let tipid = st.id_at(tip).unwrap_or(Hash256::ZERO);
                let version = st.header(tip).map(|h| h.1.version).unwrap_or(0x2000_0000);
                let built = crate::stakeblock::build(sh.params, version, tipid, height, slot, bits, &txid, vout, c.value, &c.script, None);
                hits.entry(height).or_default().push((slot, *k, built));
                sh.log(&format!("stake dry run: HIT for block {height} at slot {slot}: coin {}:{vout} ({} KOT) - would stake (dry run: not built)", txid.to_hex(), c.value as f64 / 1e8));
            }
        }
    }
}
