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
//!   CHAIND_DOWNLOADERS download threads                                          (3)
//!   CHAIND_SYNC_MB     memory for the pipeline while syncing                     (256)
//!   CHAIND_ASSUME_VALID=1  skip scripts below the newest checkpoint              (off: all scripts checked)
//!
//! Built on arcade-chain and libbitcoinconsensus from Martin Seeger's arcade-v1 (MIT, see vendor/).

mod addr;
mod net;
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

fn main() {
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
    let sync_mb: u64 = env("CHAIND_SYNC_MB", "256").parse().unwrap_or(256);
    let budget = node::Budget { inflight_bytes: (sync_mb / 4) << 20, cache_bytes: ((sync_mb * 3 / 4) << 20) as usize, script_window: 2000 };
    let peers: Vec<String> = match std::env::var("CHAIND_PEERS") {
        Ok(p) => p.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        Err(_) => params.fixed_peers.iter().map(|s| s.to_string()).collect(),
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
    let cpath = dir.join(".cookie");
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
    let n: usize = env("CHAIND_DOWNLOADERS", "3").parse().unwrap_or(3);
    for i in 0..n {
        let (sh, tx) = (sh.clone(), tx.clone());
        std::thread::spawn(move || net::download_thread(params, sh, i, tx));
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
