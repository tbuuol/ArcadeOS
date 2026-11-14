//! Kotia's own block rules (Kotia Core v2.4 / v1.0 network, Blackcoin kernel v3), on top of the coin and block
//! rules every chain has: proof of work up to block 1,000,000 and proof of stake after (also mixed in before),
//! the stake modifier carried block by block, the kernel of a staked block, the difficulty of both kinds,
//! the block signature, and the time stamps. The kernel math is Martin Seeger's port (arcade_chain::kotia_pos).

use crate::store::Store;
use arcade_chain::kotia_pos::{self, check_kernel, next_modifier, STAKE_TIMESTAMP_MASK};
use arcade_chain::params::ChainParams;
use arcade_chain::pow::bits_from_target;
use arcade_chain::{Block, Hash256};
use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{Signature, VerifyingKey};

/// nProtocolV2Time / nProtocolV3Time of Kotia mainnet
pub const V2_TIME: u32 = 1_635_954_625;
pub const V3_TIME: u32 = 1_635_954_626;
pub const LAST_POW_BLOCK: u32 = 1_000_000;
/// FutureDrift: a coinbase may be at most 15 s younger than its block
const FUTURE_DRIFT: u32 = 15;

/// Script flags of a Kotia block (ConnectBlock): P2SH, DERSIG, Kotia's DERKEY, LOW_S, and from protocol v3
/// CHECKLOCKTIMEVERIFY and NULLDUMMY.
pub fn script_flags(block_time: u32) -> u32 {
    let mut f = 1 | (1 << 2) | (1 << 3) | (1 << 29);
    if block_time >= V3_TIME {
        f |= (1 << 9) | (1 << 4);
    }
    f
}

#[derive(Clone, Copy, Debug)]
struct Seen {
    height: u32,
    bits: u32,
    time: u32,
}

/// What the next block's checks need from the chain before it: the stake modifier after the tip, and the last
/// two blocks of each kind (for the next difficulty of that kind).
pub struct PosState {
    pub modifier: Hash256,
    pos: [Option<Seen>; 2],
    pow: [Option<Seen>; 2],
}

/// The coin a coinstake's first input spends (the kernel): its value, the height and time of its transaction.
pub struct Kernel {
    pub value: u64,
    pub height: u32,
    pub tx_time: u32,
}

impl PosState {
    /// Read back from the store: walk down from `tip` until two blocks of each kind are found.
    pub fn load(st: &Store, tip: u32) -> PosState {
        let modifier = if tip == 0 { Hash256::ZERO } else { st.modifier(tip).map(|m| m.0).unwrap_or(Hash256::ZERO) };
        let mut s = PosState { modifier, pos: [None, None], pow: [None, None] };
        let mut h = tip;
        while h > 0 && (s.pos[1].is_none() || s.pow[1].is_none()) {
            let Some((_, is_pos)) = st.modifier(h) else { break };
            let Some((_, hd)) = st.header(h) else { break };
            let seen = Seen { height: h, bits: hd.bits, time: hd.time };
            let slot = if is_pos { &mut s.pos } else { &mut s.pow };
            if slot[0].is_none() {
                slot[0] = Some(seen);
            } else if slot[1].is_none() {
                slot[1] = Some(seen);
            }
            h -= 1;
        }
        s
    }

    /// The bits a block of this kind must carry (GetNextTargetRequired; genesis counts as the block of either
    /// kind that is found when there is none).
    fn expected_bits(&self, is_pos: bool) -> u32 {
        let limit = bits_from_target(kotia_pos::pos_limit());
        let (a, b) = if is_pos { (self.pos[0], self.pos[1]) } else { (self.pow[0], self.pow[1]) };
        let Some(a) = a else { return limit };           // the last of its kind is genesis
        let Some(b) = b else { return limit };           // the one before it is genesis
        let _ = a.height;
        kotia_pos::next_bits(a.bits, a.time, b.time)
    }

