//! Kotia staking. Every 16-second slot, each mature coin of ours is tried against the kernel, as Kotia Core's
//! staker does. Three modes, from the environment:
//!
//! - CHAIND_STAKE_WATCH=file (scriptPubKeys in hex, one per line), no keys: dry run. A hit is built unsigned and
//!   only logged; when the real staker's block for that coin comes in, the two are compared.
//! - CHAIND_STAKE_KEYS=file (dumpwallet format) without CHAIND_STAKE=1: signed check. A hit is built, signed and
//!   checked as every node would check it, then only logged; when the real staker's block comes in, the two are
//!   compared byte for byte (signatures too: both sign deterministically, RFC 6979). Nothing is sent.
//! - CHAIND_STAKE_KEYS and CHAIND_STAKE=1: staking. A hit that passes every check is sent to the peers.
//!   At most one block per height. The file CHAIND_STAKE_OFF (default <CHAIND_DIR>/stake-off) stops sending at
//!   once while it exists (the node keeps checking and logging).
//!
//! A block is only ever built on a synced tip, pays exactly coin + 20 KOT back to the coin's own key, and must pass
//! stakeblock::verify (shape, script, kernel, signature) before it can leave.

use crate::kotia::stake_target;
use crate::node::Shared;
use crate::stakeblock::{self, Built, Keys};
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
            if !watch.contains(&c.script) {
                return None;
            }
            Some((k.try_into().ok()?, c))
        })
        .collect()
}

struct Hit {
    slot: u32,
    coin: OutKey,
    built: Result<Built, String>,
    /// signed and passed every check (keys given)
    checked: bool,
}

