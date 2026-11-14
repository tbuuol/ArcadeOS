//! chaind: a lean, fully validating node for one chain, for ArcadeOS.
//!
//! The chain lives on disk (headers in a flat file, raw blocks appended, coins and indexes in LMDB); the process
//! itself holds only what is in flight, within budgets. Sync is a pipeline over all cores: headers' proof of work
//! in parallel, blocks downloaded from several peers, parsed and checked in parallel, connected in order, their
//! scripts checked in parallel; coins reach LMDB only for blocks that passed everything.
//!
//! Settings (environment):
//!   CHAIND_CHAIN       peptest | pepmain | ltctest | ltcmain | dogemain          (peptest)
//!   CHAIND_DIR         data directory                                            (required)
//!   CHAIND_RPC         RPC address, cookie in <dir>/.cookie                      (127.0.0.1:44880)
//!   CHAIND_PEERS       peers, comma-separated                    (the chain's known peers)
//!   CHAIND_DOWNLOADERS download threads                                          (6)
//!   CHAIND_BATCH       blocks per download request                               (64)
//!   CHAIND_SYNC_MB     memory for the pipeline while syncing                     (256)
//!   CHAIND_ASSUME_VALID=1  skip scripts below the newest checkpoint              (off: all scripts checked)
//!   CHAIND_COOKIE      where to write the RPC cookie                             (<dir>/.cookie)
//!   CHAIND_WALLET      file of WIF keys for sendtoaddress / getbalance           (no wallet)
//!
//! Built on arcade-chain and libbitcoinconsensus from Martin Seeger's arcade-v1 (MIT, see vendor/).

mod addr;
mod mempool;
mod net;
mod relay;
mod wallet;
mod node;
mod rpc;
mod rules;
mod store;
mod utxo;

