//! Building a staked Kotia block the way Kotia Core's wallet does (CreateCoinStake / SignBlock):
//! coinbase with one empty output, coinstake spending the kernel coin to [empty, P2PK of the coin's key with
//! coin + 20 KOT], the block signed with that key. No other transactions. Without a key (check mode) the
//! signatures stay empty: the block is then only for comparing with the block the real staker made.

use arcade_chain::encode::write_varint;
use arcade_chain::hash::{hash160, merkle_root, sha256d};
use arcade_chain::kotia_pos::STAKE_SUBSIDY;
use arcade_chain::{Hash256, Header};
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey};

pub struct Built {
    pub raw: Vec<u8>,
    pub header: Header,
    pub coinbase: Vec<u8>,
    /// the coinstake with its scriptSig left empty (what a comparison looks at)
    pub coinstake_blank: Vec<u8>,
}

pub fn p2pk(pubkey: &[u8]) -> Vec<u8> {
    let mut s = vec![pubkey.len() as u8];
    s.extend_from_slice(pubkey);
    s.push(0xac);
    s
}

pub fn p2pkh(pubkey: &[u8]) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend_from_slice(&hash160(pubkey));
    s.extend_from_slice(&[0x88, 0xac]);
    s
}

/// The public key a coin's script pays to, when it is P2PK.
pub fn p2pk_key(script: &[u8]) -> Option<&[u8]> {
    let n = *script.first()? as usize;
    ((n == 33 || n == 65) && script.len() == n + 2 && script[n + 1] == 0xac).then(|| &script[1..1 + n])
}

fn push(v: &mut Vec<u8>, d: &[u8]) {
    write_varint(v, d.len() as u64);
    v.extend_from_slice(d);
}

fn script_num(n: u32) -> Vec<u8> {
    let mut v = Vec::new();
    let mut x = n;
    while x > 0 {
        v.push((x & 0xff) as u8);
        x >>= 8;
    }
    if v.last().is_some_and(|b| b & 0x80 != 0) {
        v.push(0);
    }
    v
}

pub fn coinbase(height: u32, time: u32) -> Vec<u8> {
    let mut ss = Vec::new();
    let n = script_num(height);
    ss.push(n.len() as u8);
    ss.extend_from_slice(&n);
    ss.push(0x00);
    let mut o = Vec::new();
    o.extend(1i32.to_le_bytes());
    o.extend(time.to_le_bytes());
    o.push(1);
    o.extend([0u8; 32]);
    o.extend(u32::MAX.to_le_bytes());
    push(&mut o, &ss);
    o.extend(u32::MAX.to_le_bytes());
    o.push(1);
    o.extend(0u64.to_le_bytes());
    o.push(0);
    o.extend(0u32.to_le_bytes());
    o
}

fn coinstake(txid: &Hash256, vout: u32, script_sig: &[u8], out_script: &[u8], out_value: u64, time: u32) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend(1i32.to_le_bytes());
    o.extend(time.to_le_bytes());
    o.push(1);
    o.extend_from_slice(&txid.0);
    o.extend(vout.to_le_bytes());
    push(&mut o, script_sig);
    o.extend(u32::MAX.to_le_bytes());
    o.push(2);
    o.extend(0u64.to_le_bytes());
    o.push(0);
    o.extend(out_value.to_le_bytes());
    push(&mut o, out_script);
    o.extend(0u32.to_le_bytes());
    o
}

