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
import { hmac } from '@noble/hashes/hmac.js';
import { sha512 } from '@noble/hashes/sha2.js';
import { ed25519, x25519 } from '@noble/curves/ed25519.js';

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

// ------------------------------------------------------------------ Arcade-Konten (dogecoin-arcade seed.py, signin.js)
// Die Arcades leiten Schluessel, die nicht secp256k1 sind, ueber einen "nur gehaerteten" Baum ab: HMAC-SHA512 wie
// BIP32, aber OHNE Kurven-Addition (seed.py child()). Zweig 24946' ("ar"):
//   24946'/0'/0'  X25519-Schluessel des Messengers      24946'/1'/0'  Ed25519-Anmeldeschluessel (Konto-Kennung)
export const ARCADE_PURPOSE = 24946;
const H = 0x80000000;
export function arcadePath(seed, path) {
  let d = hmac(sha512, utf8ToBytes('Bitcoin seed'), seed);
  let key = d.slice(0, 32), chain = d.slice(32);
  for (const i of path) {
    const idx = new Uint8Array(4); new DataView(idx.buffer).setUint32(0, (i | 0) + H >>> 0, false);
    d = hmac(sha512, chain, concatBytes(Uint8Array.of(0), key, idx));
    key = d.slice(0, 32); chain = d.slice(32);
  }
  return key;
}
/** Woerter wie die Arcades sie normalisieren (signin.js normalise) */
export const normaliseWords = (w) => String(w || '').normalize('NFKD').toLowerCase().split(/\s+/).filter(Boolean).join(' ');
const seedOf = (words) => mnemonicToSeedSync(normaliseWords(words));

/** Anmeldeschluessel eines Kontos: {secret (32), pubkey (hex) = Konto-Kennung} */
export function loginKey(words) {
  const secret = arcadePath(seedOf(words), [ARCADE_PURPOSE, 1, 0]);
  return { secret, pubkey: bytesToHex(ed25519.getPublicKey(secret)) };
}
export function signLogin(secret, message) { return bytesToHex(ed25519.sign(utf8ToBytes(message), secret)); }

/** Messenger-Schluessel (X25519): {secret, pubkey (hex)} */
export function messagingKey(words) {
  const secret = arcadePath(seedOf(words), [ARCADE_PURPOSE, 0, 0]);
  return { secret, pubkey: bytesToHex(x25519.getPublicKey(secret)) };
}

/** Alles, was ein Konto im Browser braucht, aus den 12 Woertern */
export function fullAccount(words) {
  const a = account(words);
  return { ...a, words: normaliseWords(words), login: loginKey(words), msg: messagingKey(words) };
}

