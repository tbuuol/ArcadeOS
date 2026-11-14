//! A small wallet for the arcade's faucet: P2PKH keys from a file (one WIF per line, as Core's dumpwallet
//! writes them; the file stays root-only and is never logged), their coins followed block by block, and
//! `sendtoaddress`: pick coins, pay, change back to the first key, sign, into the mempool, out to the peers.

use crate::store::{outkey, Coin, OutKey};
use arcade_chain::hash::{hash160, sha256d};
use arcade_chain::params::ChainParams;
use arcade_chain::Hash256;
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey};
use std::collections::{BTreeMap, HashMap, HashSet};

/// fee: 0.01 coins per 1000 bytes (ten times the relay minimum)
const FEE_PER_BYTE: u64 = 1000;

struct Key {
    sk: SigningKey,
    pubkey: Vec<u8>,
}

pub struct Wallet {
    keys: Vec<Key>,
    by_script: HashMap<Vec<u8>, usize>,
    pub coins: BTreeMap<OutKey, Coin>,
    locked: HashSet<OutKey>,
}

fn base58_decode(s: &str) -> Option<Vec<u8>> {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut out: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let mut carry = A.iter().position(|&x| x == c)? as u32;
        for b in out.iter_mut().rev() {
            carry += (*b as u32) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let zeros = s.bytes().take_while(|&c| c == b'1').count();
    let mut v = vec![0u8; zeros];
    v.extend(out);
    if v.len() < 5 {
        return None;
    }
    let (data, check) = v.split_at(v.len() - 4);
    (sha256d(data).0[..4] == *check).then(|| data.to_vec())
}

/// The output script an address stands for (P2PKH or P2SH of this chain).
pub fn address_script(p: &ChainParams, a: &str) -> Option<Vec<u8>> {
    let d = base58_decode(a)?;
    if d.len() != 21 {
        return None;
    }
    if d[0] == p.p2pkh_version {
        let mut s = vec![0x76, 0xa9, 20];
        s.extend_from_slice(&d[1..]);
        s.extend([0x88, 0xac]);
        Some(s)
    } else if d[0] == p.p2sh_version {
        let mut s = vec![0xa9, 20];
        s.extend_from_slice(&d[1..]);
        s.push(0x87);
        Some(s)
    } else {
        None
    }
}

fn p2pkh(pubkey: &[u8]) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 20];
    s.extend_from_slice(&hash160(pubkey));
    s.extend([0x88, 0xac]);
    s
}

fn varint(v: &mut Vec<u8>, n: u64) {
    arcade_chain::encode::write_varint(v, n)
}

fn push(v: &mut Vec<u8>, data: &[u8]) {
    v.push(data.len() as u8);
    v.extend_from_slice(data);
}

