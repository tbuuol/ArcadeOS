//! Talking to the coin's P2P network: one thread follows the headers (their proof of work checked on all cores),
//! a few threads download blocks from different peers, ahead of the connector but within a byte budget.

use crate::node::{Msg, Shared};
use arcade_chain::p2p::{HeaderEntry, Peer};
use arcade_chain::params::ChainParams;
use arcade_chain::pow::{meets_target, pow_hash, try_pow_hash, PowAlgo};
use arcade_chain::{Block, Hash256, Header};
use crossbeam_channel::Sender;
use rayon::prelude::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// The proof of work of a header (its own scrypt hash, or its AuxPoW's parent): the expensive part of a header
/// check, done here in parallel; the cheap rules that need the chain under it (link, difficulty, time) the
/// connector checks in order.
pub fn pow_ok(params: &ChainParams, id: &Hash256, e: &HeaderEntry) -> Result<(), String> {
    let h = &e.header;
    if params.format.has_auxpow(h) {
        let a = e.auxpow.as_ref().ok_or("AuxPoW flag without data")?;
        let chain_id = params.auxpow_chain_id.unwrap_or(h.chain_id());
        a.check(*id, chain_id, params.auxpow_strict_chain_id).map_err(|x| x.to_string())?;
        if !meets_target(&pow_hash(PowAlgo::Scrypt, &a.parent.encode()), h.bits) {
            return Err("AuxPoW parent misses its target".into());
        }
        return Ok(());
    }
    let algo = params.pow.algo(h.version, u32::MAX).ok_or("unknown mining algorithm")?;
    let Some(algo) = algo else { return Ok(()) };
    match try_pow_hash(algo, &h.encode()) {
        Some(ph) if meets_target(&ph, h.bits) => Ok(()),
        // proof of stake (Kotia): the block decides (kernel, signature), not the header's hash
        Some(_) if matches!(params.pow, arcade_chain::pow::PowRule::PowThenStake { .. }) => Ok(()),
        Some(_) => Err("proof of work misses its target".into()),
        None => Ok(()),
    }
}

fn connect(params: &ChainParams, sh: &Shared, start: usize) -> Option<Peer> {
    let peers = sh.peers();
    for i in 0..peers.len() {
        let a = &peers[(start + i) % peers.len()];
        if sh.stop.load(Ordering::Relaxed) {
            return None;
        }
        match Peer::connect_with(params, a, Duration::from_secs(20), false) {
            Ok(p) => {
                let _ = p.set_read_timeout(Duration::from_secs(60));
                return Some(p);
            }
            Err(e) => sh.log(&format!("peer {a}: {e}")),
        }
    }
    None
}

