//! The connector: the one thread that changes the chain. Headers come in checked for work (net.rs) and are
//! put on the best chain here (link, difficulty, time, checkpoints, reorgs); blocks come in parsed and
//! checked alone (net.rs prep) and are connected here strictly in order: the coins they spend are looked up,
//! the coin and block rules checked, the coins moved. Their scripts go to all cores meanwhile; the coins are
//! written to LMDB only up to blocks whose every script has passed.

use crate::rules::{self, Spent};
use crate::store::{self, decode_undo, encode_undo, outkey, txi_key, Coin, OutKey, Store};
use arcade_chain::p2p::HeaderEntry;
use arcade_chain::params::{check_auxpow_rules, check_context, rule_version, too_easy, ChainParams, PowCheck, MEDIAN_TIME_SPAN};
use arcade_chain::pow::{target_from_bits, U256};
use arcade_chain::{Block, Hash256, Header};
use crossbeam_channel::{Receiver, Sender};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub enum Msg {
    Headers(Vec<(Hash256, HeaderEntry)>, Sender<Result<u32, String>>),
    Block(u32, Hash256, Result<Arc<Block>, String>, Vec<u8>),
    Scripts(u32, Result<(), rules::Broken>),
    /// write the coins out now (for a consistent read of the whole set); answered when done
    Flush(Sender<u32>),
}

/// Settings that bound the memory the pipeline may use.
pub struct Budget {
    /// downloaded blocks not yet connected
    pub inflight_bytes: u64,
    /// coins changed since the last write to LMDB
    pub cache_bytes: usize,
    /// blocks connected whose scripts are still running
    pub script_window: u32,
}

struct Sched {
    next: u32,
    requeue: VecDeque<Vec<(u32, Hash256)>>,
}

pub struct Shared {
    pub store: &'static Store,
    pub params: &'static ChainParams,
    /// best header, connected block, block whose scripts (and all below) passed, coins written up to
    pub htip: AtomicU32,
    pub ctip: AtomicU32,
    pub vtip: AtomicU32,
    pub flushed: AtomicU32,
    pub stop: AtomicBool,
    pub inflight: AtomicU64,
    pub budget: Budget,
    sched: Mutex<Sched>,
    cv: Condvar,
    peers: Mutex<Vec<String>>,
    pub started: Instant,
    pub mempool: Mutex<crate::mempool::Mempool>,
    pub relay: Mutex<Vec<crate::relay::Writer>>,
    /// a peer told of a new block: look for headers now
    pub block_hint: AtomicBool,
    pub wallet: Mutex<Option<crate::wallet::Wallet>>,
    /// bytes held by blocks whose scripts are running (their spent coins)
    pub script_bytes: AtomicU64,
    /// blocks of another chaind directory, used before the network (CHAIND_IMPORT)
    pub import: Option<crate::store::Import>,
}

impl Shared {
    pub fn new(store: &'static Store, params: &'static ChainParams, budget: Budget, peers: Vec<String>) -> Shared {
        let tip = store.get_meta_u32("tip").unwrap_or(0);
        let htip = store.get_meta_u32("htip").unwrap_or(0);
        store.load_ids(htip);
        Shared {
            store,
            params,
            htip: htip.into(),
            ctip: tip.into(),
            vtip: tip.into(),
            flushed: tip.into(),
            stop: false.into(),
            inflight: 0.into(),
            budget,
            sched: Mutex::new(Sched { next: tip + 1, requeue: VecDeque::new() }),
            cv: Condvar::new(),
            peers: Mutex::new(peers),
            started: Instant::now(),
            mempool: Mutex::new(Default::default()),
            relay: Mutex::new(Vec::new()),
            block_hint: false.into(),
            wallet: Mutex::new(None),
            script_bytes: 0.into(),
            import: std::env::var("CHAIND_IMPORT").ok().and_then(|d| crate::store::Import::open(std::path::Path::new(&d)).ok()),
        }
    }

    pub fn log(&self, s: &str) {
        eprintln!("[{:>7.1}s] {s}", self.started.elapsed().as_secs_f64());
    }