impl Wallet {
    /// Keys from a file of WIFs (dumpwallet format: the WIF is the first word of each line; # lines skipped).
    pub fn load(p: &ChainParams, path: &str) -> Result<Wallet, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("wallet {path}: {e}"))?;
        let mut w = Wallet { keys: Vec::new(), by_script: HashMap::new(), coins: BTreeMap::new(), locked: HashSet::new() };
        for line in text.lines() {
            let word = line.split_whitespace().next().unwrap_or("");
            if word.is_empty() || word.starts_with('#') {
                continue;
            }
            let Some(d) = base58_decode(word) else { continue };
            if d.first() != Some(&p.wif_version) || !(d.len() == 33 || d.len() == 34 && d[33] == 1) {
                continue;
            }
            let Ok(sk) = SigningKey::from_slice(&d[1..33]) else { continue };
            let compressed = d.len() == 34;
            let pubkey = sk.verifying_key().to_encoded_point(compressed).as_bytes().to_vec();
            w.by_script.insert(p2pkh(&pubkey), w.keys.len());
            w.keys.push(Key { sk, pubkey });
        }
        if w.keys.is_empty() {
            return Err(format!("wallet {path}: no keys of this chain"));
        }
        Ok(w)
    }

    pub fn keys(&self) -> usize {
        self.keys.len()
    }

    pub fn mine(&self, script: &[u8]) -> bool {
        self.by_script.contains_key(script)
    }

    /// Read the wallet's coins from the coin set (at start, and after a reorg).
    pub fn rescan(&mut self, st: &crate::store::Store) -> Result<(), String> {
        let r = st.env.read_txn().map_err(|e| e.to_string())?;
        self.coins.clear();
        for item in st.coins.iter(&r).map_err(|e| e.to_string())? {
            let (k, v) = item.map_err(|e| e.to_string())?;
            if let Some(c) = Coin::decode(v) {
                if self.by_script.contains_key(&c.script) {
                    self.coins.insert(k.try_into().unwrap(), c);
                }
            }
        }
        Ok(())
    }

    /// A block's coins: what it spent of ours goes, what it pays us comes.
    pub fn block(&mut self, spent: &[OutKey], created: Vec<(OutKey, Coin)>) {
        for k in spent {
            self.coins.remove(k);
            self.locked.remove(k);
        }
        for (k, c) in created {
            if self.by_script.contains_key(&c.script) {
                self.coins.insert(k, c);
            }
        }
    }

    pub fn balance(&self, tip: u32, p: &ChainParams) -> (u64, u64) {
        let m = p.value_rules.map(|v| v.maturity);
        let (mut ok, mut immature) = (0, 0);
        for c in self.coins.values() {
            if c.coinbase && m.is_some_and(|f| tip + 1 < c.height + f(c.height)) {
                immature += c.value;
            } else {
                ok += c.value;
            }
        }
        (ok, immature)
    }

    /// Build and sign a payment of `amount` to `script`. Returns the raw transaction and the coins it spends.
    pub fn pay(&mut self, p: &ChainParams, tip: u32, script: Vec<u8>, amount: u64) -> Result<(Vec<u8>, Vec<OutKey>), String> {
        let m = p.value_rules.map(|v| v.maturity);
        let mut cands: Vec<(&OutKey, &Coin)> = self
            .coins
            .iter()
            .filter(|(k, c)| !self.locked.contains(*k) && !(c.coinbase && m.is_some_and(|f| tip + 1 < c.height + f(c.height))))
            .collect();
        cands.sort_by(|a, b| b.1.value.cmp(&a.1.value));
        let mut picked: Vec<(OutKey, Coin)> = Vec::new();
        let mut sum = 0u64;
        let mut fee = 0;
        for (k, c) in cands {
            picked.push((*k, c.clone()));
            sum += c.value;
            fee = FEE_PER_BYTE * (10 + 148 * picked.len() as u64 + 34 * 2);
            if sum >= amount + fee {
                break;
            }
        }
        if sum < amount + fee {
            return Err("Insufficient funds".into());
        }
        let change = sum - amount - fee;
        let mut outs = vec![(amount, script)];
        if change >= 1_000_000 {
            outs.push((change, p2pkh(&self.keys[0].pubkey)));
        }
        let serialize = |sigs: &[Vec<u8>], signing: Option<usize>| -> Vec<u8> {
            let mut t = Vec::new();
            t.extend(1i32.to_le_bytes());
            varint(&mut t, picked.len() as u64);
            for (i, (k, c)) in picked.iter().enumerate() {
                t.extend_from_slice(&k[..32]);
                t.extend_from_slice(&u32::from_be_bytes(k[32..].try_into().unwrap()).to_le_bytes());
                let s: &[u8] = match signing {
                    Some(j) if j == i => &c.script,
                    Some(_) => &[],
                    None => &sigs[i],
                };
                varint(&mut t, s.len() as u64);
                t.extend_from_slice(s);
                t.extend(0xffff_fffeu32.to_le_bytes());
            }
            varint(&mut t, outs.len() as u64);
            for (v, s) in &outs {
                t.extend(v.to_le_bytes());
                varint(&mut t, s.len() as u64);
                t.extend_from_slice(s);
            }
            t.extend(tip.to_le_bytes()); // lock time: the current height, as Core's wallet does
            t
        };
        let mut sigs = Vec::new();
        for i in 0..picked.len() {
            let ki = self.by_script[&picked[i].1.script];
            let mut pre = serialize(&[], Some(i));
            pre.extend(1u32.to_le_bytes()); // SIGHASH_ALL
            let digest = sha256d(&pre);
            let sig: Signature = self.keys[ki].sk.sign_prehash(&digest.0).map_err(|e| e.to_string())?;
            let sig = sig.normalize_s().unwrap_or(sig);
            let mut der = sig.to_der().as_bytes().to_vec();
            der.push(1);
            let mut ss = Vec::new();
            push(&mut ss, &der);
            push(&mut ss, &self.keys[ki].pubkey);
            sigs.push(ss);
        }
        let raw = serialize(&sigs, None);
        Ok((raw, picked.into_iter().map(|x| x.0).collect()))
    }

    pub fn lock(&mut self, ks: &[OutKey]) {
        self.locked.extend(ks.iter().copied());
    }
}

pub fn txid_of(raw: &[u8]) -> Hash256 {
    sha256d(raw)
}

#[allow(dead_code)]
pub fn key_of(txid: &Hash256, v: u32) -> OutKey {
    outkey(txid, v)
}
