//! Tausch zwischen zwei Spielern (Stufe E3b der App-Laufzeit, arcade.swap.trade im App-Fenster).
//!
//! Die Absprache (Angebot, Ja, Quittung) laeuft im Fenster als versteckte Raum-Nachricht; hier baut und prueft die
//! Box nur, sie signiert nie (sie hat keine Schluessel). EINE Transaktion begleicht alles: beide Seiten bewegen sich
//! oder keine.
//!
//! * A ist, wer zuerst signiert (der Spieler, der die Inschrift gibt; bei einem Buendel, wer es anbietet), B der
//!   andere. Die Inputs von A stehen vorn (Absender = erster Input), danach mindestens ein Coin von B (so hat B
//!   signiert und ist Partei).
//! * Eins gegen eins: `INSC 01 05` (KIND_SWAP, Class C): Verkaeufer A gibt die Inschrift, Kaeufer B gibt eine
//!   Inschrift oder Coins. Buendel (eine Seite ist eine Liste): `INSC 01 08` (KIND_BUNDLE, Class B, verschleiert mit
//!   der Adresse von A, Parteien a = A und b = B mit 21-Byte-Adressen). Wie die Arcades (inscriptions.py Swap,
//!   Bundle, Leg; state.py _swap, _bundle) und so, wie appindex.rs es auswertet.
//! * Coins fliessen in derselben Transaktion, nur in eine Richtung. Die Gebuehr zahlt, wer Coins gibt, sonst A:
//!   wer Coins bekommt, bekommt sie ganz (der Index rechnet "netto bezahlt").
//! * Token: abgelehnt, diese Arcade hat kein Token-Ledger.
//! * Ablauf: A laesst bauen (`build`), signiert im Browser NUR die eigenen Inputs (SIGHASH_ALL ueber die ganze
//!   Transaktion) und versiegelt das Teilstueck an den Nachrichtenschluessel von B. B oeffnet es im Browser, laesst
//!   seine Box pruefen (`check`: genau die Absprache, nur Coins von A und B, B zahlt nicht mehr als vereinbart, und
//!   der eigene Index wuerde beide Seiten bewegen), signiert seine Inputs, und die Box von B fuegt beide Haelften
//!   zusammen, prueft noch einmal und sendet (`send`).
//! * Nur Pepecoin-Testnet: Adressen anderer Netze (Mainnet) sind unlesbar und werden abgelehnt.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::appindex::{bundle_data, parse_bundle, swap_data, Bundle, Leg, BUNDLE_MOST};
use crate::utxo::{shown, Txid};
use crate::{App, ANYDATA, DUST, MARKER, MIN_FEE_PER_KB};

const MOST_INPUTS: usize = 20; // je Seite
const MOST_FEE: u64 = 50 * MIN_FEE_PER_KB; // 0,5 PEP: mehr Gebuehr nimmt keine Pruefung hin
const SEQ: u32 = 0xffff_ffff;

/// Die Absprache aus Sicht der Chain: A signiert zuerst und steht vorn
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    pub a: String,
    pub b: String,
    pub a_gives: Vec<Leg>,
    pub b_gives: Vec<Leg>,
    pub bundle: bool,
}

/// Ein Coin einer Partei (aus dem UTXO-Index)
#[derive(Clone, Debug)]
pub struct Coin {
    pub txid: String,
    pub vout: u32,
    pub sats: u64,
    pub script: String,
}

fn pep(s: u64) -> String {
    let t = format!("{}.{:08}", s / crate::COIN, s % crate::COIN);
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Ein Teil, wie die Seite ihn nennt: {inscription: id} oder {coins: "1.5"}; {token, amount} wird abgelehnt
pub fn leg_of(v: &Value) -> Result<Leg, String> {
    if v.get("token").is_some_and(|t| !t.is_null()) {
        return Err("tokens cannot be traded on this arcade: it keeps no token ledger (trade an inscription or coins)".into());
    }
    if let Some(t) = v["inscription"].as_str() {
        let t = t.trim().to_ascii_lowercase();
        if t.len() != 64 || !t.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("an inscription is named by its id (64 hex characters)".into());
        }
        return Ok(Leg::Insc(t));
    }
    if !v["coins"].is_null() {
        let n = crate::sendreq::parse_amount(&v["coins"]).filter(|n| *n > 0).ok_or("coins is an amount like \"1.5\"")?;
        if n < DUST {
            return Err(format!("a coin side is at least {} coins", pep(DUST)));
        }
        return Ok(Leg::Coins(n));
    }
    Err("a side is {inscription: id} or {coins: \"1.5\"}".into())
}

