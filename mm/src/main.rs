//! arcadeos-mm: Solo-Stratum-Server fuer Merged Mining. Parent ist Litecoin (getblocktemplate mit segwit+mweb),
//! Aux-Chains (Dogecoin, Pepecoin, ...) kommen per createauxblock/submitauxblock dazu. Jeder Share wird hier mit
//! scrypt geprueft; reicht er fuer eine Chain, geht der Block bzw. AuxPoW sofort an deren Knoten.
//!
//! Damit der Server nie der Flaschenhals ist: neue Bloecke kommen per ZMQ (Polling nur als Rueckfall), Arbeit wird
//! einmal pro Vorlage gebaut und an alle Miner verteilt, ein Share kostet einen scrypt-Hash (~0,1 ms), Vardiff haelt
//! jeden Miner bei etwa einem Share alle paar Sekunden, Einreichungen an die Knoten laufen in eigenen Faeden.
//!
//! Konfiguration: Datei (Vorgabe /state/mm/mm.conf, sonst MM_CONF), Zeilen `schluessel=wert`:
//!   bind=0.0.0.0:3333            Stratum
//!   status=/run/mm/status.json   Statusdatei (Monitor)
//!   tag=Mono was here            Kennung in der Coinbase (hoechstens etwa 40 Zeichen)
//!   diff=16  mindiff=0.01  maxdiff=1e9  shareinterval=5   Vardiff (Sekunden zwischen Shares)
//!   parent=litecoin,127.0.0.1:9332,<auth>,127.0.0.1:28332,<LTC-Adresse>
//!   aux=dogecoin,127.0.0.1:22555,<auth>,127.0.0.1:28532,<DOGE-Adresse>
//!   aux=pepecoin,127.0.0.1:33873,<auth>,127.0.0.1:28632,<PEP-Adresse>
//! <auth> ist `user:passwort` oder `cookie:/pfad/.cookie`; ZMQ-Adresse darf `-` sein.

mod zmq;

use mm::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EN1_SIZE: usize = 4;
const EN2_SIZE: usize = 4;
const KEEP_JOBS: usize = 16;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn log(msg: &str) {
    eprintln!("{msg}");
}

// ---------------------------------------------------------------- Konfiguration

#[derive(Clone)]
struct Chain {
    name: String,
    rpc: Rpc,
    zmq: Option<String>,
    address: String,
}

struct Cfg {
    bind: String,
    status: String,
    tag: String,
    diff: f64,
    min_diff: f64,
    max_diff: f64,
    share_interval: f64,
    parent: Chain,
    aux: Vec<Chain>,
}

fn parse_chain(v: &str) -> Result<Chain, String> {
    let p: Vec<&str> = v.split(',').map(|s| s.trim()).collect();
    if p.len() != 5 {
        return Err(format!("Chain braucht name,rpc,auth,zmq,adresse: {v}"));
    }
    let auth = match p[2].strip_prefix("cookie:") {
        Some(f) => Auth::Cookie(f.to_string()),
        None => Auth::Pass(p[2].to_string()),
    };
    if p[4].is_empty() {
        return Err(format!("{}: Auszahlungsadresse fehlt", p[0]));
    }
    Ok(Chain {
        name: p[0].to_string(),
        rpc: Rpc { addr: p[1].to_string(), auth },
        zmq: if p[3] == "-" || p[3].is_empty() { None } else { Some(p[3].to_string()) },
        address: p[4].to_string(),
    })
}