    /// Check a block's Kotia rules (before its coins move; `kernel` for a staked block), then step the state.
    pub fn check_and_step(&mut self, p: &ChainParams, b: &Block, height: u32, kernel: Option<Kernel>) -> Result<bool, String> {
        let is_pos = b.is_proof_of_stake();
        let time = b.header.time;
        // time stamps (CheckBlock)
        if let Some(cbt) = b.txs[0].time {
            if time > cbt.saturating_add(FUTURE_DRIFT) {
                return Err("coinbase timestamp is too early (bad-cb-time)".into());
            }
        }
        for t in &b.txs {
            if t.time.is_some_and(|tt| tt > time) {
                return Err(format!("tx {}: block timestamp earlier than transaction timestamp (bad-tx-time)", t.txid.to_hex()));
            }
        }
        if is_pos {
            let cst = b.txs[1].time.unwrap_or(0);
            let ok = if time >= V2_TIME { time == cst && cst & STAKE_TIMESTAMP_MASK == 0 } else { time == cst };
            if !ok {
                return Err("coinstake timestamp violation (bad-cs-time)".into());
            }
            let cb = &b.txs[0];
            if cb.outputs.len() != 1 || cb.outputs[0].value != 0 || !cb.outputs[0].script_pubkey.is_empty() {
                return Err("coinbase output not empty for proof-of-stake block (bad-cb-not-empty)".into());
            }
            if b.txs.iter().skip(2).any(|t| t.is_coinstake()) {
                return Err("more than one coinstake (bad-cs-multiple)".into());
            }
        } else if height > LAST_POW_BLOCK {
            return Err(format!("proof-of-work block at height {height} (bad-pow-height)"));
        }
        if height > LAST_POW_BLOCK && time & STAKE_TIMESTAMP_MASK != 0 && time >= V2_TIME {
            return Err("incorrect pos block timestamp (bad-pos-time)".into());
        }
        // the work: proof of work meets its target; the signature: none on a PoW block, the staker's on a PoS block
        if !is_pos {
            arcade_chain::params::check_block_work(p, b, height).map_err(|e| e.to_string())?;
            if b.sig.as_ref().is_some_and(|s| !s.is_empty()) {
                return Err("proof-of-work block with a block signature (bad-block-signature)".into());
            }
        } else {
            let sig = b.sig.as_deref().unwrap_or(&[]);
            let key = kotia_pos::block_signing_key(&b.txs[1].outputs[1].script_pubkey).ok_or("no block signing key in coinstake output 1 (bad-block-signature)")?;
            let vk = VerifyingKey::from_sec1_bytes(key).map_err(|_| "bad block signing key (bad-block-signature)")?;
            let s = Signature::from_der(sig).map_err(|_| "block signature not DER (bad-block-signature)")?;
            let s = s.normalize_s().unwrap_or(s);
            vk.verify_prehash(&b.id.0, &s).map_err(|_| "bad proof-of-stake block signature (bad-block-signature)")?;
        }
        // difficulty (ConnectBlock: bad-diffbits)
        let want = self.expected_bits(is_pos);
        if b.header.bits != want {
            return Err(format!("incorrect difficulty: bits {:08x}, expected {:08x} (bad-diffbits)", b.header.bits, want));
        }
        // the kernel (CheckProofOfStake, protocol v3)
        if is_pos && time >= V3_TIME {
            let k = kernel.ok_or("kernel input unavailable (bad-cs-kernel)")?;
            if height < k.height + 20 {
                return Err(format!("stake prevout is not mature: {} confirmations (bad-cs-kernel)", height - k.height));
            }
            let inp = &b.txs[1].inputs[0];
            if !check_kernel(&self.modifier, b.header.bits, k.tx_time, &inp.prevout.txid, inp.prevout.vout, k.value, b.txs[1].time.unwrap_or(0)) {
                return Err("check kernel failed on coinstake (bad-cs-proofhash)".into());
            }
        }
        // step: the modifier after this block, and this block as the last of its kind
        let kernel_id = if is_pos { b.txs[1].inputs[0].prevout.txid } else { b.id };
        self.modifier = next_modifier(&self.modifier, &kernel_id);
        let seen = Seen { height, bits: b.header.bits, time };
        let slot = if is_pos { &mut self.pos } else { &mut self.pow };
        slot[1] = slot[0];
        slot[0] = Some(seen);
        Ok(is_pos)
    }
}