fn side(v: &Value, bundle: bool) -> Result<Vec<Leg>, String> {
    if !bundle {
        return Ok(vec![leg_of(v)?]);
    }
    let list = match v {
        Value::Array(a) => a.clone(),
        Value::Object(_) => vec![v.clone()],
        _ => return Err("a trade needs something on each side".into()),
    };
    if !(1..=BUNDLE_MOST).contains(&list.len()) {
        return Err(format!("a bundle side is a list of 1 to {BUNDLE_MOST} things"));
    }
    list.iter().map(leg_of).collect()
}

fn coins_of(l: &[Leg]) -> u64 {
    l.iter().map(|x| if let Leg::Coins(n) = x { *n } else { 0 }).sum()
}

fn pieces_of(l: &[Leg]) -> Vec<String> {
    l.iter().filter_map(|x| if let Leg::Insc(t) = x { Some(t.clone()) } else { None }).collect()
}

/// Die Absprache pruefen: `a` gibt `a_gives`, `b` gibt `b_gives` (wie die Seite sie nennt)
pub fn terms(a: &str, b: &str, a_gives: &Value, b_gives: &Value) -> Result<Terms, String> {
    let bundle = a_gives.is_array() || b_gives.is_array();
    let (ag, bg) = (side(a_gives, bundle)?, side(b_gives, bundle)?);
    for x in [a, b] {
        if crate::utxo::key_of(x).filter(|k| k[0] == 0).is_none() {
            return Err("not a Pepecoin testnet address (mainnet is refused)".into());
        }
    }
    if a == b {
        return Err("you cannot trade with yourself".into());
    }
    let (pa, pb) = (pieces_of(&ag), pieces_of(&bg));
    if pa.is_empty() && pb.is_empty() {
        return Err("a trade needs an inscription on at least one side".into());
    }
    let mut seen = HashSet::new();
    if !pa.iter().chain(&pb).all(|t| seen.insert(t.clone())) {
        return Err("one piece is named twice".into());
    }
    if coins_of(&ag) > 0 && coins_of(&bg) > 0 {
        return Err("coins go one way only".into());
    }
    if !bundle && !matches!(ag[0], Leg::Insc(_)) {
        return Err("in a one-for-one trade the player giving the inscription signs first".into());
    }
    Ok(Terms { a: a.into(), b: b.into(), a_gives: ag, b_gives: bg, bundle })
}

/// Gibt B Coins, zahlt B die Gebuehr; sonst A
fn b_pays_fee(t: &Terms) -> bool {
    coins_of(&t.b_gives) > 0
}

fn script_of(addr: &str) -> String {
    crate::utxo::script_of(&crate::utxo::key_of(addr).unwrap())
}

/// Die Ausgaben mit der Nutzlast: Class C (OP_RETURN) fuer eins gegen eins, Class B fuer ein Buendel
fn payload(t: &Terms, a_pub: Option<&[u8]>) -> Result<Vec<(u64, String)>, String> {
    if t.bundle {
        let d = bundle_data(&t.a, &t.b, &t.a_gives, &t.b_gives).ok_or("this bundle cannot be written")?;
        let pk = a_pub.ok_or("unlock your wallet first: a bundle names your public key")?;
        crate::msgs::class_b_outputs(&t.a, pk, &d)
    } else {
        let d = swap_data(&t.a_gives[0], &t.b_gives[0]).ok_or("this trade cannot be written")?;
        let mut full = MARKER.to_vec();
        full.extend_from_slice(&0u16.to_be_bytes());
        full.extend_from_slice(&ANYDATA.to_be_bytes());
        full.extend_from_slice(&d);
        Ok(vec![(0, crate::op_return_script(&full))])
    }
}

fn fee_for(inputs: usize, out_bytes: u64) -> u64 {
    (10 + inputs as u64 * 148 + out_bytes).div_ceil(1000) * MIN_FEE_PER_KB
}