fn load_cfg() -> Result<Cfg, String> {
    let path = std::env::var("MM_CONF").unwrap_or_else(|_| "/state/mm/mm.conf".into());
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut kv: HashMap<String, String> = HashMap::new();
    let mut aux = Vec::new();
    let mut parent = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line.split_once('=').ok_or_else(|| format!("{path}: Zeile ohne '=': {line}"))?;
        match k.trim() {
            "aux" => aux.push(parse_chain(v)?),
            "parent" => parent = Some(parse_chain(v)?),
            k => {
                kv.insert(k.to_string(), v.trim().to_string());
            }
        }
    }
    let num = |k: &str, d: f64| -> Result<f64, String> {
        kv.get(k).map(|v| v.parse::<f64>().map_err(|e| format!("{k}: {e}"))).unwrap_or(Ok(d))
    };
    let parent = parent.ok_or("parent=... fehlt")?;
    // scriptSig der Coinbase: hoechstens 100 Byte (Hoehe, Merge-Mining-Eintrag, Extranonce, Kennung)
    if kv.get("tag").map_or(false, |t| t.len() > 40) {
        return Err("tag: hoechstens 40 Zeichen".into());
    }
    ltc_address_script(&parent.address)?;
    Ok(Cfg {
        bind: kv.get("bind").cloned().unwrap_or_else(|| "0.0.0.0:3333".into()),
        status: kv.get("status").cloned().unwrap_or_else(|| "/run/mm/status.json".into()),
        tag: kv.get("tag").cloned().unwrap_or_else(|| "Mono was here".into()),
        diff: num("diff", 16.0)?,
        min_diff: num("mindiff", 0.01)?,
        max_diff: num("maxdiff", 1e12)?,
        share_interval: num("shareinterval", 5.0)?,
        parent,
        aux,
    })
}

// ---------------------------------------------------------------- Arbeit

#[derive(Clone)]
struct AuxWork {
    chain: usize,
    hash_display: String,
    hash: H256,
    chain_id: u32,
    target: H256,
    height: u64,
}

struct AuxSlot {
    work: AuxWork,
    index: u32,
    branch: Vec<H256>,
}

struct ParentTpl {
    prev: H256,
    version: u32,
    bits: u32,
    curtime: u32,
    height: u64,
    value: u64,
    target: H256,
    commitment: Option<Vec<u8>>,
    txids: Vec<H256>,
    txdata: Vec<Vec<u8>>,
    hogex_last: bool,
    mweb: Option<Vec<u8>>,
}

struct Job {
    id: String,
    tpl: Arc<ParentTpl>,
    coinb1: Vec<u8>,
    coinb2: Vec<u8>,
    branch: Vec<H256>,
    aux: Vec<AuxSlot>,
    clean: bool,
}

impl Job {
    fn notify(&self, clean: bool) -> String {
        let branch: Vec<String> = self.branch.iter().map(hex::encode).collect();
        json!({"id": null, "method": "mining.notify", "params": [
            self.id, stratum_prevhash(&self.tpl.prev), hex::encode(&self.coinb1), hex::encode(&self.coinb2), branch,
            format!("{:08x}", self.tpl.version), format!("{:08x}", self.tpl.bits), format!("{:08x}", self.tpl.curtime), clean
        ]})
        .to_string()
    }
}

/// Ist die Transaktion eine HogEx (Litecoin 0.21: Marker 0, Flags mit Bit 8)?
fn is_hogex(tx: &[u8]) -> bool {
    tx.len() > 6 && tx[4] == 0 && tx[5] & 8 != 0
}

fn get_template(c: &Chain) -> Result<ParentTpl, String> {
    let t = c.rpc.call("getblocktemplate", json!([{"rules": ["mweb", "segwit"]}]))?;
    let hx = |k: &str| t[k].as_str().map(str::to_string).ok_or_else(|| format!("getblocktemplate: {k} fehlt"));
    let mut txids = Vec::new();
    let mut txdata = Vec::new();
    for tx in t["transactions"].as_array().ok_or("getblocktemplate: transactions")? {
        txids.push(from_display(tx["txid"].as_str().ok_or("txid")?)?);
        txdata.push(hex::decode(tx["data"].as_str().ok_or("data")?).map_err(|e| e.to_string())?);
    }
    let hogex_last = txdata.last().map_or(false, |d| is_hogex(d));
    let bits = u32::from_str_radix(&hx("bits")?, 16).map_err(|e| e.to_string())?;
    Ok(ParentTpl {
        prev: from_display(&hx("previousblockhash")?)?,
        version: t["version"].as_u64().ok_or("version")? as u32,
        bits,
        curtime: t["curtime"].as_u64().ok_or("curtime")? as u32,
        height: t["height"].as_u64().ok_or("height")?,
        value: t["coinbasevalue"].as_u64().ok_or("coinbasevalue")?,
        target: bits_to_target(bits),
        commitment: t["default_witness_commitment"].as_str().map(|s| hex::decode(s).unwrap_or_default()),
        txids,
        txdata,
        hogex_last,
        mweb: t["mweb"].as_str().map(|s| hex::decode(s).unwrap_or_default()),
    })
}