pub fn run(sh: Arc<Shared>) {
    let st = sh.store;
    let watch_path = std::env::var("CHAIND_STAKE_WATCH").ok();
    let keys = match std::env::var("CHAIND_STAKE_KEYS") {
        Ok(path) => match Keys::load(sh.params, &path) {
            Ok(k) => Some(k),
            Err(e) => {
                sh.log(&format!("stake: {e}; staking stays off"));
                return;
            }
        },
        Err(_) => None,
    };
    let live = keys.is_some() && std::env::var("CHAIND_STAKE").as_deref() == Ok("1");
    let off_file = std::env::var("CHAIND_STAKE_OFF")
        .unwrap_or_else(|_| format!("{}/stake-off", std::env::var("CHAIND_DIR").unwrap_or_else(|_| ".".into())));
    let mode = if live { "STAKING (hits that pass every check are sent)" } else if keys.is_some() { "signed check (built and signed, nothing sent)" } else { "dry run (built unsigned, nothing signed or sent)" };
    let self_ok = match stakeblock::self_test(sh.params) {
        Ok(()) => {
            sh.log("stake: self test with throwaway keys passed (P2PK and P2PKH coinstakes, scripts, block signature, check before sending)");
            true
        }
        Err(e) => {
            sh.log(&format!("stake: SELF TEST FAILED: {e}; nothing will be sent"));
            false
        }
    };
    let load_watch = || -> HashSet<Vec<u8>> {
        let mut w = watch_path.as_deref().map(read_watch).unwrap_or_default();
        if let Some(k) = &keys {
            w.extend(k.scripts().cloned());
        }
        w
    };
    let mut watch = load_watch();
    let mtime = || watch_path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    let mut watch_mtime = mtime();
    sh.log(&format!("stake: mode {mode}; {} keys, {} watched scripts; stop file {off_file}", keys.as_ref().map_or(0, |k| k.len()), watch.len()));
    let mut coins: Vec<(OutKey, Coin)> = Vec::new();
    let mut known: HashSet<OutKey> = HashSet::new();
    let mut scanned_at = u32::MAX;
    let mut last_slot = 0u32;
    let mut seen_tip = 0u32;
    let mut sent_for: HashSet<u32> = HashSet::new();
    let mut off_said = false;
    let mut hits: HashMap<u32, Vec<Hit>> = HashMap::new();
    let (mut n_hits, mut n_taken, mut n_missed, mut n_lost, mut tried) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut n_same, mut n_identical, mut n_differ, mut n_sent, mut n_refused) = (0u64, 0u64, 0u64, 0u64, 0u64);
    while !sh.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        let m = mtime();
        if m != watch_mtime {
            watch_mtime = m;
            watch = load_watch();
            scanned_at = u32::MAX;
            sh.log(&format!("stake: watch list now {} scripts", watch.len()));
        }
        let tip = sh.flushed.load(Ordering::SeqCst);
        let htip = sh.htip.load(Ordering::SeqCst);
        let synced = tip == sh.ctip.load(Ordering::SeqCst) && tip + 1 >= htip && sh.network_height().is_none_or(|n| tip + 1 >= n);
        if !synced || tip == 0 {
            continue;
        }
        // blocks that came since: the ones our coins staked, were they foreseen, and is ours the same?
        if seen_tip == 0 {
            seen_tip = tip;
        }
        while seen_tip < tip {
            seen_tip += 1;
            let raw = st.block(seen_tip);
            let ours = raw.as_deref().and_then(|raw| Block::parse_with(raw, &sh.params.format).ok()).filter(|b| {
                b.is_proof_of_stake() && known.contains(&crate::store::outkey(&b.txs[1].inputs[0].prevout.txid, b.txs[1].inputs[0].prevout.vout))
            });
            let mine = hits.remove(&seen_tip).unwrap_or_default();
            let was_ours = ours.is_some();
            match ours {
                Some(blk) => {
                    let o = blk.txs[1].inputs[0].prevout;
                    let (t, k) = (blk.header.time, crate::store::outkey(&o.txid, o.vout));
                    match mine.iter().find(|h| h.slot == t && h.coin == k) {
                        Some(h) => {
                            n_taken += 1;
                            match &h.built {
                                Ok(bl) if h.checked && raw.as_deref() == Some(bl.raw.as_slice()) => {
                                    n_identical += 1;
                                    sh.log(&format!("stake: block {seen_tip} ({}:{} at {t}) - predicted, and ours is IDENTICAL byte for byte, signatures too", o.txid.to_hex(), o.vout));
                                }
                                Ok(bl) if bl.header.version == blk.header.version && bl.header.prev == blk.header.prev && bl.header.time == blk.header.time && bl.header.bits == blk.header.bits
                                    && bl.coinbase == blk.txs[0].raw && Some(&bl.coinstake_blank) == stakeblock::blank_script_sig(&blk.txs[1]).as_ref() && blk.txs.len() == 2 => {
                                    n_same += 1;
                                    sh.log(&format!("stake: block {seen_tip} ({}:{} at {t}) - predicted, ours the same apart from the signatures{}", o.txid.to_hex(), o.vout, if h.checked { " (signatures differ!)" } else { "" }));
                                }
                                Ok(_) => {
                                    n_differ += 1;
                                    sh.log(&format!("stake: block {seen_tip} ({}:{} at {t}) - predicted, but ours DIFFERS ({} txs)", o.txid.to_hex(), o.vout, blk.txs.len()));
                                }
                                Err(e) => sh.log(&format!("stake: block {seen_tip} ({}:{} at {t}) - predicted, ours not built: {e}", o.txid.to_hex(), o.vout)),
                            }
                        }
                        None => {
                            n_missed += 1;
                            sh.log(&format!("stake: block {seen_tip} staked by a coin of ours ({}:{} at {t}) - NOT PREDICTED ({} hits for this height)", o.txid.to_hex(), o.vout, mine.len()));
                        }
                    }
                }
                None if !mine.is_empty() => {
                    n_lost += 1;
                    sh.log(&format!("stake: block {seen_tip} went to another staker although we had {} hit(s) (race or late)", mine.len()));
                }
                None => {}
            }
            if seen_tip % 100 == 0 || was_ours {
                sh.log(&format!("stake: totals hits {n_hits}, ours predicted {n_taken}, not predicted {n_missed}, hits lost {n_lost}, identical {n_identical}, same unsigned {n_same}, different {n_differ}, sent {n_sent}, refused by own check {n_refused}, kernels tried {tried}"));
            }
        }
        hits.retain(|h, _| *h > tip);
        sent_for.retain(|h| *h + 100 > tip);
        if scanned_at != tip {
            coins = scan(&sh, &watch);
            known.extend(coins.iter().map(|c| c.0));
            if scanned_at == u32::MAX {
                let total: u64 = coins.iter().map(|c| c.1.value).sum();
                sh.log(&format!("stake: {} coins ({} KOT) at tip {tip}", coins.len(), total as f64 / 1e8));
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
        let tipid = st.id_at(tip).unwrap_or(Hash256::ZERO);
        let version = st.header(tip).map(|h| h.1.version).unwrap_or(0x2000_0000);
        for (k, c) in &coins {
            if height < c.height + MATURITY {
                continue;
            }
            tried += 1;
            let txid = Hash256(k[..32].try_into().unwrap());
            let vout = u32::from_be_bytes(k[32..].try_into().unwrap());
            if !check_kernel(&modifier, bits, c.time, &txid, vout, c.value, slot) {
                continue;
            }
            n_hits += 1;
            let key = keys.as_ref().and_then(|ks| ks.for_script(&c.script));
            let built = stakeblock::build(sh.params, version, tipid, height, slot, bits, &txid, vout, c.value, &c.script, key);
            let checked = match (&built, key) {
                (Ok(b), Some((_, pk))) => match stakeblock::verify(sh.params, &b.raw, tipid, height, slot, bits, Some(&modifier), &txid, vout, c.value, &c.script, c.time, pk) {
                    Ok(_) => true,
                    Err(e) => {
                        n_refused += 1;
                        sh.log(&format!("stake: block {height}: our own check REFUSED it, not sent: {e}"));
                        false
                    }
                },
                _ => false,
            };
            let what = match (&built, checked) {
                (Err(e), _) => format!("not built: {e}"),
                (Ok(_), true) => "built, signed, passed every check".to_string(),
                (Ok(_), false) if key.is_some() => "built but refused".to_string(),
                (Ok(_), false) => "built unsigned to compare".to_string(),
            };
            sh.log(&format!("stake: HIT for block {height} at slot {slot}: coin {}:{vout} ({} KOT) - {what}", txid.to_hex(), c.value as f64 / 1e8));
            if live && checked && self_ok && !sent_for.contains(&height) {
                let still = sh.flushed.load(Ordering::SeqCst) == tip && sh.ctip.load(Ordering::SeqCst) == tip;
                if std::path::Path::new(&off_file).exists() {
                    if !off_said {
                        sh.log(&format!("stake: {off_file} exists: not sending"));
                        off_said = true;
                    }
                } else if still {
                    off_said = false;
                    if let Ok(b) = &built {
                        sent_for.insert(height);
                        n_sent += 1;
                        crate::relay::send_block(&sh, &b.raw);
                        sh.log(&format!("stake: STAKED block {height} ({}), sent to the peers", sh.params.block_id(&b.header).to_hex()));
                    }
                }
            }
            hits.entry(height).or_default().push(Hit { slot, coin: *k, built, checked });
        }
    }
}