/// Die UNSIGNIERTE Transaktion: Inputs von A (vorn), dann von B; Nutzlast, Wechselgeld A, Wechselgeld B.
/// `a_pub`: Pubkey von A (nur fuer ein Buendel, Class B). Form wie fund(): {version, locktime, inputs, outputs, fee}
pub fn assemble(t: &Terms, a_pub: Option<&[u8]>, a_coins: &[Coin], b_coins: &[Coin]) -> Result<Value, String> {
    let outs = payload(t, a_pub)?;
    let dust_out: u64 = outs.iter().map(|o| o.0).sum();
    let out_bytes: u64 = outs.iter().map(|o| 8 + crate::varint(o.1.len() / 2).len() as u64 + o.1.len() as u64 / 2).sum::<u64>() + 2 * 34;
    let (ca, cb, bpays) = (coins_of(&t.a_gives), coins_of(&t.b_gives), b_pays_fee(t));
    let usable = |c: &[Coin], who: &str| -> Vec<Coin> {
        let s = script_of(who);
        let mut v: Vec<Coin> = c.iter().filter(|x| x.script == s).cloned().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.sats));
        v.truncate(MOST_INPUTS);
        v
    };
    let (ua, ub) = (usable(a_coins, &t.a), usable(b_coins, &t.b));
    if ua.is_empty() {
        return Err(format!("{} has no coins to sign this trade with", t.a));
    }
    if ub.is_empty() {
        return Err(format!("{} has no coins to sign this trade with", t.b));
    }
    let sum = |v: &[Coin], n: usize| v[..n].iter().map(|x| x.sats).sum::<u64>();
    let (mut na, mut nb) = (1usize, 1usize);
    let fee = loop {
        let fee = fee_for(na + nb, out_bytes);
        let need_a = ca + if bpays { 0 } else { fee + dust_out };
        let need_b = cb + if bpays { fee + dust_out } else { 0 };
        let (oka, okb) = (sum(&ua, na) >= need_a + DUST, sum(&ub, nb) >= need_b + DUST);
        if oka && okb {
            break fee;
        }
        if !oka {
            if na == ua.len() {
                return Err(format!("not enough testnet PEP at {}: {} needed", t.a, pep(need_a + DUST)));
            }
            na += 1;
        }
        if !okb {
            if nb == ub.len() {
                return Err(format!("not enough testnet PEP at {}: {} needed", t.b, pep(need_b + DUST)));
            }
            nb += 1;
        }
    };
    let a_change = sum(&ua, na) + cb - ca - if bpays { 0 } else { fee + dust_out };
    let b_change = sum(&ub, nb) + ca - cb - if bpays { fee + dust_out } else { 0 };
    let inputs: Vec<Value> = ua[..na]
        .iter()
        .map(|c| (c, &t.a))
        .chain(ub[..nb].iter().map(|c| (c, &t.b)))
        .map(|(c, _)| json!({"txid": c.txid, "vout": c.vout, "script": c.script, "amount": c.sats}))
        .collect();
    let mut outputs: Vec<Value> = outs.iter().map(|(v, s)| json!({"value": v, "script": s})).collect();
    outputs.push(json!({"value": a_change, "script": script_of(&t.a)}));
    outputs.push(json!({"value": b_change, "script": script_of(&t.b)}));
    Ok(json!({"version": 1, "locktime": 0, "inputs": inputs, "outputs": outputs, "fee": fee}))
}

// ------------------------------------------------------------------ Rohtransaktionen

fn serialize(version: u32, locktime: u32, ins: &[((Txid, u32), &[u8])], outs: &[(u64, &[u8])]) -> Vec<u8> {
    let mut t = version.to_le_bytes().to_vec();
    t.extend(crate::varint(ins.len()));
    for ((prev, n), sig) in ins {
        t.extend_from_slice(prev);
        t.extend_from_slice(&n.to_le_bytes());
        t.extend(crate::varint(sig.len()));
        t.extend_from_slice(sig);
        t.extend_from_slice(&SEQ.to_le_bytes());
    }
    t.extend(crate::varint(outs.len()));
    for (v, s) in outs {
        t.extend_from_slice(&v.to_le_bytes());
        t.extend(crate::varint(s.len()));
        t.extend_from_slice(s);
    }
    t.extend_from_slice(&locktime.to_le_bytes());
    t
}

fn ends(raw: &[u8]) -> Result<(u32, u32), String> {
    if raw.len() < 10 {
        return Err("that is not a transaction".into());
    }
    Ok((u32::from_le_bytes(raw[..4].try_into().unwrap()), u32::from_le_bytes(raw[raw.len() - 4..].try_into().unwrap())))
}

/// Was die Pruefung herausfand
#[derive(Debug)]
pub struct Checked {
    /// zum Signieren im Browser (Form wie assemble)
    pub tx: Value,
    /// Adresse und Betrag jedes Inputs (fuer den Probelauf des Index)
    #[allow(dead_code)]
    pub prev: Vec<(Option<String>, u64)>,
    pub fee: u64,
    pub b_net: i128,
}