/// The staked block on `prev` (`height`) for the kernel coin (txid:vout, its value and script) at `time`.
/// `key`: the coin's key and its public key as the coin's script has it; None builds an unsigned block
/// (only P2PK coins then, whose key the script shows).
#[allow(clippy::too_many_arguments)]
pub fn build(p: &arcade_chain::params::ChainParams, version: i32, prev: Hash256, height: u32, time: u32, bits: u32, txid: &Hash256, vout: u32, value: u64, coin_script: &[u8], key: Option<(&SigningKey, &[u8])>) -> Result<Built, String> {
    let pubkey: Vec<u8> = match (key, p2pk_key(coin_script)) {
        (Some((_, pk)), _) => pk.to_vec(),
        (None, Some(pk)) => pk.to_vec(),
        (None, None) => return Err("P2PKH coin: its key is needed to know the coinstake's script".into()),
    };
    if coin_script != p2pk(&pubkey).as_slice() && coin_script != p2pkh(&pubkey).as_slice() {
        return Err("the key does not belong to the coin".into());
    }
    let out_script = p2pk(&pubkey);
    let out_value = value + STAKE_SUBSIDY;
    let cb = coinbase(height, time);
    let blank = coinstake(txid, vout, &[], &out_script, out_value, time);
    let mut script_sig = Vec::new();
    if let Some((sk, pk)) = key {
        // legacy signature hash (the tx time is part of the tx): the spent script in the input, SIGHASH_ALL
        let mut pre = coinstake(txid, vout, coin_script, &out_script, out_value, time);
        pre.extend(1u32.to_le_bytes());
        let d = sha256d(&pre);
        let sig: Signature = sk.sign_prehash(&d.0).map_err(|e| e.to_string())?;
        let sig = sig.normalize_s().unwrap_or(sig);
        let mut der = sig.to_der().as_bytes().to_vec();
        der.push(1);
        script_sig.push(der.len() as u8);
        script_sig.extend_from_slice(&der);
        if p2pk_key(coin_script).is_none() {
            script_sig.push(pk.len() as u8);
            script_sig.extend_from_slice(pk);
        }
    }
    let cs = coinstake(txid, vout, &script_sig, &out_script, out_value, time);
    let header = Header { version, prev, merkle_root: merkle_root(&[sha256d(&cb), sha256d(&cs)]), time, bits, nonce: 0 };
    let mut raw = header.encode().to_vec();
    raw.push(2);
    raw.extend_from_slice(&cb);
    raw.extend_from_slice(&cs);
    match key {
        Some((sk, _)) => {
            let id = p.block_id(&header);
            let sig: Signature = sk.sign_prehash(&id.0).map_err(|e| e.to_string())?;
            let sig = sig.normalize_s().unwrap_or(sig);
            push(&mut raw, sig.to_der().as_bytes());
        }
        None => raw.push(0),
    }
    Ok(Built { raw, header, coinbase: cb, coinstake_blank: blank })
}

/// A coinstake from a real block with its scriptSig taken out (to compare with Built::coinstake_blank).
pub fn blank_script_sig(cs: &arcade_chain::Tx) -> Option<Vec<u8>> {
    let i = cs.inputs.first()?;
    let outs: Vec<(u64, Vec<u8>)> = cs.outputs.iter().map(|o| (o.value, o.script_pubkey.clone())).collect();
    if outs.len() != 2 || cs.inputs.len() != 1 {
        return Some(Vec::new()); // a shape we do not build (split, several inputs): never equal
    }
    let mut o = coinstake(&i.prevout.txid, i.prevout.vout, &[], &outs[1].1, outs[1].0, cs.time.unwrap_or(0));
    // keep the real first output and lock time in view too
    if outs[0].0 != 0 || !outs[0].1.is_empty() || cs.lock_time != 0 || cs.version != 1 || i.sequence != u32::MAX {
        o.push(0xff);
    }
    Some(o)
}

/// The staking keys (a dumpwallet file: the WIF is the first word of a line; # lines skipped). Each key stakes
/// the coins paying to its P2PK and its P2PKH script. The file is read once and never written or logged.
pub struct Keys {
    keys: Vec<(SigningKey, Vec<u8>)>,
    by_script: std::collections::HashMap<Vec<u8>, usize>,
}