fn get_aux(i: usize, c: &Chain) -> Result<AuxWork, String> {
    let a = c.rpc.call("createauxblock", json!([c.address]))?;
    let hash_display = a["hash"].as_str().ok_or("createauxblock: hash")?.to_string();
    let bits = u32::from_str_radix(a["bits"].as_str().ok_or("createauxblock: bits")?, 16).map_err(|e| e.to_string())?;
    Ok(AuxWork {
        chain: i,
        hash: from_display(&hash_display)?,
        hash_display,
        chain_id: a["chainid"].as_u64().ok_or("createauxblock: chainid")? as u32,
        target: bits_to_target(bits),
        height: a["height"].as_u64().unwrap_or(0),
    })
}

/// Merge-Mining-Baum: kleinste Groesse und Nonce, bei der jede Aux-Chain ihren eigenen Platz hat.
fn aux_tree(aux: &[AuxWork]) -> (Vec<AuxSlot>, H256, u32, u32) {
    if aux.is_empty() {
        return (Vec::new(), [0; 32], 1, 0);
    }
    for h in 0..8u32 {
        if (1usize << h) < aux.len() {
            continue;
        }
        for nonce in 0..1000u32 {
            let slots: Vec<u32> = aux.iter().map(|a| aux_slot(nonce, a.chain_id, h)).collect();
            let distinct: HashSet<u32> = slots.iter().copied().collect();
            if distinct.len() != slots.len() {
                continue;
            }
            let mut leaves = vec![[0u8; 32]; 1 << h];
            for (a, s) in aux.iter().zip(&slots) {
                leaves[*s as usize] = a.hash;
            }
            let root = merkle_root(&leaves);
            let out = aux
                .iter()
                .zip(&slots)
                .map(|(a, s)| AuxSlot { work: a.clone(), index: *s, branch: merkle_branch(&leaves, *s as usize) })
                .collect();
            return (out, root, 1 << h, nonce);
        }
    }
    (Vec::new(), [0; 32], 1, 0)
}

fn build_job(id: String, tpl: Arc<ParentTpl>, aux: &[AuxWork], payout: &[u8], tag: &str, clean: bool) -> Job {
    let (slots, root, size, mm_nonce) = aux_tree(aux);
    let mut script = Vec::new();
    push_height(&mut script, tpl.height);
    if !slots.is_empty() {
        let mut mm = vec![0xfa, 0xbe, 0x6d, 0x6d];
        let mut r = root;
        r.reverse(); // die Wurzel steht umgedreht in der Coinbase (auxpow.cpp)
        mm.extend_from_slice(&r);
        mm.extend_from_slice(&size.to_le_bytes());
        mm.extend_from_slice(&mm_nonce.to_le_bytes());
        push_data(&mut script, &mm);
    }
    script.push((EN1_SIZE + EN2_SIZE) as u8); // Push der Extranonce
    let en_at = script.len();
    let mut tail = Vec::new();
    push_data(&mut tail, tag.as_bytes());
    let script_len = en_at + EN1_SIZE + EN2_SIZE + tail.len();

    let mut coinb1 = Vec::new();
    coinb1.extend_from_slice(&1u32.to_le_bytes());
    coinb1.push(1);
    coinb1.extend_from_slice(&[0u8; 32]);
    coinb1.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    put_varint(&mut coinb1, script_len as u64);
    coinb1.extend_from_slice(&script);

    let mut coinb2 = tail;
    coinb2.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    let outs = 1 + tpl.commitment.is_some() as u64;
    put_varint(&mut coinb2, outs);
    coinb2.extend_from_slice(&tpl.value.to_le_bytes());
    put_varint(&mut coinb2, payout.len() as u64);
    coinb2.extend_from_slice(payout);
    if let Some(c) = &tpl.commitment {
        coinb2.extend_from_slice(&0u64.to_le_bytes());
        put_varint(&mut coinb2, c.len() as u64);
        coinb2.extend_from_slice(c);
    }
    coinb2.extend_from_slice(&0u32.to_le_bytes());

    let mut leaves = vec![[0u8; 32]];
    leaves.extend_from_slice(&tpl.txids);
    let branch = merkle_branch_first(&leaves);
    Job { id, tpl, coinb1, coinb2, branch, aux: slots, clean }
}