/// Prueft eine Transaktion gegen die Absprache. `full`: beide haben signiert, sonst nur A (die Inputs von B leer).
/// `owners`: jetzige Besitzer der Inschriften der Absprache; `h`: Hoehe, an der der Index sie liest.
pub fn verify(t: &Terms, raw: &[u8], a_coins: &[Coin], b_coins: &[Coin], owners: &[(String, String)], h: u32, full: bool) -> Result<Checked, String> {
    let tx = crate::utxo::parse_tx(raw).map_err(|_| "that is not a transaction".to_string())?;
    let (version, locktime) = ends(raw)?;
    let ins: Vec<((Txid, u32), &[u8])> = tx.ins.iter().map(|i| (i.prev, i.sig)).collect();
    if serialize(version, locktime, &ins, &tx.outs) != raw {
        return Err("that transaction has parts this arcade does not build (sequence, witness)".into());
    }
    let (sa, sb) = (script_of(&t.a), script_of(&t.b));
    let mut prev = Vec::new();
    let mut inputs = Vec::new();
    let (mut a_in, mut b_in, mut b_seen) = (0u64, 0u64, false);
    for (n, i) in tx.ins.iter().enumerate() {
        let (txid, vout) = (shown(&i.prev.0), i.prev.1);
        let find = |c: &[Coin], s: &str| c.iter().find(|x| x.txid == txid && x.vout == vout && x.script == s).cloned();
        let (owner, coin) = if let Some(c) = find(a_coins, &sa) {
            (&t.a, c)
        } else if let Some(c) = find(b_coins, &sb) {
            (&t.b, c)
        } else {
            return Err("a coin in it is neither yours nor the other player's (or it is spent)".into());
        };
        let is_a = owner == &t.a;
        if n == 0 && !is_a {
            return Err("the player who signs first must have the first input".into());
        }
        let signer = crate::msgs::input_addr(i);
        if is_a || full {
            if signer.as_deref() != Some(owner.as_str()) {
                return Err(format!("an input of {owner} is not signed by {owner}"));
            }
        } else if !i.sig.is_empty() {
            return Err("an input of yours is already signed - not by you".into());
        }
        if is_a {
            a_in += coin.sats;
        } else {
            b_in += coin.sats;
            b_seen = true;
        }
        prev.push((Some(owner.clone()), coin.sats));
        inputs.push(json!({"txid": txid, "vout": vout, "script": coin.script, "amount": coin.sats}));
    }
    if !b_seen {
        return Err("the other player has no input in it, so it would not be their trade".into());
    }
    // die Nutzlast: genau die Absprache
    let (from, data) = crate::msgs::anydata(&tx, &crate::msgs::marker()).ok_or("there is no trade written in it")?;
    if from != t.a {
        return Err("the trade in it is not from the player who signs first".into());
    }
    let same = if t.bundle {
        let want = Bundle { a: t.a.clone(), b: t.b.clone(), give: t.a_gives.clone(), take: t.b_gives.clone() };
        !crate::msgs::is_class_c(&tx) && data.strip_prefix(b"INSC\x01\x08").and_then(parse_bundle) == Some(want)
    } else {
        crate::msgs::is_class_c(&tx) && swap_data(&t.a_gives[0], &t.b_gives[0]).as_deref() == Some(&data[..])
    };
    if !same {
        return Err("the trade written in it is not the one you agreed".into());
    }
    // wer wie viel bekommt
    let (mut a_out, mut b_out, mut total_out) = (0u64, 0u64, 0u64);
    let mut outputs = Vec::new();
    for (v, s) in &tx.outs {
        let hx = hex::encode(s);
        if hx == sa {
            a_out += v;
        } else if hx == sb {
            b_out += v;
        }
        total_out += v;
        outputs.push(json!({"value": v, "script": hx}));
    }
    let fee = (a_in + b_in).checked_sub(total_out).ok_or("it pays out more than it spends")?;
    if fee > MOST_FEE {
        return Err(format!("its fee of {} PEP is more than this arcade allows", pep(fee)));
    }
    let (a_net, b_net) = (a_out as i128 - a_in as i128, b_out as i128 - b_in as i128);
    let (ca, cb) = (coins_of(&t.a_gives) as i128, coins_of(&t.b_gives) as i128);
    let slack = |pays: bool| if pays { MOST_FEE as i128 } else { 0 };
    if b_net < ca - cb - slack(b_pays_fee(t)) {
        return Err(format!("{} would pay more than agreed", t.b));
    }
    if a_net < cb - ca - slack(!b_pays_fee(t)) {
        return Err(format!("{} would pay more than agreed", t.a));
    }
    // was der Index damit taete: beide Seiten bewegen sich, sonst nichts signieren
    let after = crate::appindex::simulate(owners, raw, &prev, h).ok_or("the index cannot read it")?;
    let (pa, pb) = (pieces_of(&t.a_gives), pieces_of(&t.b_gives));
    for (piece, owner) in &after {
        let want = if pa.contains(piece) { &t.b } else if pb.contains(piece) { &t.a } else { continue };
        if owner != want {
            return Err("this arcade's own index would not move both sides - nothing signed".into());
        }
    }
    if after.len() != pa.len() + pb.len() {
        return Err("an inscription of this trade is unknown here".into());
    }
    let tx_json = json!({"version": version, "locktime": locktime, "inputs": inputs, "outputs": outputs, "fee": fee});
    Ok(Checked { tx: tx_json, prev, fee, b_net })
}