use arcade_chain::params::{ChainParams, DOGECOIN_MAIN, LITECOIN_MAIN, LITECOIN_TEST, PEPECOIN_MAIN, PEPECOIN_TEST};
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn env(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

/// This process' memory: all of it, its own (heap, stacks), and mapped files (page cache it can lose).
pub fn mem() -> String {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |k: &str| s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    format!("RSS {} MB (own {} MB, mapped files {} MB)", kb("VmRSS:") >> 10, kb("RssAnon:") >> 10, kb("RssFile:") >> 10)
}

/// `chaind cli <method> [params...]`: ask a running chaind (CHAIND_RPC, cookie at CHAIND_COOKIE or
/// <CHAIND_DIR>/.cookie) and print the result as Core's cli does. Numbers, true/false and JSON stay as they are.
fn cli(args: &[String]) -> ! {
    use std::io::{Read, Write};
    let rpc = env("CHAIND_RPC", "127.0.0.1:44880");
    let cookie = std::env::var("CHAIND_COOKIE").unwrap_or_else(|_| format!("{}/.cookie", env("CHAIND_DIR", ".")));
    let Ok(c) = std::fs::read_to_string(&cookie) else {
        eprintln!("error: no cookie at {cookie} (is chaind running?)");
        std::process::exit(1)
    };
    let params: Vec<serde_json::Value> = args[1..].iter().map(|a| serde_json::from_str(a).unwrap_or_else(|_| serde_json::Value::String(a.clone()))).collect();
    let body = serde_json::json!({"jsonrpc": "1.0", "id": "cli", "method": args.first().cloned().unwrap_or_default(), "params": params}).to_string();
    let auth = {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let b = c.trim().as_bytes();
        let mut o = String::new();
        for ch in b.chunks(3) {
            let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                o.push(if i <= ch.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        o
    };
    let Ok(mut s) = std::net::TcpStream::connect(&rpc) else {
        eprintln!("error: could not connect to chaind at {rpc}");
        std::process::exit(1)
    };
    let _ = write!(s, "POST / HTTP/1.1\r\nHost: {rpc}\r\nAuthorization: Basic {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let mut resp = Vec::new();
    let _ = s.read_to_end(&mut resp);
    let at = resp.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    let v: serde_json::Value = serde_json::from_slice(&resp[at..]).unwrap_or(serde_json::Value::Null);
    if !v["error"].is_null() {
        eprintln!("error: {}", v["error"]);
        std::process::exit(1);
    }
    match &v["result"] {
        serde_json::Value::String(s) => println!("{s}"),
        r => println!("{}", serde_json::to_string_pretty(r).unwrap_or_default()),
    }
    std::process::exit(0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("cli") {
        cli(&args[2..]);
    }
    // `chaind dbinfo <dir>`: what LMDB really holds (the file only ever grows to its high-water mark)
    if args.get(1).map(|s| s.as_str()) == Some("dbinfo") {
        let st = store::Store::open(std::path::Path::new(&args[2])).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
        let r = st.env.read_txn().unwrap();
        let mut total = 0u64;
        for (name, db) in [("coins", &st.coins), ("txi", &st.txi), ("meta", &st.meta)] {
            let s = db.stat(&r).unwrap();
            let pages = (s.branch_pages + s.leaf_pages + s.overflow_pages) as u64;
            total += pages * s.page_size as u64;
            println!("{name:6} {:>10} entries {:>8} pages = {:>6} MB  ({:.1} bytes per entry)", s.entries, pages, pages * s.page_size as u64 >> 20,
                pages as f64 * s.page_size as f64 / s.entries.max(1) as f64);
        }
        let i = st.env.info();
        println!("format {}, data {} MB, file high-water mark {} MB", store::fmt(), total >> 20, (i.last_page_number as u64 * 4096) >> 20);
        std::process::exit(0);
    }
    let params: &'static ChainParams = match env("CHAIND_CHAIN", "peptest").as_str() {
        "peptest" => &PEPECOIN_TEST,
        "pepmain" => &PEPECOIN_MAIN,
        "ltctest" => &LITECOIN_TEST,
        "ltcmain" => &LITECOIN_MAIN,
        "dogemain" => &DOGECOIN_MAIN,
        c => {
            eprintln!("chaind: unknown chain {c}");
            std::process::exit(1);
        }
    };
    let dir = std::path::PathBuf::from(std::env::var("CHAIND_DIR").unwrap_or_else(|_| {
        eprintln!("chaind: set CHAIND_DIR");
        std::process::exit(1)
    }));
    let store: &'static store::Store = Box::leak(Box::new(store::Store::open(&dir).unwrap_or_else(|e| {
        eprintln!("chaind: {e}");
        std::process::exit(1)
    })));
    // the workers run libbitcoinconsensus and secp256k1, which keep big buffers on the stack: 16 MB each
    // (std's 2 MB overflowed on a Litecoin testnet block); it is address space, only what is touched is memory
    let _ = rayon::ThreadPoolBuilder::new().stack_size(16 << 20).thread_name(|i| format!("work{i}")).build_global();
    let sync_mb: u64 = env("CHAIND_SYNC_MB", "256").parse().unwrap_or(256);
    let budget = node::Budget { inflight_bytes: (sync_mb / 4) << 20, cache_bytes: ((sync_mb * 3 / 4) << 20) as usize, script_window: 2000 };
    let peers: Vec<String> = match std::env::var("CHAIND_PEERS") {
        Ok(p) => p.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        Err(_) => {
            let mut v: Vec<String> = params.fixed_peers.iter().map(|s| s.to_string()).collect();
            for seed in params.dns_seeds {
                use std::net::ToSocketAddrs;
                if let Ok(a) = (*seed, params.p2p_port).to_socket_addrs() {
                    v.extend(a.filter(|x| x.is_ipv4()).take(12).map(|x| x.to_string()));
                }
            }
            v.dedup();
            v
        }
    };
    // genesis first, so the shared state starts from what is on disk
    {
        let (t, r) = crossbeam_channel::unbounded();
        let tmp = Arc::new(node::Shared::new(store, params, node::Budget { inflight_bytes: 0, cache_bytes: 0, script_window: 1 }, vec![]));
        node::Connector::new(tmp, r, t).init();
    }
    let sh = Arc::new(node::Shared::new(store, params, budget, peers));
    sh.log(&format!(
        "chaind {} for {}: coins at {}, headers at {}, {} cores, sync memory {sync_mb} MB, scripts {}",
        env!("CARGO_PKG_VERSION"),
        params.name,
        sh.ctip.load(Ordering::SeqCst),
        sh.htip.load(Ordering::SeqCst),
        rayon::current_num_threads(),
        if rules::verify_all_scripts() { "all checked" } else { "checked above the newest checkpoint" }
    ));
    if let Ok(path) = std::env::var("CHAIND_WALLET") {
        match wallet::Wallet::load(params, &path) {
            Ok(mut w) => {
                let t = std::time::Instant::now();
                if let Err(e) = w.rescan(store) {
                    sh.log(&format!("wallet: {e}"));
                }
                let (ok, imm) = w.balance(sh.ctip.load(Ordering::SeqCst), params);
                sh.log(&format!("wallet: {} keys, {} coins, balance {} (+{} immature), read in {:.1}s",
                    w.keys(), w.coins.len(), ok as f64 / 1e8, imm as f64 / 1e8, t.elapsed().as_secs_f64()));
                *sh.wallet.lock().unwrap() = Some(w);
            }
            Err(e) => sh.log(&e),
        }
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    // RPC with Core's cookie
    let cookie = format!("__cookie__:{}", {
        let mut b = [0u8; 32];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut b);
        }
        hex::encode(b)
    });
    let cpath = std::env::var("CHAIND_COOKIE").map(std::path::PathBuf::from).unwrap_or_else(|_| dir.join(".cookie"));
    if let Some(d) = cpath.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&cpath, &cookie);
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&cpath, std::fs::Permissions::from_mode(0o600));
    }
    {
        let (sh, tx, bind) = (sh.clone(), tx.clone(), env("CHAIND_RPC", "127.0.0.1:44880"));
        std::thread::spawn(move || rpc::serve(sh, bind, cookie, tx));
    }
    {
        let (sh, tx) = (sh.clone(), tx.clone());
        std::thread::spawn(move || net::header_thread(params, sh, tx));
    }
    let n: usize = env("CHAIND_DOWNLOADERS", "6").parse().unwrap_or(6);
    for i in 0..n {
        let (sh, tx) = (sh.clone(), tx.clone());
        std::thread::spawn(move || net::download_thread(params, sh, i, tx));
    }
    for i in 0..2 {
        let sh = sh.clone();
        std::thread::spawn(move || relay::relay_thread(params, sh, i));
    }
    // SIGTERM / SIGINT: stop cleanly (the connector writes everything that passed)
    {
        let sh = sh.clone();
        let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]).expect("signals");
        std::thread::spawn(move || {
            if signals.forever().next().is_some() {
                sh.log("stop requested");
                sh.stop.store(true, Ordering::SeqCst);
                sh.wake();
            }
        });
    }
    node::Connector::new(sh.clone(), rx, tx).run();
    let _ = std::fs::remove_file(&cpath);
}
