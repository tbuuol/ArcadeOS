// ArcadeOS-Admin: alles, was im Browser mit Schluesseln zu tun hat. Die 12 Woerter und der private Schluessel
// verlassen den Browser nie; die Box bekommt nur oeffentliche Schluessel, Adressen und fertige Signaturen.
//
// Gleich wie die Arcade-Wallets (dogecoin-arcade coins.js), damit dieselben Woerter dort dasselbe Konto sind:
//   BIP39 (englisch, 128 Bit) -> BIP32 m/44'/1'/0'/0/0 (SLIP-44 1 fuer Test-Chains) -> P2PKH, Version 113 (Pepecoin-Testnet)
//   Nachrichten wie Core "signmessage" mit "Pepecoin Signed Message:\n"
//   Transaktionen: Legacy-P2PKH, SIGHASH_ALL
//
// Gebuendelt mit esbuild aus @noble/hashes, @noble/curves, @scure/base, @scure/bip32, @scure/bip39 (MIT, Paul Miller).

import { secp256k1 } from '@noble/curves/secp256k1.js';
import { sha256 } from '@noble/hashes/sha2.js';
import { ripemd160 } from '@noble/hashes/legacy.js';
import { bytesToHex, hexToBytes, concatBytes, utf8ToBytes } from '@noble/hashes/utils.js';
import { base58check as b58c, base64 } from '@scure/base';
import { HDKey } from '@scure/bip32';
import { generateMnemonic, mnemonicToSeedSync, validateMnemonic } from '@scure/bip39';
import { wordlist } from '@scure/bip39/wordlists/english.js';
import { gcm } from '@noble/ciphers/aes.js';
import { pbkdf2Async } from '@noble/hashes/pbkdf2.js';
import { randomBytes } from '@noble/hashes/utils.js';

export const NETWORK = { name: 'test', pubkeyhash: 113, path: "m/44'/1'/0'/0/0" };
export const MAGIC = 'Pepecoin Signed Message:\n';

const b58 = b58c(sha256);
const dsha = (b) => sha256(sha256(b));
export const hash160 = (b) => ripemd160(sha256(b));
export { bytesToHex, hexToBytes, wordlist };

export function newWords() { return generateMnemonic(wordlist, 128); }
export function validWords(words) { return validateMnemonic(norm(words), wordlist); }
const norm = (w) => String(w || '').trim().toLowerCase().split(/\s+/).join(' ');

export function address(pubkey, version = NETWORK.pubkeyhash) {
  return b58.encode(concatBytes(Uint8Array.of(version), hash160(pubkey)));
}

/** Konto aus den 12 Woertern: {priv, pub (komprimiert, hex), address}. */
export function account(words) {
  const w = norm(words);
  if (!validateMnemonic(w, wordlist)) throw new Error('Die Woerter stimmen nicht (Rechtschreibung oder Pruefsumme).');
  const key = HDKey.fromMasterSeed(mnemonicToSeedSync(w)).derive(NETWORK.path);
  return { priv: key.privateKey, pub: bytesToHex(key.publicKey), address: address(key.publicKey) };
}

function varint(n) {
  if (n < 0xfd) return Uint8Array.of(n);
  if (n <= 0xffff) return Uint8Array.of(0xfd, n & 0xff, n >> 8);
  const b = new Uint8Array(5); b[0] = 0xfe; new DataView(b.buffer).setUint32(1, n, true); return b;
}

/** Core-"signmessage": base64 der 65-Byte-Signatur (Kopf 27 + recid + 4 fuer komprimiert). */
export function signMessage(priv, message) {
  const m = utf8ToBytes(MAGIC), t = utf8ToBytes(message);
  const h = dsha(concatBytes(varint(m.length), m, varint(t.length), t));
  const sig = secp256k1.sign(h, priv, { prehash: false, format: 'recovered' });
  return base64.encode(concatBytes(Uint8Array.of(27 + sig[0] + 4), sig.subarray(1)));
}