/// Voller Block fuer submitblock: Kopf, Coinbase (mit Witness, wenn die Vorlage eine Commitment hat),
/// die Transaktionen der Vorlage und dahinter der MWEB-Block.
fn assemble_block(job: &Job, header: &[u8; 80], coinbase: &[u8]) -> Vec<u8> {
    let tpl = &job.tpl;
    let mut b = header.to_vec();
    put_varint(&mut b, 1 + tpl.txdata.len() as u64);
    if tpl.commitment.is_some() {
        // version | 00 01 | vin+vout | witness (32 Nullbytes) | locktime
        b.extend_from_slice(&coinbase[..4]);
        b.extend_from_slice(&[0, 1]);
        b.extend_from_slice(&coinbase[4..coinbase.len() - 4]);
        b.extend_from_slice(&[1, 32]);
        b.extend_from_slice(&[0u8; 32]);
        b.extend_from_slice(&coinbase[coinbase.len() - 4..]);
    } else {
        b.extend_from_slice(coinbase);
    }
    for t in &tpl.txdata {
        b.extend_from_slice(t);
    }
    if tpl.hogex_last {
        match &tpl.mweb {
            Some(m) => {
                b.push(1);
                b.extend_from_slice(m);
            }
            None => b.push(0),
        }
    }
    b
}

/// AuxPoW (CAuxPow: CMerkleTx der Parent-Coinbase, Zweig im Merge-Mining-Baum, Parent-Kopf).
fn auxpow(job: &Job, slot: &AuxSlot, header: &[u8; 80], coinbase: &[u8]) -> Vec<u8> {
    let mut a = coinbase.to_vec();
    a.extend_from_slice(&sha256d(header));
    put_varint(&mut a, job.branch.len() as u64);
    for h in &job.branch {
        a.extend_from_slice(h);
    }
    a.extend_from_slice(&0i32.to_le_bytes());
    put_varint(&mut a, slot.branch.len() as u64);
    for h in &slot.branch {
        a.extend_from_slice(h);
    }
    a.extend_from_slice(&(slot.index as i32).to_le_bytes());
    a.extend_from_slice(header);
    a
}

// ---------------------------------------------------------------- Gemeinsamer Zustand

#[derive(Default)]
struct Stats {
    accepted: AtomicU64,
    rejected: AtomicU64,
    /// (Zeit, Schwierigkeit) akzeptierter Shares, fuer die Hashrate
    window: Mutex<VecDeque<(Instant, f64)>>,
    found: Mutex<Vec<Value>>,
    best: Mutex<f64>,
}

struct Client {
    id: u64,
    out: Mutex<TcpStream>,
    en1: [u8; EN1_SIZE],
    diff: Mutex<f64>,
    prev_diff: Mutex<f64>,
    worker: Mutex<String>,
    shares: Mutex<VecDeque<Instant>>,
    subscribed: Mutex<bool>,
}

impl Client {
    fn send(&self, line: &str) -> bool {
        let mut s = self.out.lock().unwrap();
        s.write_all(line.as_bytes()).and_then(|_| s.write_all(b"\n")).is_ok()
    }
}

struct State {
    cfg: Cfg,
    payout: Vec<u8>,
    jobs: RwLock<VecDeque<Arc<Job>>>,
    clients: Mutex<Vec<Arc<Client>>>,
    seen: Mutex<HashSet<H256>>,
    stats: Stats,
    next_job: AtomicU64,
    en1_counter: AtomicU32,
    aux_heights: Mutex<Vec<u64>>,
}

impl State {
    fn current(&self) -> Option<Arc<Job>> {
        self.jobs.read().unwrap().back().cloned()
    }