    pub fn peers(&self) -> Vec<String> {
        self.peers.lock().unwrap().clone()
    }

    /// Block locator from the best header: the last ten, then doubling steps, then genesis.
    pub fn locator(&self) -> Vec<Hash256> {
        let mut h = self.htip.load(Ordering::SeqCst) as i64;
        let (mut v, mut step) = (Vec::new(), 1i64);
        while h > 0 {
            if let Some(id) = self.store.id_at(h as u32) {
                v.push(id);
            }
            if v.len() >= 10 {
                step *= 2;
            }
            h -= step;
        }
        v.push(self.params.genesis_hash());
        v
    }

    /// The next blocks to download: within the headers, at most so far ahead of the connected tip, and only
    /// while the downloaded-but-not-connected bytes stay under budget. None when stopping.
    pub fn next_task(&self, n: u32) -> Option<Vec<(u32, Hash256)>> {
        let mut s = self.sched.lock().unwrap();
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(t) = s.requeue.pop_front() {
                return Some(t);
            }
            let ctip = self.ctip.load(Ordering::SeqCst);
            s.next = s.next.max(ctip + 1);
            let limit = self.htip.load(Ordering::SeqCst).min(ctip + 4000);
            if s.next <= limit && self.inflight.load(Ordering::SeqCst) < self.budget.inflight_bytes {
                let end = (s.next + n - 1).min(limit);
                let t: Vec<(u32, Hash256)> = (s.next..=end).filter_map(|h| self.store.id_at(h).map(|id| (h, id))).collect();
                s.next = end + 1;
                if !t.is_empty() {
                    return Some(t);
                }
            }
            s = self.cv.wait_timeout(s, Duration::from_millis(500)).unwrap().0;
        }
    }

    pub fn requeue(&self, t: Vec<(u32, Hash256)>) {
        let ctip = self.ctip.load(Ordering::SeqCst);
        let t: Vec<_> = t.into_iter().filter(|x| x.0 > ctip).collect();
        if !t.is_empty() {
            self.sched.lock().unwrap().requeue.push_back(t);
        }
        self.cv.notify_all();
    }

    /// The best chain changed from `from` up: what was scheduled there is for the old chain.
    fn reset_from(&self, from: u32) {
        let mut s = self.sched.lock().unwrap();
        s.next = s.next.min(from);
        for t in s.requeue.iter_mut() {
            t.retain(|x| x.0 < from);
        }
        s.requeue.retain(|t| !t.is_empty());
        self.cv.notify_all();
    }

    pub fn wake(&self) {
        self.cv.notify_all();
    }
}

/// A coin a connected-but-unchecked block spends, owned (its scripts run on another thread).
#[derive(Clone)]
struct SpentCoin {
    value: u64,
    script: Vec<u8>,
    height: u32,
    coinbase: bool,
    pegout: bool,
}

struct Entry {
    coin: Option<Coin>,
    /// not in LMDB (made since the last write): spending it needs no delete there
    fresh: bool,
}

pub fn is_unspendable(p: &ChainParams, script: &[u8]) -> bool {
    // as Core (CScript::IsUnspendable): OP_RETURN outputs never enter the coin set; from 0.15-based nodes on
    // (here: the SegWit chains) neither do scripts over MAX_SCRIPT_SIZE
    script.first() == Some(&0x6a) || (p.format.segwit && script.len() > 10_000)
}

pub struct Connector {
    sh: Arc<Shared>,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    pending: BTreeMap<u32, (Hash256, Arc<Block>, Vec<u8>)>,
    retries: HashMap<u32, u32>,
    cache: HashMap<OutKey, Entry>,
    cache_bytes: usize,
    txi_put: Vec<([u8; 14], u8, [u8; 8], u8)>,
    done: std::collections::BTreeSet<u32>,
    draining: bool,
    rtxn: Option<heed::RoTxn<'static>>,
    last_log: Instant,
    last_log_ctip: u32,
    pub scripts_checked: Arc<AtomicU64>,
    inputs_seen: u64,
    want_flush: Vec<Sender<u32>>,
    /// when the connected tip last moved (to notice a block that never comes)
    moved_at: Instant,
    moved_tip: u32,
    stalls: u64,
    script_sizes: HashMap<u32, u64>,
}