/// Follow the best chain's headers: ask, check the work, hand them to the connector, ask again.
pub fn header_thread(params: &'static ChainParams, sh: Arc<Shared>, to_node: Sender<Msg>) {
    // all cores: while headers come, the blocks behind them are few and light
    let n = rayon::current_num_threads();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(n).stack_size(16 << 20).thread_name(|i| format!("pow{i}")).build().unwrap();
    let mut start = 0;
    while !sh.stop.load(Ordering::Relaxed) {
        let Some(mut peer) = connect(params, &sh, start) else {
            std::thread::sleep(Duration::from_secs(10));
            continue;
        };
        sh.log(&format!("headers from {} ({}, height {})", peer.addr, peer.version.user_agent, peer.version.start_height));
        loop {
            if sh.stop.load(Ordering::Relaxed) {
                return;
            }
            let loc = sh.locator();
            let entries = match peer.get_headers(&loc) {
                Ok(e) => e,
                Err(e) => {
                    sh.log(&format!("headers from {}: {e}", peer.addr));
                    break;
                }
            };
            if entries.is_empty() {
                // caught up: look again in a while, or as soon as a peer tells of a new block
                for _ in 0..20 {
                    if sh.block_hint.swap(false, Ordering::SeqCst) || sh.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                continue;
            }
            let checked: Result<Vec<(Hash256, HeaderEntry)>, String> = pool.install(|| entries
                .into_par_iter()
                .map(|e| {
                    let id = params.block_id(&e.header);
                    pow_ok(params, &id, &e).map(|_| (id, e)).map_err(|w| format!("header {}: {w}", id.to_hex()))
                })
                .collect());
            let batch = match checked {
                Ok(b) => b,
                Err(e) => {
                    sh.log(&format!("peer {} sent a bad header, leaving it: {e}", peer.addr));
                    break;
                }
            };
            let n = batch.len();
            let (rtx, rrx) = crossbeam_channel::bounded(1);
            if to_node.send(Msg::Headers(batch, rtx)).is_err() {
                return;
            }
            match rrx.recv() {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    sh.log(&format!("headers from {} refused: {e}", peer.addr));
                    break;
                }
                Err(_) => return,
            }
            if n < 2000 {
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        start += 1;
    }
}

/// Download blocks the scheduler hands out, from one peer at a time; on trouble, give the work back and try the next.
pub fn download_thread(params: &'static ChainParams, sh: Arc<Shared>, no: usize, to_node: Sender<Msg>) {
    // first from another chaind directory, as far as it has the blocks of our headers (checked like any block)
    if let Some(imp) = &sh.import {
        while let Some(task) = sh.next_task(64) {
            let mut missed = Vec::new();
            for (height, id) in task {
                match imp.block(height).filter(|raw| raw.len() >= 80 && params.block_id(&Header::parse(raw[..80].try_into().unwrap())) == id) {
                    Some(raw) => {
                        sh.inflight.fetch_add(raw.len() as u64, Ordering::SeqCst);
                        let tx = to_node.clone();
                        rayon::spawn(move || {
                            let r = prep(params, id, &raw);
                            let _ = tx.send(Msg::Block(height, id, r, raw));
                        });
                    }
                    None => missed.push((height, id)),
                }
            }
            if !missed.is_empty() {
                sh.requeue(missed);
                break;
            }
        }
    }
    let mut start = no;
    // blocks per request: far peers answer in one round trip what would otherwise take many
    let batch: u32 = std::env::var("CHAIND_BATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(64);
    while !sh.stop.load(Ordering::Relaxed) {
        let Some(mut peer) = connect(params, &sh, start) else {
            std::thread::sleep(Duration::from_secs(10));
            continue;
        };
        loop {
            let Some(task) = sh.next_task(batch) else { return };
            let ids: Vec<Hash256> = task.iter().map(|t| t.1).collect();
            match peer.get_blocks(&ids) {
                Ok(raws) => {
                    let mut got = vec![false; task.len()];
                    for raw in raws {
                        if raw.len() < 80 {
                            continue;
                        }
                        let id = params.block_id(&Header::parse(raw[..80].try_into().unwrap()));
                        let Some(i) = task.iter().position(|t| t.1 == id) else { continue };
                        got[i] = true;
                        sh.inflight.fetch_add(raw.len() as u64, Ordering::SeqCst);
                        let (height, tx) = (task[i].0, to_node.clone());
                        rayon::spawn(move || {
                            let r = prep(params, id, &raw);
                            let _ = tx.send(Msg::Block(height, id, r, raw));
                        });
                    }
                    let missed: Vec<(u32, Hash256)> = task.iter().zip(&got).filter(|(_, g)| !**g).map(|(t, _)| *t).collect();
                    if !missed.is_empty() {
                        sh.requeue(missed);
                        break;
                    }
                }
                Err(e) => {
                    sh.log(&format!("blocks from {}: {e}", peer.addr));
                    sh.requeue(task);
                    break;
                }
            }
        }
        start += 1;
    }
}

/// What can be checked of a block alone, on any core: it parses, its merkle root, its shape, its AuxPoW.
pub fn prep(params: &ChainParams, id: Hash256, raw: &[u8]) -> Result<Arc<Block>, String> {
    let b = Block::parse_with(raw, &params.format).map_err(|e| format!("does not parse: {e}"))?;
    if b.id != id {
        return Err("id differs".into());
    }
    arcade_chain::params::check_block_body(params, &b).map_err(|e| e.to_string())?;
    if let Some(a) = &b.auxpow {
        let chain_id = params.auxpow_chain_id.unwrap_or(b.header.chain_id());
        a.check(id, chain_id, params.auxpow_strict_chain_id).map_err(|x| x.to_string())?;
        if !meets_target(&pow_hash(PowAlgo::Scrypt, &a.parent.encode()), b.header.bits) {
            return Err("AuxPoW parent misses its target".into());
        }
    }
    Ok(Arc::new(b))
}