    fn publish(&self, job: Job) {
        let clean = job.clean;
        let job = Arc::new(job);
        {
            let mut jobs = self.jobs.write().unwrap();
            if clean {
                jobs.clear();
                self.seen.lock().unwrap().clear();
            }
            jobs.push_back(job.clone());
            while jobs.len() > KEEP_JOBS {
                jobs.pop_front();
            }
        }
        let line = job.notify(clean);
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|c| !*c.subscribed.lock().unwrap() || c.send(&line));
    }
}

// ---------------------------------------------------------------- Vorlagen-Faden

fn work_loop(st: Arc<State>, rx: mpsc::Receiver<String>) {
    let parent_name = st.cfg.parent.name.clone();
    let mut tpl: Option<Arc<ParentTpl>> = None;
    let mut aux: Vec<Option<AuxWork>> = vec![None; st.cfg.aux.len()];
    let mut last_tpl = Instant::now() - Duration::from_secs(3600);
    let mut last_aux = vec![Instant::now() - Duration::from_secs(3600); st.cfg.aux.len()];
    let mut last_best = String::new();
    let mut last_check = Instant::now() - Duration::from_secs(3600);
    loop {
        // Ereignis abwarten (ZMQ) oder spaetestens nach 1 s selbst nachsehen
        let mut events: Vec<String> = Vec::new();
        if let Ok(e) = rx.recv_timeout(Duration::from_secs(1)) {
            events.push(e);
            while let Ok(e) = rx.try_recv() {
                events.push(e);
            }
        }
        let mut new_parent = events.iter().any(|e| *e == parent_name);
        // Rueckfall ohne ZMQ: Tip alle 2 s vergleichen
        if last_check.elapsed() >= Duration::from_secs(2) {
            last_check = Instant::now();
            match st.cfg.parent.rpc.call("getbestblockhash", json!([])) {
                Ok(v) => {
                    let b = v.as_str().unwrap_or("").to_string();
                    if b != last_best {
                        last_best = b;
                        new_parent = true;
                    }
                }
                Err(e) => log(&format!("{parent_name}: {e}")),
            }
        }
        let mut changed = false;
        let mut clean = false;
        if new_parent || tpl.is_none() || last_tpl.elapsed() >= Duration::from_secs(30) {
            match get_template(&st.cfg.parent) {
                Ok(t) => {
                    let tip_changed = tpl.as_ref().map_or(true, |o| o.prev != t.prev);
                    clean |= tip_changed;
                    if tip_changed {
                        log(&format!("{parent_name}: neue Arbeit fuer Block {} ({} Transaktionen)", t.height, t.txids.len()));
                    }
                    tpl = Some(Arc::new(t));
                    changed = true;
                }
                Err(e) => log(&format!("{parent_name}: getblocktemplate: {e}")),
            }
            last_tpl = Instant::now();
        }
        for (i, c) in st.cfg.aux.iter().enumerate() {
            let hit = events.iter().any(|e| *e == c.name);
            if hit || aux[i].is_none() || last_aux[i].elapsed() >= Duration::from_secs(10) {
                last_aux[i] = Instant::now();
                match get_aux(i, c) {
                    Ok(a) => {
                        let differs = aux[i].as_ref().map_or(true, |o| o.hash != a.hash);
                        if differs {
                            let new_tip = aux[i].as_ref().map_or(true, |o| o.height != a.height);
                            if new_tip {
                                log(&format!("{}: neue Arbeit fuer Block {}", c.name, a.height));
                                clean = true;
                            }
                            st.aux_heights.lock().unwrap()[i] = a.height;
                            aux[i] = Some(a);
                            changed = true;
                        }
                    }
                    Err(e) => {
                        // Knoten noch im Sync oder nicht erreichbar: ohne diese Chain weiterminen
                        if aux[i].take().is_some() {
                            changed = true;
                        }
                        log(&format!("{}: createauxblock: {e}", c.name));
                    }
                }
            }
        }
        if changed {
            if let Some(t) = &tpl {
                let works: Vec<AuxWork> = aux.iter().flatten().cloned().collect();
                let id = format!("{:x}", st.next_job.fetch_add(1, Ordering::Relaxed));
                st.publish(build_job(id, t.clone(), &works, &st.payout, &st.cfg.tag, clean));
            }
        }
    }
}

// ---------------------------------------------------------------- Shares