// Das verschluesselte Wallet, wie die Arcades es speichern und weitergeben (wallet_js.js seal/open):
// {kdf: "PBKDF2-SHA512", iterations: 600000, salt, nonce, sealed} = AES-256-GCM der normalisierten Woerter,
// Schluessel PBKDF2-HMAC-SHA512(Passwort). Gleiches Format => Konten gehen zwischen den Arcades und uns hin und her.
export const ARCADE_ROUNDS = 600000;
export const MIN_PASSWORD = 12;
export async function arcadeSeal(words, password) {
  const salt = randomBytes(16), nonce = randomBytes(12);
  const key = await pbkdf2Async(sha512, utf8ToBytes(password), salt, { c: ARCADE_ROUNDS, dkLen: 32 });
  const sealed = gcm(key, nonce).encrypt(utf8ToBytes(normaliseWords(words)));
  return { kdf: 'PBKDF2-SHA512', iterations: ARCADE_ROUNDS, salt: bytesToHex(salt), nonce: bytesToHex(nonce), sealed: bytesToHex(sealed) };
}
export async function arcadeOpen(blob, password) {
  if (!blob || blob.kdf !== 'PBKDF2-SHA512' || !(blob.iterations >= 100000)) throw new Error('Unbekanntes Wallet-Format.');
  const key = await pbkdf2Async(sha512, utf8ToBytes(password), hexToBytes(blob.salt), { c: blob.iterations, dkLen: 32 });
  let pt;
  try { pt = gcm(key, hexToBytes(blob.nonce)).decrypt(hexToBytes(blob.sealed)); } catch { throw new Error('Falsches Passwort.'); }
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

// ------------------------------------------------------------------ Messenger (arcm, wie dogecoin-arcade envelope.py)
// inner = crypto_box(Absender, Empfaenger, "arcm" 01 01 || Text) mit Zufalls-Nonce (24)
// sealed = crypto_box_seal(Empfaenger, Absender-pk || nonce || inner)
// Nutzlast = "arcm" 01 01 || clen (2, BE) || sealed
import { hsalsa, xsalsa20poly1305 } from '@noble/ciphers/salsa.js';
import { blake2b } from '@noble/hashes/blake2.js';

const SIGMA = new Uint32Array([0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]);
const ARCM_SINGLE = Uint8Array.of(0x61, 0x72, 0x63, 0x6d, 1, 1);
function boxKey(secret, pub) {                       // crypto_box_beforenm
  const s = Uint8Array.from(x25519.getSharedSecret(secret, pub));
  const out = new Uint32Array(8);
  hsalsa(SIGMA, new Uint32Array(s.buffer), new Uint32Array(4), out);
  return new Uint8Array(out.buffer);
}
function sealNonce(epk, pub) { return blake2b(concatBytes(epk, pub), { dkLen: 24 }); }
export function boxSeal(msg, pub) {                  // crypto_box_seal
  const esk = randomBytes(32), epk = x25519.getPublicKey(esk);
  return concatBytes(epk, xsalsa20poly1305(boxKey(esk, pub), sealNonce(epk, pub)).encrypt(msg));
}
export function boxSealOpen(c, secret, pub) {        // crypto_box_seal_open
  const epk = c.subarray(0, 32);
  return xsalsa20poly1305(boxKey(secret, epk), sealNonce(epk, pub)).decrypt(c.subarray(32));
}
/** Nachricht an `toPub` (hex) versiegeln -> Nutzlast (hex) */
export function arcmSeal(me, toPub, text) {
  const to = hexToBytes(toPub), nonce = randomBytes(24);
  const inner = xsalsa20poly1305(boxKey(me.secret, to), nonce).encrypt(concatBytes(ARCM_SINGLE, utf8ToBytes(text)));
  const sealed = boxSeal(concatBytes(hexToBytes(me.pubkey), nonce, inner), to);
  const clen = Uint8Array.of(sealed.length >> 8, sealed.length & 0xff);
  return bytesToHex(concatBytes(ARCM_SINGLE, clen, sealed));
}
const ARCB = Uint8Array.of(1, 0x41, 0x52, 0x43, 0x42);
/** Nutzlast (hex) oeffnen, wenn sie an `me` geht: {sender (hex), text, profile, file} oder null */
export function arcmOpen(me, payloadHex) {
  const p = hexToBytes(payloadHex);
  if (p.length < 8 || p[5] !== 1) return null;
  for (let i = 0; i < 6; i++) if (p[i] !== ARCM_SINGLE[i]) return null;
  const clen = (p[6] << 8) | p[7];
  if (clen < 48 + 32 + 40 || 8 + clen > p.length) return null;
  let env;
  try { env = boxSealOpen(p.subarray(8, 8 + clen), me.secret, hexToBytes(me.pubkey)); } catch { return null; }
  if (env.length < 32 + 40) return null;
  const sender = env.subarray(0, 32), nonce = env.subarray(32, 56);
  let plain;
  try { plain = xsalsa20poly1305(boxKey(me.secret, sender), nonce).decrypt(env.subarray(56)); } catch { return { sender: bytesToHex(sender), forged: true }; }
  for (let i = 0; i < 6; i++) if (plain[i] !== ARCM_SINGLE[i]) return { sender: bytesToHex(sender), forged: true };
  const body = plain.subarray(6), out = { sender: bytesToHex(sender), text: '' };
  const dec = (b) => new TextDecoder().decode(b);
  if (body.length > 8 && ARCB.every((x, i) => body[i] === x) && body[5] === 1) {
    const n = (body[6] << 8) | body[7];
    try {
      const h = JSON.parse(dec(body.subarray(8, 8 + n)));
      out.text = typeof h.text === 'string' ? h.text : '';
      if (h.profile && typeof h.profile === 'object') out.profile = { name: String(h.profile.name || '').slice(0, 80) };
      if (h.file && typeof h.file === 'object') out.file = { name: String(h.file.name || 'Anhang').slice(0, 120), type: String(h.file.type || ''), size: body.length - 8 - n };
      return out;
    } catch { /* als Text zeigen */ }
  }
  out.text = dec(body);
  return out;
}