fn header_rules(p: &ChainParams, h: &Header, parent: &Header, height: u32, below: &[Header]) -> Result<(), String> {
    if h.prev != p.block_id(parent) {
        return Err("does not build on its parent".into());
    }
    if let Some(cp) = p.checkpoint_at(height) {
        if p.block_id(h) != cp {
            return Err("not the checkpoint's block".into());
        }
    }
    match target_from_bits(h.bits) {
        Some(t) if t != U256::ZERO => {}
        _ => return Err("bad bits".into()),
    }
    check_auxpow_rules(p, h, height).map_err(|e| e.to_string())?;
    if let Some(br) = p.block_rules.filter(|b| b.enforced) {
        let (got, want) = (rule_version(p, h.version), (br.min_version)(height));
        if got < want {
            return Err(format!("block version {got} too old (bad-version, at least {want})"));
        }
        if let Some(Err(why)) = br.version_rule.map(|f| f(height, h.version)) {
            return Err(why);
        }
    }
    p.pow.algo(h.version, height).ok_or("unknown mining algorithm")?;
    if p.max_easier && too_easy(p, h.bits, parent.bits) {
        return Err("difficulty drops too far".into());
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    check_context(p, h, height, below, now).map_err(|e| e.to_string())
}

/// Where each transaction of a raw block starts and how long it is (for the tx index).
fn tx_spans(b: &Block) -> Vec<(u32, u32)> {
    let mut off = 80 + b.auxpow.as_ref().map_or(0, |a| a.raw.len());
    let mut v = Vec::new();
    arcade_chain::encode::write_varint(&mut v, b.txs.len() as u64);
    off += v.len();
    b.txs
        .iter()
        .map(|t| {
            let s = (off as u32, t.raw.len() as u32);
            off += t.raw.len();
            s
        })
        .collect()
}

impl Connector {
    pub fn new(sh: Arc<Shared>, rx: Receiver<Msg>, tx: Sender<Msg>) -> Connector {
        Connector {
            sh,
            rx,
            tx,
            pending: BTreeMap::new(),
            retries: HashMap::new(),
            cache: HashMap::new(),
            cache_bytes: 0,
            txi_put: Vec::new(),
            done: Default::default(),
            draining: false,
            rtxn: None,
            last_log: Instant::now(),
            last_log_ctip: 0,
            scripts_checked: Arc::new(AtomicU64::new(0)),
            inputs_seen: 0,
            want_flush: Vec::new(),
            moved_at: Instant::now(),
            moved_tip: 0,
            stalls: 0,
            script_sizes: HashMap::new(),
        }
    }

    fn store(&self) -> &'static Store {
        self.sh.store
    }

    /// Genesis on an empty store: its header at height 0 (its coins never count, as in Core).
    pub fn init(&mut self) {
        let st = self.store();
        if st.id_at(0).is_none() {
            let g = self.sh.params.genesis_header().expect("genesis header");
            st.put_header(0, &self.sh.params.genesis_hash(), &g);
            let mut w = st.env.write_txn().unwrap();
            st.meta.put(&mut w, b"htip", &0u32.to_le_bytes()).unwrap();
            st.meta.put(&mut w, b"tip", &0u32.to_le_bytes()).unwrap();
            w.commit().unwrap();
            st.sync_files();
        }
    }

    /// The headers under `height`, parent first (for the time and difficulty rules).
    fn below(&self, height: u32) -> Vec<Header> {
        let st = self.store();
        (0..MEDIAN_TIME_SPAN as u32).take_while(|i| *i < height).filter_map(|i| st.header(height - 1 - i).map(|x| x.1)).collect()
    }

    fn work_of(&self, bits: u32) -> U256 {
        self.sh.params.header_work(bits, PowCheck::Verified)
    }

    /// Put a batch of (work-checked) headers on the best chain; returns the new best height.
    fn apply_headers(&mut self, batch: Vec<(Hash256, HeaderEntry)>) -> Result<u32, String> {
        let st = self.store();
        let htip = self.sh.htip.load(Ordering::SeqCst);
        let first = &batch[0].1.header;
        let fork = if st.id_at(htip) == Some(first.prev) {
            htip
        } else {
            st.height_of(&first.prev).ok_or("the headers do not connect to our chain")?
        };
        // skip what we already have on the best chain
        let mut skip = 0;
        while skip < batch.len() && st.id_at(fork + 1 + skip as u32) == Some(batch[skip].0) {
            skip += 1;
        }
        let batch = &batch[skip..];
        if batch.is_empty() {
            return Ok(htip);
        }
        let fork = fork + skip as u32;
        if fork < htip {
            // a competing branch: only if it has more work than ours above the fork
            let ours = (fork + 1..=htip).filter_map(|h| st.header(h)).fold(U256::ZERO, |a, (_, h)| a.checked_add(self.work_of(h.bits)).unwrap_or(U256::MAX));
            let theirs = batch.iter().fold(U256::ZERO, |a, (_, e)| a.checked_add(self.work_of(e.header.bits)).unwrap_or(U256::MAX));
            if theirs <= ours {
                return Ok(htip);
            }
            if let Some(cp) = self.sh.params.last_checkpoint(htip).filter(|cp| fork < *cp) {
                return Err(format!("forks below the checkpoint at {cp}"));
            }
            self.sh.log(&format!("reorg: a branch from height {fork} with more work replaces {} headers", htip - fork));
            if fork < self.sh.ctip.load(Ordering::SeqCst) {
                self.disconnect_to(fork);
            }
            self.sh.reset_from(fork + 1);
        }
        // check them in order against the chain under them
        let mut below = self.below(fork + 1);
        let mut parent = st.header(fork).ok_or("parent header missing")?.1;
        let mut w = st.env.write_txn().map_err(|e| e.to_string())?;
        // the old branch's ids above the fork leave the index
        for h in fork + 1..=htip {
            if let Some(id) = st.id_at(h) {
                st.unindex_id(&id);
            }
        }
        let mut height = fork;
        for (id, e) in batch {
            height += 1;
            if let Err(why) = header_rules(self.sh.params, &e.header, &parent, height, &below) {
                // keep what came before it
                height -= 1;
                if height > fork {
                    self.finish_headers(w, height, fork, htip);
                }
                return Err(format!("header {height}+1 {}: {why}", id.to_hex()));
            }
            st.put_header(height, id, &e.header);
            st.index_id(id, height);
            below.insert(0, e.header);
            below.truncate(MEDIAN_TIME_SPAN);
            parent = e.header;
        }
        self.finish_headers(w, height, fork, htip);
        Ok(height)
    }

    fn finish_headers(&self, mut w: heed::RwTxn, height: u32, fork: u32, old: u32) {
        let st = self.store();
        st.sync_files();
        st.meta.put(&mut w, b"htip", &height.to_le_bytes()).unwrap();
        w.commit().unwrap();
        self.sh.htip.store(height, Ordering::SeqCst);
        if height < old && fork < old {
            self.sh.reset_from(fork + 1);
        }
        self.sh.wake();
    }

    fn coin(&mut self, k: &OutKey) -> Option<Coin> {
        if let Some(e) = self.cache.get(k) {
            return e.coin.clone();
        }
        let st = self.store();
        if self.rtxn.is_none() {
            self.rtxn = Some(st.env.read_txn().unwrap());
        }
        let r = self.rtxn.as_ref().unwrap();
        st.coins.get(r, k).ok().flatten().and_then(Coin::decode)
    }

    fn cache_put(&mut self, k: OutKey, e: Entry) {
        self.cache_bytes += 36 + 64 + e.coin.as_ref().map_or(0, |c| c.script.len());
        self.cache.insert(k, e);
    }

    /// Connect the next block: its coins, its coin and block rules; its scripts go to the workers.
    fn connect(&mut self, height: u32, id: Hash256, b: Arc<Block>, raw: Vec<u8>) -> Result<(), String> {
        let p = self.sh.params;
        let bip34 = p.block_rules.map_or(0, |r| r.bip34_height);
        let mut spent: Vec<Vec<SpentCoin>> = Vec::with_capacity(b.txs.len());
        let mut undo: Vec<(OutKey, Coin)> = Vec::new();
        let mut spent_keys: Vec<OutKey> = Vec::new();
        let mut created_mine: Vec<(OutKey, Coin)> = Vec::new();
        let wallet_on = self.sh.wallet.lock().unwrap().is_some();
        for tx in &b.txs {
            let mut sp = Vec::with_capacity(tx.inputs.len());
            if !tx.is_coinbase() {
                for inp in &tx.inputs {
                    let k = outkey(&inp.prevout.txid, inp.prevout.vout);
                    let c = self.coin(&k).ok_or_else(|| format!("tx {} spends a coin that does not exist or is spent (bad-txns-inputs-missingorspent)", tx.txid.to_hex()))?;
                    match self.cache.get(&k) {
                        Some(e) if e.fresh => {
                            self.cache.remove(&k);
                        }
                        _ => self.cache_put(k, Entry { coin: None, fresh: false }),
                    }
                    if wallet_on {
                        spent_keys.push(k);
                    }
                    sp.push(SpentCoin { value: c.value, script: c.script.clone(), height: c.height, coinbase: c.coinbase, pegout: c.pegout });
                    if c.height != height {
                        undo.push((k, c));
                    }
                }
            }
            // BIP30: before BIP34 no transaction may overwrite one with unspent outputs
            if height < bip34 {
                for v in 0..tx.outputs.len() as u32 {
                    if self.coin(&outkey(&tx.txid, v)).is_some() {
                        return Err(format!("tx {} overwrites unspent outputs (bad-txns-BIP30)", tx.txid.to_hex()));
                    }
                }
            }
            for (v, o) in tx.outputs.iter().enumerate() {
                if is_unspendable(p, &o.script_pubkey) {
                    continue;
                }
                let k = outkey(&tx.txid, v as u32);
                let fresh = !self.cache.contains_key(&k);
                let coin = Coin { value: o.value, height, coinbase: tx.is_coinbase() || tx.is_coinstake(), script: o.script_pubkey.clone(), pegout: tx.hogex && v > 0 };
                if wallet_on && self.sh.wallet.lock().unwrap().as_ref().is_some_and(|w| w.mine(&coin.script)) {
                    created_mine.push((k, coin.clone()));
                }
                self.cache_put(k, Entry { coin: Some(coin), fresh });
            }
            spent.push(sp);
        }
        {
            let refs: Vec<Vec<Spent>> = spent
                .iter()
                .map(|v| v.iter().map(|s| Spent { value: s.value, script: &s.script, height: s.height, coinbase: s.coinbase, pegout: s.pegout }).collect())
                .collect();
            let st = self.store();
            let mtp = |h: u32| -> Option<u32> {
                let mut t: Vec<u32> = (0..11u32).take_while(|i| *i <= h).filter_map(|i| st.header(h - i).map(|x| x.1.time)).collect();
                t.sort_unstable();
                t.get(t.len() / 2).copied()
            };
            rules::check_info(p, &b, height, &refs, None, &mtp).map_err(|e| format!("tx {}: {}", e.tx.to_hex(), e.why))?;
        }
        // the mempool loses what this block confirms or makes void; the wallet follows its coins
        self.sh.mempool.lock().unwrap().block_connected(&b.txs);
        if let Some(w) = self.sh.wallet.lock().unwrap().as_mut() {
            w.block(&spent_keys, created_mine);
        }
        let raw_len = raw.len() as u64;
        let st = self.store();
        st.put_block(height, &raw);
        st.put_undo(height, &encode_undo(&undo));
        for (i, ((off, len), tx)) in tx_spans(&b).into_iter().zip(&b.txs).enumerate() {
            let mut v = [0u8; 8];
            v[..4].copy_from_slice(&off.to_le_bytes());
            v[4..].copy_from_slice(&len.to_le_bytes());
            let (k, kl) = txi_key(&tx.txid, height, i as u16);
            self.txi_put.push((k, kl as u8, v, if store::fmt() >= 2 { 0 } else { 8 }));
        }
        self.cache_bytes += b.txs.len() * 40;
        self.inputs_seen += spent.iter().map(|s| s.len() as u64).sum::<u64>();
        self.sh.ctip.store(height, Ordering::SeqCst);
        let _ = id;
        match rules::script_flags(p, height, rules::verify_all_scripts()) {
            Some(flags) => {
                let bytes: u64 = spent.iter().flatten().map(|s| s.script.len() as u64 + 48).sum::<u64>() + raw_len;
                self.script_sizes.insert(height, bytes);
                self.sh.script_bytes.fetch_add(bytes, Ordering::SeqCst);
                let (tx, counter) = (self.tx.clone(), self.scripts_checked.clone());
                rayon::spawn(move || {
                    let r = b.txs.par_iter().zip(spent.par_iter()).skip(1).try_for_each(|(t, sp)| {
                        let refs: Vec<Spent> = sp.iter().map(|s| Spent { value: s.value, script: &s.script, height: s.height, coinbase: s.coinbase, pegout: s.pegout }).collect();
                        counter.fetch_add(refs.len() as u64, Ordering::Relaxed);
                        rules::tx_scripts(p, t, &refs, height, flags)
                    });
                    let _ = tx.send(Msg::Scripts(height, r));
                });
            }
            None => {
                self.done.insert(height);
            }
        }
        Ok(())
    }

    fn advance_verified(&mut self) {
        let mut v = self.sh.vtip.load(Ordering::SeqCst);
        while self.done.remove(&(v + 1)) {
            v += 1;
        }
        self.sh.vtip.store(v, Ordering::SeqCst);
    }

    /// Write the coins, the tx index and the tip to LMDB (after the block files are on disk).
    fn flush(&mut self) {
        let st = self.store();
        let t0 = Instant::now();
        self.rtxn = None;
        st.sync_files();
        let ctip = self.sh.ctip.load(Ordering::SeqCst);
        let mut w = st.env.write_txn().unwrap();
        let mut entries: Vec<(OutKey, Entry)> = self.cache.drain().collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let (mut puts, mut dels) = (0, 0);
        for (k, e) in &entries {
            match &e.coin {
                Some(c) => {
                    st.coins.put(&mut w, k, &c.encode()).unwrap();
                    puts += 1;
                }
                None if !e.fresh => {
                    let _ = st.coins.delete(&mut w, k);
                    dels += 1;
                }
                None => {}
            }
        }
        self.txi_put.sort_unstable();
        for (k, kl, v, vl) in &self.txi_put {
            st.txi.put(&mut w, &k[..*kl as usize], &v[..*vl as usize]).unwrap();
        }
        let ntx = self.txi_put.len();
        self.txi_put.clear();
        self.txi_put.shrink_to(1 << 16);
        st.meta.put(&mut w, b"tip", &ctip.to_le_bytes()).unwrap();
        w.commit().unwrap();
        self.cache = HashMap::new();
        self.cache_bytes = 0;
        self.sh.flushed.store(ctip, Ordering::SeqCst);
        if t0.elapsed() > Duration::from_secs(1) {
            self.sh.log(&format!("wrote coins at {ctip}: {puts} new, {dels} spent, {ntx} txs indexed, in {:.1}s", t0.elapsed().as_secs_f64()));
        }
        for s in self.want_flush.drain(..) {
            let _ = s.send(ctip);
        }
    }

    /// Take blocks back down to `to` (a reorg below the connected tip): with what LMDB holds, so first
    /// wait for the scripts and write everything out.
    fn disconnect_to(&mut self, to: u32) {
        self.wait_scripts();
        self.flush();
        let st = self.store();
        let p = self.sh.params;
        let mut h = self.sh.ctip.load(Ordering::SeqCst);
        while h > to {
            let raw = st.block(h).expect("stored block");
            let b = Block::parse_with(&raw, &p.format).expect("stored block parses");
            let undo = decode_undo(&st.undo(h).expect("undo")).expect("undo parses");
            let mut w = st.env.write_txn().unwrap();
            for (i, tx) in b.txs.iter().enumerate().rev() {
                for v in 0..tx.outputs.len() as u32 {
                    let _ = st.coins.delete(&mut w, &outkey(&tx.txid, v));
                }
                let (k, kl) = txi_key(&tx.txid, h, i as u16);
                let _ = st.txi.delete(&mut w, &k[..kl]);
            }
            for (k, c) in &undo {
                st.coins.put(&mut w, k, &c.encode()).unwrap();
            }
            h -= 1;
            st.meta.put(&mut w, b"tip", &h.to_le_bytes()).unwrap();
            w.commit().unwrap();
        }
        for a in [&self.sh.ctip, &self.sh.vtip, &self.sh.flushed] {
            a.store(to, Ordering::SeqCst);
        }
        let drop: Vec<u32> = self.pending.keys().copied().collect();
        for k in drop {
            if let Some((_, _, raw)) = self.pending.remove(&k) {
                self.sh.inflight.fetch_sub(raw.len() as u64, Ordering::SeqCst);
            }
        }
        self.sh.mempool.lock().unwrap().clear();
        if let Some(w) = self.sh.wallet.lock().unwrap().as_mut() {
            let _ = w.rescan(st);
        }
        self.sh.log(&format!("took blocks back to {to}"));
    }

    fn wait_scripts(&mut self) {
        while self.sh.vtip.load(Ordering::SeqCst) < self.sh.ctip.load(Ordering::SeqCst) {
            match self.rx.recv_timeout(Duration::from_secs(1)) {
                Ok(Msg::Scripts(h, r)) => {
                    if let Err(e) = r {
                        self.fatal(h, &format!("tx {}: {}", e.tx.to_hex(), e.why));
                    }
                    self.done.insert(h);
                    if let Some(b) = self.script_sizes.remove(&h) {
                        self.sh.script_bytes.fetch_sub(b, Ordering::SeqCst);
                    }
                    self.advance_verified();
                }
                Ok(Msg::Block(h, id, Ok(b), raw)) => self.take_block(h, id, b, raw),
                Ok(Msg::Block(h, _, Err(_), raw)) => {
                    self.sh.inflight.fetch_sub(raw.len() as u64, Ordering::SeqCst);
                    let _ = h;
                }
                Ok(Msg::Headers(_, r)) => {
                    let _ = r.send(Err("busy, try again".into()));
                }
                Ok(Msg::Flush(s)) => self.want_flush.push(s),
                Err(_) => {}
            }
        }
    }

    fn fatal(&self, height: u32, why: &str) -> ! {
        self.sh.log(&format!("BLOCK {height} BREAKS THE RULES: {why}"));
        self.sh.log("stopping: the coins on disk are those of the last block that passed; nothing after it was written");
        self.sh.stop.store(true, Ordering::SeqCst);
        std::process::exit(2);
    }

    fn take_block(&mut self, h: u32, id: Hash256, b: Arc<Block>, raw: Vec<u8>) {
        if h <= self.sh.ctip.load(Ordering::SeqCst) || self.store().id_at(h) != Some(id) {
            self.sh.inflight.fetch_sub(raw.len() as u64, Ordering::SeqCst);
            return;
        }
        if let Some(old) = self.pending.insert(h, (id, b, raw)) {
            self.sh.inflight.fetch_sub(old.2.len() as u64, Ordering::SeqCst);
        }
    }

    pub fn run(mut self) {
        loop {
            if self.sh.stop.load(Ordering::Relaxed) {
                self.wait_scripts();
                self.flush();
                self.sh.log(&format!("stopped at {}", self.sh.ctip.load(Ordering::SeqCst)));
                return;
            }
            // connect what is next, while the script window and the coin cache allow
            loop {
                let (ctip, vtip) = (self.sh.ctip.load(Ordering::SeqCst), self.sh.vtip.load(Ordering::SeqCst));
                let scripts_full = ctip > vtip && self.sh.script_bytes.load(Ordering::SeqCst) as usize >= self.sh.budget.cache_bytes / 2;
                if self.draining || ctip - vtip >= self.sh.budget.script_window || scripts_full || self.cache_bytes >= self.sh.budget.cache_bytes {
                    break;
                }
                let Some((id, b, raw)) = self.pending.remove(&(ctip + 1)) else { break };
                let len = raw.len() as u64;
                if let Err(why) = self.connect(ctip + 1, id, b, raw) {
                    self.fatal(ctip + 1, &why);
                }
                self.sh.inflight.fetch_sub(len, Ordering::SeqCst);
                self.sh.wake();
            }
            let (ctip, vtip, htip, flushed) = (
                self.sh.ctip.load(Ordering::SeqCst),
                self.sh.vtip.load(Ordering::SeqCst),
                self.sh.htip.load(Ordering::SeqCst),
                self.sh.flushed.load(Ordering::SeqCst),
            );
            let want = self.cache_bytes >= self.sh.budget.cache_bytes
                || ctip >= flushed + 100_000
                || (ctip == htip && ctip > flushed)
                || !self.want_flush.is_empty();
            // stop connecting while the scripts catch up for a write; if the reason went away meanwhile (the headers
            // moved on), go on connecting
            self.draining = want;
            if want && vtip == ctip {
                self.flush();
                self.draining = false;
            }
            // a block that never comes (lost by a peer, or its answer went astray): ask for it again
            if ctip != self.moved_tip {
                self.moved_tip = ctip;
                self.moved_at = Instant::now();
            } else if !self.pending.is_empty() && !self.draining && ctip == vtip && self.moved_at.elapsed() > Duration::from_secs(15) {
                let first = *self.pending.keys().next().unwrap();
                let st = self.store();
                let gap: Vec<(u32, Hash256)> = (ctip + 1..first.min(ctip + 65)).filter_map(|h| st.id_at(h).map(|id| (h, id))).collect();
                if !gap.is_empty() {
                    self.stalls += 1;
                    self.sh.log(&format!("blocks {}..{} did not come in 15 s, asking again (stall {})", gap[0].0, gap[gap.len() - 1].0, self.stalls));
                    self.sh.requeue(gap);
                }
                self.moved_at = Instant::now();
            }
            // every 15 s while catching up, every 5 minutes at the tip
            let every = if ctip + 6 >= htip && flushed == ctip { 300 } else { 15 };
            if self.last_log.elapsed() >= Duration::from_secs(every) {
                let secs = self.last_log.elapsed().as_secs_f64();
                self.sh.log(&format!(
                    "headers {htip} | connected {ctip} ({:.0}/s) | scripts ok {vtip} | on disk {flushed} | cache {} MB | waiting {} blocks ({} MB) | inputs {} checked {} | {}",
                    (ctip - self.last_log_ctip) as f64 / secs,
                    self.cache_bytes >> 20,
                    self.pending.len(),
                    self.sh.inflight.load(Ordering::SeqCst) >> 20,
                    self.inputs_seen,
                    self.scripts_checked.load(Ordering::Relaxed),
                    crate::mem()
                ));
                self.last_log = Instant::now();
                self.last_log_ctip = ctip;
            }
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Msg::Headers(batch, reply)) => {
                    let r = self.apply_headers(batch);
                    let _ = reply.send(r);
                }
                Ok(Msg::Block(h, id, Ok(b), raw)) => self.take_block(h, id, b, raw),
                Ok(Msg::Block(h, id, Err(why), raw)) => {
                    self.sh.inflight.fetch_sub(raw.len() as u64, Ordering::SeqCst);
                    let n = self.retries.entry(h).or_insert(0);
                    *n += 1;
                    self.sh.log(&format!("block {h} from a peer is unusable ({why}), asking again"));
                    if *n > 5 {
                        self.fatal(h, &why);
                    }
                    self.sh.requeue(vec![(h, id)]);
                }
                Ok(Msg::Scripts(h, r)) => {
                    if let Err(e) = r {
                        self.fatal(h, &format!("tx {}: {}", e.tx.to_hex(), e.why));
                    }
                    self.done.insert(h);
                    if let Some(b) = self.script_sizes.remove(&h) {
                        self.sh.script_bytes.fetch_sub(b, Ordering::SeqCst);
                    }
                    self.advance_verified();
                }
                Ok(Msg::Flush(s)) => self.want_flush.push(s),
                Err(_) => {}
            }
        }
    }
}