fn hexu32(v: &Value) -> Option<u32> {
    u32::from_str_radix(v.as_str()?, 16).ok()
}

fn submit_share(st: &Arc<State>, c: &Client, p: &Value) -> Result<(), (i32, &'static str)> {
    let job_id = p[1].as_str().ok_or((20, "bad params"))?;
    let en2 = hex::decode(p[2].as_str().ok_or((20, "bad params"))?).map_err(|_| (20, "bad extranonce2"))?;
    if en2.len() != EN2_SIZE {
        return Err((20, "bad extranonce2 size"));
    }
    let ntime = hexu32(&p[3]).ok_or((20, "bad ntime"))?;
    let nonce = hexu32(&p[4]).ok_or((20, "bad nonce"))?;
    let job = st.jobs.read().unwrap().iter().rev().find(|j| j.id == job_id).cloned().ok_or((21, "job not found"))?;
    if ntime < job.tpl.curtime || ntime as u64 > now() + 7200 {
        return Err((20, "ntime out of range"));
    }
    let (coinbase, root) = coinbase_root(&job.coinb1, &c.en1, &en2, &job.coinb2, &job.branch);
    let header = build_header(job.tpl.version, &job.tpl.prev, &root, ntime, job.tpl.bits, nonce);
    let hash = scrypt_pow(&header);
    if !st.seen.lock().unwrap().insert(hash) {
        return Err((22, "duplicate share"));
    }
    let diff = *c.diff.lock().unwrap();
    let prev_diff = *c.prev_diff.lock().unwrap();
    let credited = if meets(&hash, &diff_to_target(diff)) {
        diff
    } else if meets(&hash, &diff_to_target(prev_diff)) {
        prev_diff // kurz nach einer Vardiff-Aenderung
    } else {
        return Err((23, "low difficulty share"));
    };
    {
        let hd = hash_diff(&hash);
        let mut best = st.stats.best.lock().unwrap();
        if hd > *best {
            *best = hd;
        }
    }
    let worker = c.worker.lock().unwrap().clone();
    // Parent-Block?
    if meets(&hash, &job.tpl.target) {
        let block = hex::encode(assemble_block(&job, &header, &coinbase));
        let st2 = st.clone();
        let height = job.tpl.height;
        let bhash = to_display(&sha256d(&header));
        std::thread::spawn(move || {
            let r = st2.cfg.parent.rpc.call("submitblock", json!([block]));
            report_found(&st2, &st2.cfg.parent.name, height, &bhash, &worker_or(&r), r.is_ok() && r.as_ref().unwrap().is_null());
        });
    }
    // Aux-Bloecke?
    for slot in &job.aux {
        if meets(&hash, &slot.work.target) {
            let pow = hex::encode(auxpow(&job, slot, &header, &coinbase));
            let st2 = st.clone();
            let chain = st.cfg.aux[slot.work.chain].clone();
            let (h, height) = (slot.work.hash_display.clone(), slot.work.height);
            std::thread::spawn(move || {
                let r = chain.rpc.call("submitauxblock", json!([h, pow]));
                let ok = matches!(r, Ok(Value::Bool(true)));
                report_found(&st2, &chain.name, height, &h, &worker_or(&r), ok);
            });
        }
    }
    let _ = worker;
    st.stats.accepted.fetch_add(1, Ordering::Relaxed);
    let t = Instant::now();
    {
        let mut w = st.stats.window.lock().unwrap();
        w.push_back((t, credited));
        while w.front().map_or(false, |(x, _)| t.duration_since(*x) > Duration::from_secs(600)) {
            w.pop_front();
        }
    }
    c.shares.lock().unwrap().push_back(t);
    Ok(())
}

fn worker_or(r: &Result<Value, String>) -> String {
    match r {
        Ok(v) => v.to_string(),
        Err(e) => e.clone(),
    }
}

fn report_found(st: &State, chain: &str, height: u64, hash: &str, answer: &str, ok: bool) {
    log(&format!("BLOCK {chain} {height} {hash}: {} ({answer})", if ok { "angenommen" } else { "abgelehnt" }));
    st.stats.found.lock().unwrap().push(json!({"chain": chain, "height": height, "hash": hash, "ok": ok, "answer": answer, "t": now()}));
}