impl Keys {
    pub fn load(p: &arcade_chain::params::ChainParams, path: &str) -> Result<Keys, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("stake keys {path}: {e}"))?;
        let mut k = Keys { keys: Vec::new(), by_script: std::collections::HashMap::new() };
        for line in text.lines() {
            let word = line.split_whitespace().next().unwrap_or("");
            if word.is_empty() || word.starts_with('#') {
                continue;
            }
            let Some(d) = crate::wallet::base58_decode(word) else { continue };
            if d.first() != Some(&p.wif_version) || !(d.len() == 33 || d.len() == 34 && d[33] == 1) {
                continue;
            }
            let Ok(sk) = SigningKey::from_slice(&d[1..33]) else { continue };
            let pk = sk.verifying_key().to_encoded_point(d.len() == 34).as_bytes().to_vec();
            k.by_script.insert(p2pk(&pk), k.keys.len());
            k.by_script.insert(p2pkh(&pk), k.keys.len());
            k.keys.push((sk, pk));
        }
        if k.keys.is_empty() {
            return Err(format!("stake keys {path}: no Kotia keys in it"));
        }
        Ok(k)
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn scripts(&self) -> impl Iterator<Item = &Vec<u8>> {
        self.by_script.keys()
    }
    pub fn for_script(&self, s: &[u8]) -> Option<(&SigningKey, &[u8])> {
        self.by_script.get(s).map(|&i| (&self.keys[i].0, self.keys[i].1.as_slice()))
    }
}

/// Everything a node will check of our staked block, checked here before it leaves: it parses as a staked block
/// of two transactions on `prev` at `time` with `bits`; the coinstake spends exactly the kernel coin, pays coin +
/// 20 KOT (no more, no less) to the P2PK of the coin's own key and nothing else; its script verifies with Kotia's
/// flags; the kernel hits; the block signature verifies with the key in the coinstake.
#[allow(clippy::too_many_arguments)]
pub fn verify(p: &'static arcade_chain::params::ChainParams, raw: &[u8], prev: Hash256, height: u32, time: u32, bits: u32, modifier: Option<&Hash256>,
              txid: &Hash256, vout: u32, value: u64, coin_script: &[u8], coin_time: u32, pubkey: &[u8]) -> Result<Hash256, String> {
    use k256::ecdsa::signature::hazmat::PrehashVerifier;
    let header = Header::parse(raw.get(..80).ok_or("short")?.try_into().unwrap());
    let id = p.block_id(&header);
    let blk = crate::net::prep(p, id, raw)?;
    if blk.header.prev != prev || blk.header.time != time || blk.header.bits != bits || time & arcade_chain::kotia_pos::STAKE_TIMESTAMP_MASK != 0 {
        return Err("header (prev, time or bits) is not what was meant".into());
    }
    if !blk.is_proof_of_stake() || blk.txs.len() != 2 {
        return Err("not a staked block of two transactions".into());
    }
    let (cb, cs) = (&blk.txs[0], &blk.txs[1]);
    if cb.outputs.len() != 1 || cb.outputs[0].value != 0 || !cb.outputs[0].script_pubkey.is_empty() || cb.time != Some(time) {
        return Err("coinbase is not empty".into());
    }
    if cs.inputs.len() != 1 || cs.inputs[0].prevout.txid != *txid || cs.inputs[0].prevout.vout != vout || cs.time != Some(time) {
        return Err("coinstake does not spend exactly the kernel coin".into());
    }
    if cs.outputs.len() != 2 || cs.outputs[0].value != 0 || !cs.outputs[0].script_pubkey.is_empty()
        || cs.outputs[1].value != value + STAKE_SUBSIDY || cs.outputs[1].script_pubkey != p2pk(pubkey) {
        return Err("coinstake does not pay coin + 20 KOT back to the coin's own key".into());
    }
    if coin_script != p2pk(pubkey).as_slice() && coin_script != p2pkh(pubkey).as_slice() {
        return Err("the coin is not this key's".into());
    }
    let sp = [crate::rules::Spent { value, script: coin_script, height: 1, coinbase: false, pegout: false }];
    crate::rules::tx_scripts(p, cs, &sp, height, crate::kotia::script_flags(time)).map_err(|e| format!("coinstake script: {}", e.why))?;
    if modifier.is_some_and(|m| !arcade_chain::kotia_pos::check_kernel(m, bits, coin_time, txid, vout, value, time)) {
        return Err("kernel does not hit".into());
    }
    let key = arcade_chain::kotia_pos::block_signing_key(&cs.outputs[1].script_pubkey).ok_or("no signing key in output 1")?;
    let vk = k256::ecdsa::VerifyingKey::from_sec1_bytes(key).map_err(|e| e.to_string())?;
    let s = Signature::from_der(blk.sig.as_deref().unwrap_or(&[])).map_err(|e| e.to_string())?;
    vk.verify_prehash(&blk.id.0, &s).map_err(|_| "block signature does not verify".to_string())?;
    Ok(blk.id)
}

