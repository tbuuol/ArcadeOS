//! Core's block rules that need the coins a block spends: the coin rules (`ValueRules`) and the
//! script and block rules (`BlockRules`) of a chain.
//!
//! Adapted from arcade-chaindb/src/rules.rs of Martin Seeger's arcade-v1 (MIT). The checks are his,
//! unchanged; chaind calls [`check_info`] without scripts while the coins move on in order, and runs
//! each transaction's scripts on all cores with [`tx_scripts`] (the same code as the script part of
//! [`check_info`]).

use arcade_chain::params::{ChainParams, SCRIPT_CHECKSEQUENCEVERIFY, SCRIPT_P2SH, SCRIPT_TAPROOT, SCRIPT_WITNESS};
use arcade_chain::sigops;
use arcade_chain::{Block, Hash256, Tx};

/// A coin a block's input spends.
#[derive(Clone, Copy, Debug)]
pub struct Spent<'a> {
    pub value: u64,
    pub script: &'a [u8],
    /// The height of the block that made it.
    pub height: u32,
    /// Made by a coinbase (or a coinstake): it must mature before it is spent.
    pub coinbase: bool,
    /// A Litecoin MWEB peg-out (a HogEx output after the first): it matures too, sooner.
    pub pegout: bool,
}

/// Why a block breaks the rules: the transaction, and Core's reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub tx: Hash256,
    pub why: String,
}

/// ARCADE_VERIFY_ALL_SCRIPTS=1: check scripts under the newest checkpoint too (a full check of
/// a chain's history, as Core with -assumevalid=0).
pub fn verify_all_scripts() -> bool {
    // chaind checks every script from block 1 on, as Core with -assumevalid=0, unless told not to
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("CHAIND_ASSUME_VALID").map_or(true, |v| v != "1"))
}

/// The script flags to check a block at `height` with, or None when its scripts aren't checked:
/// the chain has no block rules, or the block is under the newest checkpoint ("assume valid":
/// V1 lets no fork start under it) and ARCADE_VERIFY_ALL_SCRIPTS is off.
pub fn script_flags(params: &ChainParams, height: u32, all: bool) -> Option<u32> {
    let br = params.block_rules.filter(|b| height > 0 && b.scripts)?;
    (all || params.last_checkpoint(u32::MAX).is_none_or(|cp| height > cp)).then(|| (br.script_flags)(height))
}

/// Size without witness data (Core's SERIALIZE_TRANSACTION_NO_WITNESS): the wire bytes,
/// re-encoded only for a transaction that has a witness.
fn stripped(t: &Tx) -> usize {
    if t.has_witness() { t.encode_legacy().len() } else { t.raw.len() }
}