/// Vardiff: Schwierigkeit so anpassen, dass ein Miner etwa alle `share_interval` Sekunden einen Share schickt.
fn retarget(st: &State, c: &Client) {
    let interval = st.cfg.share_interval;
    let (count, span) = {
        let mut s = c.shares.lock().unwrap();
        let now = Instant::now();
        while s.front().map_or(false, |t| now.duration_since(*t) > Duration::from_secs(120)) {
            s.pop_front();
        }
        if s.len() < 4 {
            return;
        }
        (s.len(), now.duration_since(*s.front().unwrap()).as_secs_f64().max(1.0))
    };
    let rate = (count - 1) as f64 / span; // Shares pro Sekunde
    let cur = *c.diff.lock().unwrap();
    let want = (cur * rate * interval).clamp(st.cfg.min_diff, st.cfg.max_diff);
    if want > cur * 2.0 || want < cur / 2.0 {
        set_diff(st, c, want);
        c.shares.lock().unwrap().clear();
    }
}

fn set_diff(st: &State, c: &Client, d: f64) {
    {
        let mut cur = c.diff.lock().unwrap();
        *c.prev_diff.lock().unwrap() = *cur;
        *cur = d;
    }
    c.send(&json!({"id": null, "method": "mining.set_difficulty", "params": [d]}).to_string());
    if let Some(j) = st.current() {
        c.send(&j.notify(false));
    }
}

// ---------------------------------------------------------------- Stratum-Verbindungen

fn serve(st: Arc<State>, stream: TcpStream, id: u64) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    stream.set_nodelay(true).ok();
    // Ein haengender Miner darf das Verteilen neuer Arbeit an die anderen nicht aufhalten.
    stream.set_write_timeout(Some(Duration::from_secs(3))).ok();
    let n = st.en1_counter.fetch_add(1, Ordering::Relaxed);
    let client = Arc::new(Client {
        id,
        out: Mutex::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        }),
        en1: n.to_be_bytes(),
        diff: Mutex::new(st.cfg.diff),
        prev_diff: Mutex::new(st.cfg.diff),
        worker: Mutex::new(String::new()),
        shares: Mutex::new(VecDeque::new()),
        subscribed: Mutex::new(false),
    });
    st.clients.lock().unwrap().push(client.clone());
    log(&format!("Miner {id} verbunden: {peer}"));
    let reader = BufReader::new(stream);
    for line in reader.split(b'\n') {
        let Ok(line) = line else { break };
        if line.len() > 16 * 1024 {
            break;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else { continue };
        let mid = msg["id"].clone();
        let params = msg["params"].clone();
        let reply = |result: Value, error: Value| json!({"id": mid, "result": result, "error": error}).to_string();
        match msg["method"].as_str().unwrap_or("") {
            "mining.subscribe" => {
                let sid = format!("{:x}", client.id);
                client.send(&reply(
                    json!([[["mining.set_difficulty", sid], ["mining.notify", sid]], hex::encode(client.en1), EN2_SIZE]),
                    Value::Null,
                ));
                *client.subscribed.lock().unwrap() = true;
                let d = *client.diff.lock().unwrap();
                client.send(&json!({"id": null, "method": "mining.set_difficulty", "params": [d]}).to_string());
                if let Some(j) = st.current() {
                    client.send(&j.notify(true));
                }
            }
            "mining.authorize" => {
                *client.worker.lock().unwrap() = params[0].as_str().unwrap_or("").to_string();
                client.send(&reply(json!(true), Value::Null));
            }
            "mining.configure" => {
                client.send(&reply(json!({}), Value::Null));
            }
            "mining.extranonce.subscribe" => {
                client.send(&reply(json!(true), Value::Null));
            }
            "mining.suggest_difficulty" => {
                if let Some(d) = params[0].as_f64() {
                    set_diff(&st, &client, d.clamp(st.cfg.min_diff, st.cfg.max_diff));
                }
            }
            "mining.submit" => {
                match submit_share(&st, &client, &params) {
                    Ok(()) => {
                        client.send(&reply(json!(true), Value::Null));
                    }
                    Err((code, msg)) => {
                        st.stats.rejected.fetch_add(1, Ordering::Relaxed);
                        client.send(&reply(json!(false), json!([code, msg, null])));
                    }
                }
                retarget(&st, &client);
            }
            _ => {
                if !mid.is_null() {
                    client.send(&reply(Value::Null, json!([20, "unknown method", null])));
                }
            }
        }
    }
    st.clients.lock().unwrap().retain(|c| c.id != id);
    log(&format!("Miner {id} getrennt: {peer}"));
}