/// Signing checked end to end with a throwaway key (no real coin): a P2PK and a P2PKH coin are staked, and the
/// blocks go through the checks any block goes through: shape and merkle root, the coinstake's script, the
/// block signature.
pub fn self_test(p: &'static arcade_chain::params::ChainParams) -> Result<(), String> {
    use k256::ecdsa::signature::hazmat::PrehashVerifier;
    let mut b = [0u8; 32];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom").map_err(|e| e.to_string())?, &mut b).map_err(|e| e.to_string())?;
    let sk = SigningKey::from_slice(&b).map_err(|e| e.to_string())?;
    let pk = sk.verifying_key().to_encoded_point(true).as_bytes().to_vec();
    let other = SigningKey::from_slice(&sha256d(&b).0).map_err(|e| e.to_string())?.verifying_key().to_encoded_point(true).as_bytes().to_vec();
    let time = 1_800_000_000 & !arcade_chain::kotia_pos::STAKE_TIMESTAMP_MASK;
    for script in [p2pk(&pk), p2pkh(&pk)] {
        let txid = sha256d(&script);
        let built = build(p, 0x2000_0000, Hash256::ZERO, 2_000_000, time, 0x1d00ffff, &txid, 3, 123_456_789, &script, Some((&sk, &pk)))?;
        let id = p.block_id(&built.header);
        let blk = crate::net::prep(p, id, &built.raw)?;
        if !blk.is_proof_of_stake() || blk.txs.len() != 2 {
            return Err("built block is not a staked block".into());
        }
        let sp = [crate::rules::Spent { value: 123_456_789, script: &script, height: 1, coinbase: false, pegout: false }];
        crate::rules::tx_scripts(p, &blk.txs[1], &sp, 2_000_000, crate::kotia::script_flags(time)).map_err(|e| format!("coinstake script: {}", e.why))?;
        let key = arcade_chain::kotia_pos::block_signing_key(&blk.txs[1].outputs[1].script_pubkey).ok_or("no signing key in output 1")?;
        let vk = k256::ecdsa::VerifyingKey::from_sec1_bytes(key).map_err(|e| e.to_string())?;
        let s = Signature::from_der(blk.sig.as_deref().unwrap_or(&[])).map_err(|e| e.to_string())?;
        vk.verify_prehash(&blk.id.0, &s).map_err(|_| "block signature does not verify".to_string())?;
        if blk.txs[1].outputs[1].value != 123_456_789 + STAKE_SUBSIDY || blk.txs[1].outputs[1].script_pubkey != p2pk(&pk) {
            return Err("coinstake does not pay coin + reward back to the coin's key".into());
        }
        // the check before sending passes this block, and refuses it for a coin of another value or another key
        verify(p, &built.raw, Hash256::ZERO, 2_000_000, time, 0x1d00ffff, None, &txid, 3, 123_456_789, &script, 0, &pk)?;
        if verify(p, &built.raw, Hash256::ZERO, 2_000_000, time, 0x1d00ffff, None, &txid, 3, 123_456_788, &script, 0, &pk).is_ok() {
            return Err("check before sending passed a block paying 1 satoshi too much".into());
        }
        if verify(p, &built.raw, Hash256::ZERO, 2_000_000, time, 0x1d00ffff, None, &txid, 3, 123_456_789, &script, 0, &other).is_ok() {
            return Err("check before sending passed a block for another key".into());
        }
    }
    Ok(())
}
