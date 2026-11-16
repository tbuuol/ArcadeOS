//! Was der Teiler bekommt (vorbereitet, noch von nichts benutzt): wer eine Chain leiht, zahlt der Arcade, deren
//! Knoten er benutzt, ein Fuenftel jeder Gebuehr, die sein Betreiber auf dieser Chain nimmt (wie die Arcades,
//! docs/shared-cores.md "What the arcade that shares is paid"):
//!   - Teiler: ein Fuenftel; ist das unter der Staubgrenze der Chain, die Staubgrenze (der Teiler bekommt immer
//!     etwas, das die Chain traegt).
//!   - Diese Arcade: der Rest, nie unter der Staubgrenze (die Gebuehr kann dadurch etwas hoeher werden).
//! Wohin: die Adresse, die der Teiler in seinem Angebot nennt (`p`), wenn sie fuer diese Chain gueltig ist
//! (borrow::payee). Diese Arcade nennt ihre eigene, wenn der Betreiber eine hinterlegt hat (chains.json "payto").
//!
//! Benutzt wird das erst von Funktionen, die Gebuehren nehmen (Inschriften, Tausch); bis dahin zahlt nichts.

use arcade_chain::params::ChainParams;
use sha2::{Digest, Sha256};

const COIN: u64 = 100_000_000;

/// Staubgrenze je Chain (dogecoin-arcade config.py dust_limit; sonst 0.01)
pub fn dust_limit(net: &str) -> u64 {
    match net {
        "ltc" | "ltctest" | "bel" | "wjk" | "dgb" | "dgbtest" => 10_000,
        "kot" => 100_000,
        _ => COIN / 100,
    }
}

/// (diese Arcade, Teiler) fuer eine Gebuehr `total` auf `net`
#[allow(dead_code)] // vorbereitet: die erste Funktion mit Gebuehren auf einer geliehenen Chain benutzt sie
pub fn split_with_sharer(net: &str, total: u64) -> (u64, u64) {
    let dust = dust_limit(net);
    let sharer = (total / 5).max(dust);
    let ours = total.saturating_sub(sharer).max(dust);
    (ours, sharer)
}

fn params(net: &str) -> Option<&'static ChainParams> {
    crate::borrow::lendable().iter().find(|l| l.net == net).map(|l| l.params)
}

fn b58decode_check(s: &str) -> Option<Vec<u8>> {
    let v = bs58::decode(s).into_vec().ok()?;
    if v.len() < 5 {
        return None;
    }
    let (d, c) = v.split_at(v.len() - 4);
    let h = Sha256::digest(Sha256::digest(d));
    (h[..4] == *c).then(|| d.to_vec())
}

const BECH32_CHARS: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn polymod(v: &[u8]) -> u32 {
    let g = [0x3b6a57b2u32, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut c = 1u32;
    for &x in v {
        let b = c >> 25;
        c = ((c & 0x1ffffff) << 5) ^ x as u32;
        for (i, gi) in g.iter().enumerate() {
            if (b >> i) & 1 == 1 {
                c ^= gi;
            }
        }
    }
    c
}

/// Segwit-Adresse (BIP173/BIP350) mit dem HRP der Chain: Version 0 mit Bech32 (20 oder 32 Byte), ab 1 mit Bech32m
fn bech32_ok(hrp: &str, s: &str) -> bool {
    let s_low = s.to_ascii_lowercase();
    if s != s_low && s != s.to_ascii_uppercase() {
        return false;
    }
    let Some((h, data)) = s_low.rsplit_once('1') else { return false };
    if h != hrp || data.len() < 7 || s.len() > 90 {
        return false;
    }
    let Some(d): Option<Vec<u8>> = data.bytes().map(|c| BECH32_CHARS.iter().position(|x| *x == c).map(|p| p as u8)).collect() else { return false };
    let mut v: Vec<u8> = h.bytes().map(|c| c >> 5).collect();
    v.push(0);
    v.extend(h.bytes().map(|c| c & 31));
    v.extend(&d);
    let pm = polymod(&v);
    let ver = d[0];
    let want = if ver == 0 { 1 } else { 0x2bc830a3 };
    if pm != want || ver > 16 {
        return false;
    }
    // Programm: 5-Bit-Gruppen ohne Version und Pruefsumme in Bytes
    let prog5 = &d[1..d.len() - 6];
    let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for &x in prog5 {
        acc = (acc << 5) | x as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits >= 5 || (acc & ((1 << bits) - 1)) != 0 {
        return false;
    }
    (2..=40).contains(&out.len()) && (ver != 0 || out.len() == 20 || out.len() == 32)
}

/// Ist `addr` eine Adresse, an die auf `net` gezahlt werden kann? (P2PKH, P2SH; Segwit, wo die Chain es hat)
pub fn valid_address(net: &str, addr: &str) -> bool {
    let Some(p) = params(net) else { return false };
    if addr.len() > 100 {
        return false;
    }
    if let Some(d) = b58decode_check(addr) {
        return d.len() == 21 && (d[0] == p.p2pkh_version || d[0] == p.p2sh_version);
    }
    p.bech32_hrp.is_some_and(|hrp| bech32_ok(hrp, addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifth_and_dust() {
        // 1 PEP: 0.2 to the sharer, 0.8 stays
        assert_eq!(split_with_sharer("main", COIN), (80_000_000, 20_000_000));
        // a fifth under the dust limit: raised to it
        assert_eq!(split_with_sharer("main", 2_000_000), (1_000_000, 1_000_000));
        // what stays never under the dust limit either: the fee grows
        assert_eq!(split_with_sharer("main", 1_000_000), (1_000_000, 1_000_000));
        assert_eq!(split_with_sharer("kot", 300_000), (200_000, 100_000));
        assert_eq!(split_with_sharer("ltc", 1_000_000), (800_000, 200_000));
    }

    #[test]
    fn addresses() {
        // Pepecoin testnet (our faucet), Kotia (the Freigabe-Fach address), a Pepecoin testnet address on the wrong chain
        assert!(valid_address("test", "nmyxoweU4boh3UqFc5naJeTQbrNPGb4FUu"));
        assert!(valid_address("kot", "KiQnZKk6DAEJjVvTPpZMS2rGacG8jj9qdg"));
        assert!(!valid_address("kot", "nmyxoweU4boh3UqFc5naJeTQbrNPGb4FUu"));
        assert!(!valid_address("test", "nmyxoweU4boh3UqFc5naJeTQbrNPGb4FUv"), "checksum");
        // Bech32 / Bech32m: the official vectors of BIP173 (P2WPKH, P2WSH) and BIP350 (Taproot)
        assert!(bech32_ok("bc", "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"));
        assert!(bech32_ok("bc", "BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4"));
        assert!(bech32_ok("tb", "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sl5k7"));
        assert!(bech32_ok("bc", "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0"));
        // broken checksum, wrong hrp, mixed case, v0 with the Bech32m checksum (BIP350 invalid vector)
        assert!(!bech32_ok("bc", "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5"));
        assert!(!bech32_ok("ltc", "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"));
        assert!(!bech32_ok("bc", "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kV8f3t4"));
        assert!(!bech32_ok("bc", "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kemeawh"));
        assert!(!valid_address("nope", "x"));
    }
}