// ---------------------------------------------------------------- Status fuer den Monitor

fn status_loop(st: Arc<State>) {
    let path = st.cfg.status.clone();
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    loop {
        std::thread::sleep(Duration::from_secs(5));
        let hashrate = {
            let w = st.stats.window.lock().unwrap();
            let span = w.front().map(|(t, _)| t.elapsed().as_secs_f64()).unwrap_or(0.0).max(60.0);
            w.iter().map(|(_, d)| d * 65536.0).sum::<f64>() / span
        };
        let job = st.current();
        let workers: Vec<Value> = st
            .clients
            .lock()
            .unwrap()
            .iter()
            .map(|c| json!({"worker": *c.worker.lock().unwrap(), "diff": *c.diff.lock().unwrap()}))
            .collect();
        let aux_heights = st.aux_heights.lock().unwrap().clone();
        let chains: Vec<Value> = std::iter::once(json!({"name": st.cfg.parent.name, "height": job.as_ref().map(|j| j.tpl.height)}))
            .chain(st.cfg.aux.iter().enumerate().map(|(i, c)| {
                let active = job.as_ref().map_or(false, |j| j.aux.iter().any(|s| s.work.chain == i));
                json!({"name": c.name, "height": aux_heights[i], "active": active})
            }))
            .collect();
        let s = json!({
            "t": now(),
            "hashrate": hashrate,
            "workers": workers,
            "accepted": st.stats.accepted.load(Ordering::Relaxed),
            "rejected": st.stats.rejected.load(Ordering::Relaxed),
            "best_share": *st.stats.best.lock().unwrap(),
            "chains": chains,
            "found": *st.stats.found.lock().unwrap(),
        });
        let tmp = format!("{path}.tmp");
        if std::fs::write(&tmp, s.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn main() {
    let cfg = match load_cfg() {
        Ok(c) => c,
        Err(e) => {
            log(&format!("Konfiguration: {e}"));
            std::process::exit(2);
        }
    };
    let payout = ltc_address_script(&cfg.parent.address).expect("Adresse geprueft");
    let (tx, rx) = mpsc::channel();
    for c in std::iter::once(&cfg.parent).chain(cfg.aux.iter()) {
        if let Some(z) = &c.zmq {
            zmq::watch(z.clone(), c.name.clone(), tx.clone());
        }
    }
    let listener = match TcpListener::bind(&cfg.bind) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("Stratum {}: {e}", cfg.bind));
            std::process::exit(1);
        }
    };
    log(&format!(
        "arcadeos-mm: Stratum auf {}, Parent {}, Aux {}",
        cfg.bind,
        cfg.parent.name,
        cfg.aux.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    ));
    let n_aux = cfg.aux.len();
    let st = Arc::new(State {
        cfg,
        payout,
        jobs: RwLock::new(VecDeque::new()),
        clients: Mutex::new(Vec::new()),
        seen: Mutex::new(HashSet::new()),
        stats: Stats::default(),
        next_job: AtomicU64::new(1),
        en1_counter: AtomicU32::new(rand_seed()),
        aux_heights: Mutex::new(vec![0; n_aux]),
    });
    {
        let st = st.clone();
        std::thread::spawn(move || work_loop(st, rx));
    }
    {
        let st = st.clone();
        std::thread::spawn(move || status_loop(st));
    }
    let mut next_id = 1u64;
    for s in listener.incoming() {
        let Ok(s) = s else { continue };
        let st = st.clone();
        let id = next_id;
        next_id += 1;
        std::thread::spawn(move || serve(st, s, id));
    }
}

fn rand_seed() -> u32 {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    t ^ std::process::id().rotate_left(16)
}
