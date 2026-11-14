//! Addresses of standard output scripts, as Core 0.14 names them (for the RPC's scriptPubKey).

use arcade_chain::hash::{hash160, sha256d};
use arcade_chain::params::ChainParams;

pub fn base58check(version: u8, payload: &[u8]) -> String {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut data = vec![version];
    data.extend_from_slice(payload);
    let check = sha256d(&data);
    data.extend_from_slice(&check.0[..4]);
    let zeros = data.iter().take_while(|b| **b == 0).count();
    let mut digits: Vec<u8> = Vec::new();
    for &b in &data {
        let mut carry = b as u32;
        for d in digits.iter_mut() {
            carry += (*d as u32) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut s = String::new();
    for _ in 0..zeros {
        s.push('1');
    }
    for d in digits.iter().rev() {
        s.push(A[*d as usize] as char);
    }
    s
}

/// (Core's type name, address) of a P2PKH, P2SH or P2PK script.
pub fn address(p: &ChainParams, s: &[u8]) -> Option<(&'static str, String)> {
    if s.len() == 25 && s[0] == 0x76 && s[1] == 0xa9 && s[2] == 20 && s[23] == 0x88 && s[24] == 0xac {
        return Some(("pubkeyhash", base58check(p.p2pkh_version, &s[3..23])));
    }
    if s.len() == 23 && s[0] == 0xa9 && s[1] == 20 && s[22] == 0x87 {
        return Some(("scripthash", base58check(p.p2sh_version, &s[2..22])));
    }
    if (s.len() == 35 && s[0] == 33 || s.len() == 67 && s[0] == 65) && s[s.len() - 1] == 0xac {
        return Some(("pubkey", base58check(p.p2pkh_version, &hash160(&s[1..s.len() - 1]))));
    }
    None
}