/// Die beiden Haelften zusammenfuegen: die Signaturen von A aus `raw_a`, die von B aus `raw_b`
pub fn merge(t: &Terms, raw_a: &[u8], raw_b: &[u8], a_coins: &[Coin]) -> Result<Vec<u8>, String> {
    let (ta, tb) = (crate::utxo::parse_tx(raw_a).map_err(|e| e.to_string())?, crate::utxo::parse_tx(raw_b).map_err(|e| e.to_string())?);
    if ends(raw_a)? != ends(raw_b)? || ta.outs != tb.outs || ta.ins.len() != tb.ins.len() || ta.ins.iter().zip(&tb.ins).any(|(x, y)| x.prev != y.prev) {
        return Err("the two halves are not the same transaction".into());
    }
    let sa = script_of(&t.a);
    let ins: Vec<((Txid, u32), &[u8])> = ta
        .ins
        .iter()
        .zip(&tb.ins)
        .map(|(x, y)| {
            let of_a = a_coins.iter().any(|c| c.script == sa && c.vout == x.prev.1 && c.txid == shown(&x.prev.0));
            (x.prev, if of_a { x.sig } else { y.sig })
        })
        .collect();
    let (version, locktime) = ends(raw_a)?;
    Ok(serialize(version, locktime, &ins, &ta.outs))
}

// ------------------------------------------------------------------ Schnittstelle (public.rs, angemeldet)

fn coins_at(app: &App, addr: &str) -> Result<Vec<Coin>, (u16, String)> {
    let list = crate::utxo::coins(app, addr).map_err(|e| (503, e))?;
    Ok(list
        .iter()
        .filter_map(|c| {
            Some(Coin {
                txid: c["txid"].as_str()?.to_string(),
                vout: c["vout"].as_u64()? as u32,
                sats: c["sats"].as_u64()?,
                script: c["scriptPubKey"].as_str()?.to_string(),
            })
        })
        .collect())
}

fn owners_now(t: &Terms) -> Result<Vec<(String, String)>, (u16, String)> {
    pieces_of(&t.a_gives)
        .into_iter()
        .chain(pieces_of(&t.b_gives))
        .map(|p| crate::appindex::inscription(&p).map(|i| (p.clone(), i.owner)).ok_or((404, format!("inscription {} is unknown here", &p[..12]))))
        .collect()
}

fn hexbytes(b: &Value, k: &str) -> Result<Vec<u8>, (u16, String)> {
    let s = b[k].as_str().unwrap_or("");
    if s.is_empty() || s.len() > 40_000 {
        return Err((400, format!("{k} is missing")));
    }
    hex::decode(s).map_err(|_| (400, format!("{k} is not hex")))
}

/// POST /api/trades/<op> fuer das angemeldete Konto `me` (Adresse) mit @name `tag`.
/// build: {card, with, give, get, coinpub?}  (ich signiere zuerst)  -> {tx, fee, fee_by, net}
/// check: {card, with, give, get, raw}       (ich signiere als zweiter) -> {tx, fee, fee_by, net}
/// send:  {card, with, give, get, raw, raw_b} -> {txid}
/// give/get sind immer aus MEINER Sicht.
pub fn api(app: &App, me: &str, tag: Option<&str>, coinpub: Option<Vec<u8>>, op: &str, b: &Value) -> Result<Value, (u16, String)> {
    if tag.is_none_or(|t| t.is_empty()) {
        return Err((403, "a trade is for a player with an @name: claim one first".into()));
    }
    for k in ["chain", "network"] {
        if let Some(n) = b[k].as_str().filter(|n| !n.is_empty()) {
            if !matches!(n, "test" | "pep-test" | "pepecoin-test" | "pepetest") {
                return Err((400, "only Pepecoin testnet: this arcade does not trade on mainnet".into()));
            }
        }
    }
    let card = b["card"].as_str().unwrap_or("").to_ascii_lowercase();
    if crate::viewer::enabled_app(app, &card).is_none() {
        return Err((403, "this app is not enabled on this arcade".into()));
    }
    let peer = b["with"].as_str().unwrap_or("").trim().to_string();
    if !app.index.lock().unwrap().by_addr.contains_key(&peer) {
        return Err((403, "the other player has no @name: guests cannot trade".into()));
    }
    let i_sign_first = op == "build";
    let t = if i_sign_first { terms(me, &peer, &b["give"], &b["get"]) } else { terms(&peer, me, &b["get"], &b["give"]) }.map_err(|e| (400, e))?;
    let owners = owners_now(&t)?;
    for (p, o) in &owners {
        let giver = if pieces_of(&t.a_gives).contains(p) { &t.a } else { &t.b };
        if o != giver {
            return Err((409, format!("inscription {} does not belong to {giver} (any more)", &p[..12])));
        }
    }
    let (a_coins, b_coins) = (coins_at(app, &t.a)?, coins_at(app, &t.b)?);
    let fee_by = |mine_pays: bool| if mine_pays { "you" } else { "them" };
    match op {
        "build" => {
            let pk = coinpub.filter(|k| k.len() == 33 && crate::b58check(crate::PKH, &crate::hash160(k)) == me);
            let tx = assemble(&t, pk.as_deref(), &a_coins, &b_coins).map_err(|e| (400, e))?;
            let net = net_of(&tx, &crate::utxo::script_of(&crate::utxo::key_of(me).unwrap()));
            Ok(json!({"ok": true, "tx": tx, "fee": tx["fee"], "fee_by": fee_by(!b_pays_fee(&t)), "net": net.to_string(), "kind": if t.bundle { "bundle" } else { "swap" }}))
        }
        "check" | "send" => {
            let h = crate::appindex::progress().0 + 1;
            let raw = hexbytes(b, "raw")?;
            let c = verify(&t, &raw, &a_coins, &b_coins, &owners, h, false).map_err(|e| (400, e))?;
            if op == "check" {
                return Ok(json!({"ok": true, "tx": c.tx, "fee": c.fee, "fee_by": fee_by(b_pays_fee(&t)), "net": c.b_net.to_string(), "kind": if t.bundle { "bundle" } else { "swap" }}));
            }
            let raw_b = hexbytes(b, "raw_b")?;
            let whole = merge(&t, &raw, &raw_b, &a_coins).map_err(|e| (400, e))?;
            verify(&t, &whole, &a_coins, &b_coins, &owners, h, true).map_err(|e| (400, e))?;
            let txid = crate::rpc(&app.cfg, "sendrawtransaction", json!([hex::encode(&whole)])).map_err(|e| (502, format!("the node refused it: {e}")))?;
            let txid = txid.as_str().unwrap_or("").to_string();
            crate::log(&format!("Tausch {} gesendet: {txid} ({} <-> {})", if t.bundle { "Buendel" } else { "eins gegen eins" }, t.a, t.b));
            Ok(json!({"ok": true, "txid": txid}))
        }
        _ => Err((404, "trade ops are build, check and send".into())),
    }
}