/** Pruefen wie Core "verifymessage": stimmt die Signatur zur Adresse? */
export function verifyMessage(sigB64, message, addr) {
  const raw = base64.decode(sigB64);
  if (raw.length !== 65) return false;
  const head = raw[0] - 27, compressed = head >= 4, recid = head & 3;
  const m = utf8ToBytes(MAGIC), t = utf8ToBytes(message);
  const h = dsha(concatBytes(varint(m.length), m, varint(t.length), t));
  const pub = secp256k1.recoverPublicKey(concatBytes(Uint8Array.of(recid), raw.subarray(1)), h, { prehash: false });
  const p = secp256k1.Point.fromBytes(pub).toBytes(compressed);
  return address(p) === addr;
}

// ------------------------------------------------------------------ Woerter mit Passwort verschluesseln
// Ohne WebCrypto (crypto.subtle gibt es nur auf https-Seiten, die Admin-Seite laeuft im LAN ueber http):
// PBKDF2-SHA256 (310.000 Runden) -> AES-256-GCM aus @noble/ciphers. Zufall: crypto.getRandomValues (ueberall da).
const PBKDF2_ROUNDS = 310000;
const vaultKey = (password, salt) => pbkdf2Async(sha256, utf8ToBytes(password), salt, { c: PBKDF2_ROUNDS, dkLen: 32 });

export async function sealWords(words, password) {
  const salt = randomBytes(16), iv = randomBytes(12);
  const ct = gcm(await vaultKey(password, salt), iv).encrypt(utf8ToBytes(words));
  return { v: 2, salt: bytesToHex(salt), iv: bytesToHex(iv), ct: bytesToHex(ct) };
}

export async function openWords(sealed, password) {
  if (!sealed || sealed.v !== 2) throw new Error('Unbekanntes Format, bitte mit den 12 Woertern anmelden.');
  const key = await vaultKey(password, hexToBytes(sealed.salt));
  let pt;
  try { pt = gcm(key, hexToBytes(sealed.iv)).decrypt(hexToBytes(sealed.ct)); } catch { throw new Error('Falsches Passwort.'); }
  return new TextDecoder().decode(pt);
}

// ------------------------------------------------------------------ Transaktion (Legacy, P2PKH)
const u32 = (n) => { const b = new Uint8Array(4); new DataView(b.buffer).setUint32(0, n >>> 0, true); return b; };
const u64 = (n) => { const b = new Uint8Array(8); new DataView(b.buffer).setBigUint64(0, BigInt(n), true); return b; };
const rev = (hex) => hexToBytes(hex).reverse();
const push = (data) => concatBytes(Uint8Array.of(data.length), data);   // Daten bis 75 Byte

function serialize(tx, scripts) {
  const parts = [u32(tx.version), varint(tx.inputs.length)];
  tx.inputs.forEach((i, n) => parts.push(rev(i.txid), u32(i.vout), varint(scripts[n].length), scripts[n], u32(0xffffffff)));
  parts.push(varint(tx.outputs.length));
  for (const o of tx.outputs) { const s = hexToBytes(o.script); parts.push(u64(o.value), varint(s.length), s); }
  parts.push(u32(tx.locktime || 0));
  return concatBytes(...parts);
}

/**
 * Unsignierte Transaktion der Box signieren: {version, locktime, inputs:[{txid, vout, script (hex, P2PKH des Kontos)}],
 * outputs:[{value (Satoshi), script (hex)}]}. Jeder Input muss dem Konto gehoeren (Skript = P2PKH seiner Adresse),
 * sonst wird nichts signiert. Ergebnis: Rohtransaktion als hex.
 */
export function signTx(tx, acct) {
  const pub = hexToBytes(acct.pub);
  const mine = bytesToHex(concatBytes(Uint8Array.of(0x76, 0xa9, 0x14), hash160(pub), Uint8Array.of(0x88, 0xac)));
  for (const i of tx.inputs) if (i.script !== mine) throw new Error('Ein Input gehoert nicht zu diesem Konto - nichts signiert.');
  const empty = tx.inputs.map(() => new Uint8Array(0));
  const sigScripts = tx.inputs.map((inp, n) => {
    const scripts = empty.slice(); scripts[n] = hexToBytes(inp.script);
    const h = dsha(concatBytes(serialize(tx, scripts), u32(1)));            // SIGHASH_ALL
    const der = secp256k1.sign(h, acct.priv, { prehash: false, format: 'der' });
    return concatBytes(push(concatBytes(der, Uint8Array.of(1))), push(pub));
  });
  return bytesToHex(serialize(tx, sigScripts));
}