fn varint_len(n: u64) -> usize {
    match n {
        0..=0xfc => 1,
        0xfd..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// The median time past of the block at a height of the chain being checked (the median of its
/// and the 10 before its times), for the BIP68/BIP113 rules; None when it can't be read.
pub type Mtp<'a> = &'a dyn Fn(u32) -> Option<u32>;

/// Check `blk` at `height` against its chain's rules, given the coins each transaction spends
/// (`spent[i]` for transaction i, in input order; empty for the coinbase), the script flags
/// to run its scripts with (None: scripts not checked), and the chain's median times past.
pub fn check(params: &ChainParams, blk: &Block, height: u32, spent: &[Vec<Spent>], scripts: Option<u32>, mtp: Mtp) -> Result<(), Broken> {
    check_info(params, blk, height, spent, scripts, mtp).map(|_| ())
}

/// The bytes a transaction is checked from: as on the wire, except Litecoin's HogEx, whose MWEB
/// flag libbitcoinconsensus can't read (its inputs carry no witness, so it is the same check).
fn script_bytes(tx: &Tx) -> std::borrow::Cow<'_, [u8]> {
    if tx.hogex { tx.encode_legacy().into() } else { (&tx.raw[..]).into() }
}

/// The chain's params with only its enforced rules: what connecting a block checks (a replay
/// checks them all).
pub fn enforced_only(params: &ChainParams) -> ChainParams {
    let mut p = params.clone();
    p.value_rules = p.value_rules.filter(|v| v.enforced);
    p.block_rules = p.block_rules.filter(|b| b.enforced);
    p
}

/// What [`check_info`] saw of the coinbase: what it pays, and the most it may (the subsidy plus
/// the fees), where the chain has coin rules. A miner taking the whole reward pays exactly the
/// limit: a replay counting those shows the subsidy formula is the chain's own.
#[derive(Debug, Clone, Copy, Default)]
pub struct Info {
    pub coinbase_out: u128,
    pub limit: Option<u128>,
}

/// [`check`], and what it saw of the coinbase.
pub fn check_info(params: &ChainParams, blk: &Block, height: u32, spent: &[Vec<Spent>], scripts: Option<u32>, mtp: Mtp) -> Result<Info, Broken> {
    let vr = params.value_rules.filter(|_| height > 0);
    let br = params.block_rules.filter(|_| height > 0);
    if vr.is_none() && br.is_none() {
        return Ok(Info::default());
    }
    let broke = |tx: &Hash256, why: String| Broken { tx: *tx, why };
    let cb = &blk.txs[0];
    // Core's block flags (GetBlockScriptFlags): whether scripts are run or not, they say which
    // rules are on at this height.
    let on = br.map_or(0, |b| (b.script_flags)(height));
    let (witness_on, csv_on) = (on & SCRIPT_WITNESS != 0, on & SCRIPT_CHECKSEQUENCEVERIFY != 0);
    // BIP113: lock times against the median time past of the block before, once CSV is on.
    let prev_mtp = if csv_on {
        match mtp(height - 1) {
            Some(t) => Some(t),
            None => return Err(broke(&cb.txid, format!("the median time past at {} can't be read here", height - 1))),
        }
    } else {
        None
    };
    // Signature-operation cost: legacy and P2SH count 4, witness ones 1 (only legacy before P2SH).
    let mut cost = 0u64;
    if let Some(br) = &br {
        // Litecoin's MWEB part (after the transactions) counts toward neither size nor weight.
        let extra = if params.format.mweb { 0 } else { blk.extra.len() };
        let around = 80 + blk.auxpow.as_ref().map_or(0, |a| a.raw.len()) + varint_len(blk.txs.len() as u64)
            + blk.sig.as_ref().map_or(0, |g| varint_len(g.len() as u64) + g.len()) + extra;
        let size = around + blk.txs.iter().map(stripped).sum::<usize>();
        if blk.txs.len() > br.max_block_size || size > br.max_block_size {
            return Err(broke(&cb.txid, format!("block is {size} bytes, over {} (bad-blk-length)", br.max_block_size)));
        }
        let weight = size * 3 + around + blk.txs.iter().map(|t| t.raw.len()).sum::<usize>();
        if weight > br.max_block_weight {
            return Err(broke(&cb.txid, format!("block weight {weight}, over {} (bad-blk-weight)", br.max_block_weight)));
        }
        if witness_on {
            if let Err(e) = blk.check_witness_commitment() {
                return Err(broke(&cb.txid, format!("{e} (bad-witness-merkle-match / unexpected-witness)")));
            }
        } else if let Some(t) = blk.txs.iter().find(|t| t.has_witness()) {
            return Err(broke(&t.txid, "witness data before SegWit is on (unexpected-witness)".into()));
        }
        // The version floor is a header rule (check_header); checked here too so a replay proves it.
        let (got, want) = (arcade_chain::params::rule_version(params, blk.header.version), (br.min_version)(height));
        if got < want {
            return Err(broke(&cb.txid, format!("block version {got} is too old at this height (bad-version, at least {want})")));
        }
        if let Some(Err(why)) = br.version_rule.map(|f| f(height, blk.header.version)) {
            return Err(broke(&cb.txid, format!("{why} (bad-version)")));
        }
        if height >= br.bip34_height && !cb.inputs[0].script_sig.starts_with(&sigops::height_push(height)) {
            return Err(broke(&cb.txid, "coinbase doesn't start with the block height (bad-cb-height)".into()));
        }
        for tx in &blk.txs {
            if stripped(tx) > br.max_block_size {
                return Err(broke(&tx.txid, "transaction over the size limit (bad-txns-oversize)".into()));
            }
            if !sigops::is_final(tx, height, prev_mtp.unwrap_or(blk.header.time)) {
                return Err(broke(&tx.txid, format!("not final: lock time {} (bad-txns-nonfinal)", tx.lock_time)));
            }
            cost += 4 * sigops::legacy_sig_ops(tx) as u64;
        }
        if cost > 4 * br.max_sigops as u64 {
            return Err(broke(&cb.txid, format!("signature-operation cost {cost}, over {} (bad-blk-sigops)", 4 * br.max_sigops as u64)));
        }
    }
    let (mut fees, mut coinbase_out) = (0u128, 0u128);
    // Proof of stake (ValueRules::stake_reward): the coinstake's in and out.
    let pos = vr.is_some_and(|v| v.stake_reward.is_some()) && blk.txs.len() > 1 && blk.txs[1].is_coinstake();
    let (mut stake_in, mut stake_out) = (0u128, 0u128);
    for (i, (tx, sp)) in blk.txs.iter().zip(spent).enumerate() {
        let mut value_in = 0u128;
        if let Some(vr) = &vr {
            for s in sp {
                value_in += s.value as u128;
                if s.value > vr.max_money || value_in > vr.max_money as u128 {
                    return Err(broke(&tx.txid, "input values out of range (bad-txns-inputvalues-outofrange)".into()));
                }
                let need = (vr.maturity)(s.height);
                if s.coinbase && height.saturating_sub(s.height) < need {
                    return Err(broke(&tx.txid, format!(
                        "spends a coinbase from height {} before it matures ({need} blocks) (bad-txns-premature-spend-of-coinbase)", s.height)));
                }
                if s.pegout && height.saturating_sub(s.height) < vr.pegout_maturity {
                    return Err(broke(&tx.txid, format!(
                        "spends an MWEB peg-out from height {} before it matures ({} blocks) (bad-txns-premature-spend-of-pegout)", s.height, vr.pegout_maturity)));
                }
            }
        }
        if let (Some(br), false) = (&br, tx.is_coinbase()) {
            // BIP68: relative lock times, against the median time past of the block before.
            if let Some(prev_mtp) = prev_mtp {
                let heights: Vec<u32> = sp.iter().map(|s| s.height).collect();
                let Some((min_h, min_t)) = sigops::sequence_locks(tx, &heights, &mut |h| mtp(h).map(i64::from)) else {
                    return Err(broke(&tx.txid, "a coin's median time past can't be read here".into()));
                };
                if min_h >= height as i64 || min_t >= prev_mtp as i64 {
                    return Err(broke(&tx.txid, "relative lock time not reached (bad-txns-nonfinal, BIP68)".into()));
                }
            }
            // Signature-operation cost: P2SH redeem scripts (GetP2SHSigOpCount) and witness scripts
            // (CountWitnessSigOps), then the scripts.
            let p2sh = on & SCRIPT_P2SH != 0;
            for (inp, s) in tx.inputs.iter().zip(sp) {
                if p2sh && sigops::is_p2sh(s.script) {
                    cost += 4 * sigops::p2sh_sig_op_count(&inp.script_sig) as u64;
                }
                if witness_on {
                    cost += sigops::witness_sig_op_count(&inp.script_sig, s.script, &inp.witness, p2sh) as u64;
                }
            }
            if cost > 4 * br.max_sigops as u64 {
                return Err(broke(&tx.txid, format!("signature-operation cost {cost}, over {} (bad-blk-sigops)", 4 * br.max_sigops as u64)));
            }
            if let Some(flags) = scripts {
                let prefix = (br.sighash_prefix)(height);
                let bytes = script_bytes(tx);
                // Taproot needs every coin the transaction spends.
                let all: Vec<bitcoinconsensus::Utxo> = if flags & SCRIPT_TAPROOT != 0 {
                    sp.iter().map(|s| bitcoinconsensus::Utxo { script_pubkey: s.script.as_ptr(), script_pubkey_len: s.script.len() as u32, value: s.value as i64 }).collect()
                } else {
                    Vec::new()
                };
                let outs = (flags & SCRIPT_TAPROOT != 0).then_some(&all[..]);
                for (i, s) in sp.iter().enumerate() {
                    if let Err(e) = bitcoinconsensus::verify_with_flags_and_sighash_prefix(s.script, s.value, &bytes, outs, i, flags, prefix) {
                        return Err(broke(&tx.txid, format!("input {i}: script check failed ({e:?}) (mandatory-script-verify-flag-failed)")));
                    }
                }
            }
        }
        if let Some(vr) = &vr {
            let mut out = 0u128;
            for o in &tx.outputs {
                out += o.value as u128;
                if o.value > vr.max_money || out > vr.max_money as u128 {
                    return Err(broke(&tx.txid, "output values out of range (bad-txns-vout-toolarge)".into()));
                }
            }
            if tx.is_coinbase() {
                coinbase_out = out;
            } else if pos && i == 1 {
                (stake_in, stake_out) = (value_in, out);
            } else if !tx.is_coinstake() {
                // Litecoin's HogEx too: MWEB fees leave the extension block through it (what
                // it spends, the pool and the peg-ins, less what it pays, the new pool and the
                // peg-outs), and the coinbase may claim them.
                if value_in < out {
                    return Err(broke(&tx.txid, format!("pays out {out} but spends {value_in} (bad-txns-in-belowout)")));
                }
                fees += value_in - out;
                if fees > vr.max_money as u128 {
                    return Err(broke(&tx.txid, "fees out of range (bad-txns-fee-outofrange)".into()));
                }
            }
        }
    }
    let mut info = Info { coinbase_out, limit: None };
    if let Some(vr) = &vr {
        let subsidy = (vr.subsidy)(height, &blk.header.prev);
        if let (true, Some(reward)) = (pos, vr.stake_reward) {
            // Kotia: the staker takes the reward and the fees; the coinbase pays nothing.
            let limit = reward(height) as u128 + fees;
            let got = stake_out.saturating_sub(stake_in);
            if got > limit {
                return Err(broke(&blk.txs[1].txid, format!("coinstake takes {got}, more than the stake reward and fees ({limit}) (bad-cs-amount)")));
            }
            if coinbase_out != 0 {
                return Err(broke(&cb.txid, "a proof-of-stake block's coinbase pays something (bad-cb-amount)".into()));
            }
            info = Info { coinbase_out: got, limit: Some(limit) };
        } else {
            let limit = subsidy as u128 + fees;
            if coinbase_out > limit {
                return Err(broke(&cb.txid, format!("coinbase pays {coinbase_out}, more than the subsidy and fees ({limit}) (bad-cb-amount)")));
            }
            info.limit = Some(limit);
        }
        if let Some(Err(why)) = vr.coinbase_rule.map(|f| f(height, cb, subsidy)) {
            return Err(broke(&cb.txid, why));
        }
    }
    Ok(info)
}

/// The scripts of one transaction (not the coinbase), against the coins it spends: the script part
/// of [`check_info`], for a worker thread.
pub fn tx_scripts(params: &ChainParams, tx: &Tx, sp: &[Spent], height: u32, flags: u32) -> Result<(), Broken> {
    let Some(br) = params.block_rules else { return Ok(()) };
    let prefix = (br.sighash_prefix)(height);
    let bytes = script_bytes(tx);
    let all: Vec<bitcoinconsensus::Utxo> = if flags & SCRIPT_TAPROOT != 0 {
        sp.iter().map(|s| bitcoinconsensus::Utxo { script_pubkey: s.script.as_ptr(), script_pubkey_len: s.script.len() as u32, value: s.value as i64 }).collect()
    } else {
        Vec::new()
    };
    let outs = (flags & SCRIPT_TAPROOT != 0).then_some(&all[..]);
    for (i, s) in sp.iter().enumerate() {
        if let Err(e) = bitcoinconsensus::verify_with_flags_and_sighash_prefix(s.script, s.value, &bytes, outs, i, flags, prefix) {
            return Err(Broken { tx: tx.txid, why: format!("input {i}: script check failed ({e:?}) (mandatory-script-verify-flag-failed)") });
        }
    }
    Ok(())
}