/// Was die unsignierte Transaktion fuer das Skript `mine` netto bewegt (Satoshi, negativ = zahlt)
fn net_of(tx: &Value, mine: &str) -> i128 {
    let sum = |k: &str, v: &str| -> i128 {
        tx[k].as_array().map(|a| a.iter().filter(|x| x["script"] == mine).map(|x| x[v].as_u64().unwrap_or(0) as i128).sum()).unwrap_or(0)
    };
    sum("outputs", "value") - sum("inputs", "amount")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utxo::internal;
    use k256::ecdsa::SigningKey;

    const C: u64 = crate::COIN;
    const X: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const Y: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const Z: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    struct Who {
        sk: SigningKey,
        pk: Vec<u8>,
        addr: String,
    }
    fn who(n: u8) -> Who {
        let sk = SigningKey::from_slice(&[n; 32]).unwrap();
        let pk = sk.verifying_key().to_encoded_point(true).as_bytes().to_vec();
        let addr = crate::b58check(crate::PKH, &crate::hash160(&pk));
        Who { sk, pk, addr }
    }
    fn coin(w: &Who, n: u8, sats: u64) -> Coin {
        Coin { txid: hex::encode([n; 32]), vout: n as u32, sats, script: script_of(&w.addr) }
    }

    /// Wie crypto.js signTxPart im Browser: nur die Inputs mit dem eigenen Skript, SIGHASH_ALL, die anderen bleiben
    /// wie sie sind (leer oder schon signiert)
    fn sign_part(tx: &Value, w: &Who, have: Option<&[u8]>) -> Vec<u8> {
        let mine = script_of(&w.addr);
        let ins = tx["inputs"].as_array().unwrap();
        let outs: Vec<(u64, Vec<u8>)> = tx["outputs"].as_array().unwrap().iter().map(|o| (o["value"].as_u64().unwrap(), hex::decode(o["script"].as_str().unwrap()).unwrap())).collect();
        let outs_ref: Vec<(u64, &[u8])> = outs.iter().map(|(v, s)| (*v, s.as_slice())).collect();
        let prevs: Vec<(Txid, u32)> = ins.iter().map(|i| (internal(i["txid"].as_str().unwrap()).unwrap(), i["vout"].as_u64().unwrap() as u32)).collect();
        let old: Vec<Vec<u8>> = match have {
            Some(raw) => crate::utxo::parse_tx(raw).unwrap().ins.iter().map(|i| i.sig.to_vec()).collect(),
            None => vec![vec![]; ins.len()],
        };
        let mut sigs = old.clone();
        for (n, i) in ins.iter().enumerate() {
            if i["script"].as_str() != Some(mine.as_str()) {
                continue;
            }
            let spk = hex::decode(&mine).unwrap();
            let scripts: Vec<((Txid, u32), &[u8])> = prevs.iter().enumerate().map(|(k, p)| (*p, if k == n { &spk[..] } else { &[][..] })).collect();
            let mut pre = serialize(1, 0, &scripts, &outs_ref);
            pre.extend_from_slice(&1u32.to_le_bytes());
            let h = crate::dsha(&pre);
            let (sig, _) = w.sk.sign_prehash_recoverable(&h).unwrap();
            let der = sig.normalize_s().unwrap_or(sig).to_der();
            let mut s = vec![der.as_bytes().len() as u8 + 1];
            s.extend_from_slice(der.as_bytes());
            s.push(1);
            s.push(w.pk.len() as u8);
            s.extend_from_slice(&w.pk);
            sigs[n] = s;
        }
        let scripts: Vec<((Txid, u32), &[u8])> = prevs.iter().zip(&sigs).map(|(p, s)| (*p, &s[..])).collect();
        serialize(1, 0, &scripts, &outs_ref)
    }

    fn owners(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(t, o)| (t.to_string(), o.to_string())).collect()
    }

    /// Eins gegen eins (Inschrift gegen Coins): gebaut, beide signieren, der Index bewegt beide Seiten
    #[test]
    fn swap_built_signed_and_read_by_the_index() {
        let (a, b) = (who(1), who(2));
        let t = terms(&a.addr, &b.addr, &json!({"inscription": X}), &json!({"coins": "5"})).unwrap();
        let (ac, bc) = (vec![coin(&a, 1, C)], vec![coin(&b, 2, 3 * C), coin(&b, 3, 4 * C)]);
        let tx = assemble(&t, None, &ac, &bc).unwrap();
        // A vorn, B zahlt 5 und die Gebuehr (gibt Coins)
        assert_eq!(tx["inputs"][0]["script"], script_of(&a.addr));
        assert_eq!(tx["inputs"].as_array().unwrap().len(), 3);
        assert_eq!(net_of(&tx, &script_of(&a.addr)), 5 * C as i128);
        let own = owners(&[(X, &a.addr)]);
        let half = sign_part(&tx, &a, None);
        let c = verify(&t, &half, &ac, &bc, &own, 1_600_000, false).unwrap();
        assert_eq!(c.b_net, -(5 * C as i128) - c.fee as i128);
        // B signiert dieselbe Transaktion (aus der Pruefung neu aufgebaut) und fuegt zusammen
        let mine_b = sign_part(&c.tx, &b, Some(&half));
        let whole = merge(&t, &half, &mine_b, &ac).unwrap();
        verify(&t, &whole, &ac, &bc, &own, 1_600_000, true).unwrap();
        let after = crate::appindex::simulate(&own, &whole, &c.prev, 1_600_000).unwrap();
        assert_eq!(after[0].1, b.addr);
        // die Haelfte von A allein (B hat nicht signiert): unvollstaendig
        assert!(verify(&t, &half, &ac, &bc, &own, 1_600_000, true).is_err());
    }

    /// Inschrift gegen Inschrift: A zahlt die Gebuehr, B bekommt seinen Coin ganz zurueck
    #[test]
    fn swap_piece_for_piece() {
        let (a, b) = (who(1), who(2));
        let t = terms(&a.addr, &b.addr, &json!({"inscription": X}), &json!({"inscription": Y})).unwrap();
        let (ac, bc) = (vec![coin(&a, 1, C)], vec![coin(&b, 2, DUST)]);
        let tx = assemble(&t, None, &ac, &bc).unwrap();
        assert_eq!(net_of(&tx, &script_of(&b.addr)), 0);
        let own = owners(&[(X, &a.addr), (Y, &b.addr)]);
        let half = sign_part(&tx, &a, None);
        let c = verify(&t, &half, &ac, &bc, &own, 1_600_000, false).unwrap();
        let whole = merge(&t, &half, &sign_part(&c.tx, &b, Some(&half)), &ac).unwrap();
        let after = crate::appindex::simulate(&own, &whole, &c.prev, 1_600_000).unwrap();
        assert_eq!((after[0].1.as_str(), after[1].1.as_str()), (b.addr.as_str(), a.addr.as_str()));
    }

    /// Eine falsche Partei: ein Dritter statt B, eine andere Absprache, ein fremder Coin - alles abgewiesen
    #[test]
    fn wrong_party_is_refused() {
        let (a, b, o) = (who(1), who(2), who(3));
        let t = terms(&a.addr, &b.addr, &json!({"inscription": X}), &json!({"inscription": Y})).unwrap();
        let (ac, bc, oc) = (vec![coin(&a, 1, C)], vec![coin(&b, 2, C)], vec![coin(&o, 4, C)]);
        let own = owners(&[(X, &a.addr), (Y, &b.addr)]);
        // gebaut fuer A <-> Dritter (der Dritte hat Y nicht): B prueft gegen SEINE Absprache -> falsche Coins
        let t_o = Terms { b: o.addr.clone(), ..t.clone() };
        let tx_o = assemble(&t_o, None, &ac, &oc).unwrap();
        let half_o = sign_part(&tx_o, &a, None);
        assert!(verify(&t, &half_o, &ac, &bc, &own, 1_600_000, false).unwrap_err().contains("neither yours"));
        // und der Index selbst bewegt nichts, wenn der Kaeufer Y nicht besitzt
        let whole_o = sign_part(&tx_o, &o, Some(&half_o));
        let prev: Vec<(Option<String>, u64)> = vec![(Some(a.addr.clone()), C), (Some(o.addr.clone()), C)];
        let after = crate::appindex::simulate(&own, &whole_o, &prev, 1_600_000).unwrap();
        assert_eq!((after[0].1.as_str(), after[1].1.as_str()), (a.addr.as_str(), b.addr.as_str()));
        // eine andere Inschrift als abgesprochen
        let t_z = terms(&a.addr, &b.addr, &json!({"inscription": X}), &json!({"inscription": Z})).unwrap();
        let half_z = sign_part(&assemble(&t_z, None, &ac, &bc).unwrap(), &a, None);
        assert!(verify(&t, &half_z, &ac, &bc, &own, 1_600_000, false).unwrap_err().contains("not the one you agreed"));
        // B steht vorn statt A
        let t_rev = terms(&b.addr, &a.addr, &json!({"inscription": Y}), &json!({"inscription": X})).unwrap();
        let tx_rev = assemble(&t_rev, None, &bc, &ac).unwrap();
        let half_rev = sign_part(&tx_rev, &b, None);
        assert!(verify(&t, &half_rev, &ac, &bc, &own, 1_600_000, false).is_err());
        // X gehoert A nicht (mehr): der Probelauf des Index weist ab
        let other = owners(&[(X, &o.addr), (Y, &b.addr)]);
        let half = sign_part(&assemble(&t, None, &ac, &bc).unwrap(), &a, None);
        assert!(verify(&t, &half, &ac, &bc, &other, 1_600_000, false).unwrap_err().contains("would not move"));
    }

    /// Buendel (Class B): zwei Inschriften und Coins gegen eine Inschrift; nur die benannten Parteien
    #[test]
    fn bundle_built_signed_and_read_by_the_index() {
        let (a, b, o) = (who(1), who(2), who(3));
        let t = terms(&a.addr, &b.addr, &json!([{"inscription": X}, {"inscription": Z}]), &json!([{"inscription": Y}, {"coins": "2"}])).unwrap();
        assert!(t.bundle);
        let (ac, bc) = (vec![coin(&a, 1, C)], vec![coin(&b, 2, 5 * C)]);
        assert!(assemble(&t, None, &ac, &bc).is_err()); // ohne Pubkey kein Class B
        let tx = assemble(&t, Some(&a.pk), &ac, &bc).unwrap();
        let own = owners(&[(X, &a.addr), (Z, &a.addr), (Y, &b.addr)]);
        let at = crate::appindex::BUNDLES_FROM;
        let half = sign_part(&tx, &a, None);
        let c = verify(&t, &half, &ac, &bc, &own, at, false).unwrap();
        let whole = merge(&t, &half, &sign_part(&c.tx, &b, Some(&half)), &ac).unwrap();
        verify(&t, &whole, &ac, &bc, &own, at, true).unwrap();
        let after = crate::appindex::simulate(&own, &whole, &c.prev, at).unwrap();
        assert_eq!(after.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), vec![b.addr.as_str(), b.addr.as_str(), a.addr.as_str()]);
        // ein Dritter als b: der Index weist das Buendel ab (b hat nicht signiert)
        let prev_o = vec![(Some(a.addr.clone()), C), (Some(o.addr.clone()), 5 * C)];
        let after = crate::appindex::simulate(&own, &whole, &prev_o, at).unwrap();
        assert_eq!(after[0].1, a.addr);
    }

    #[test]
    fn terms_refuse_tokens_mainnet_and_guests_of_logic() {
        let (a, b) = (who(1), who(2));
        assert!(terms(&a.addr, &b.addr, &json!({"inscription": X}), &json!({"token": 5, "amount": "1"})).unwrap_err().contains("token ledger"));
        assert!(terms(&a.addr, &b.addr, &json!({"coins": "1"}), &json!({"inscription": X})).is_err());
        assert!(terms(&a.addr, &a.addr, &json!({"inscription": X}), &json!({"coins": "1"})).is_err());
        assert!(terms(&a.addr, &b.addr, &json!([{"coins": "1"}]), &json!([{"coins": "1"}, {"inscription": X}])).unwrap_err().contains("one way"));
        // Pepecoin-Mainnet-Adresse (Version 56 'P')
        let main = crate::b58check(56, &[1; 20]);
        assert!(terms(&main, &b.addr, &json!({"inscription": X}), &json!({"coins": "1"})).unwrap_err().contains("mainnet"));
    }
}
