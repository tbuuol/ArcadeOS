//! mm-gpu: scrypt-Miner fuer die GPU (OpenCL, z. B. Mesa rusticl auf der Carrizo-APU), spricht Stratum v1 wie
//! ein ASIC, normalerweise mit arcadeos-mm auf derselben Maschine.
//!
//!   mm-gpu [stratum-adresse (127.0.0.1:3333)] [worker (gpu)]
//!   Umgebung: MM_GPU_ITEMS (Hashes pro Kernel-Aufruf, Vorgabe 1536), RUSTICL_ENABLE=radeonsi fuer rusticl.
//!
//! Der Kernel liefert alle Hashes einer Runde; geprueft wird hier gegen das Share-Ziel (ein Vergleich je Hash).

use mm::*;
use serde_json::{json, Value};
use std::ffi::{c_void, CString};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- OpenCL (nur was gebraucht wird)

type ClInt = i32;
type ClUint = u32;
type Ptr = *mut c_void;

#[link(name = "OpenCL")]
extern "C" {
    fn clGetPlatformIDs(n: ClUint, p: *mut Ptr, num: *mut ClUint) -> ClInt;
    fn clGetDeviceIDs(p: Ptr, t: u64, n: ClUint, d: *mut Ptr, num: *mut ClUint) -> ClInt;
    fn clCreateContext(props: *const isize, n: ClUint, d: *const Ptr, cb: Ptr, ud: Ptr, err: *mut ClInt) -> Ptr;
    fn clCreateCommandQueueWithProperties(c: Ptr, d: Ptr, props: *const u64, err: *mut ClInt) -> Ptr;
    fn clCreateProgramWithSource(c: Ptr, n: ClUint, s: *const *const i8, len: *const usize, err: *mut ClInt) -> Ptr;
    fn clBuildProgram(p: Ptr, n: ClUint, d: *const Ptr, opts: *const i8, cb: Ptr, ud: Ptr) -> ClInt;
    fn clGetProgramBuildInfo(p: Ptr, d: Ptr, what: ClUint, size: usize, val: *mut c_void, ret: *mut usize) -> ClInt;
    fn clCreateKernel(p: Ptr, name: *const i8, err: *mut ClInt) -> Ptr;
    fn clCreateBuffer(c: Ptr, flags: u64, size: usize, host: Ptr, err: *mut ClInt) -> Ptr;
    fn clSetKernelArg(k: Ptr, i: ClUint, size: usize, val: *const c_void) -> ClInt;
    fn clEnqueueWriteBuffer(q: Ptr, b: Ptr, block: ClUint, off: usize, size: usize, p: *const c_void, n: ClUint, w: Ptr, e: Ptr) -> ClInt;
    fn clEnqueueReadBuffer(q: Ptr, b: Ptr, block: ClUint, off: usize, size: usize, p: *mut c_void, n: ClUint, w: Ptr, e: Ptr) -> ClInt;
    fn clEnqueueNDRangeKernel(q: Ptr, k: Ptr, dim: ClUint, off: *const usize, g: *const usize, l: *const usize, n: ClUint, w: Ptr, e: Ptr) -> ClInt;
    fn clFinish(q: Ptr) -> ClInt;
}

const CL_DEVICE_TYPE_ALL: u64 = 0xFFFF_FFFF;
const CL_MEM_READ_WRITE: u64 = 1;
const CL_MEM_WRITE_ONLY: u64 = 2;
const CL_MEM_READ_ONLY: u64 = 4;
const CL_PROGRAM_BUILD_LOG: ClUint = 0x1183;

fn ck(e: ClInt, what: &str) {
    if e != 0 {
        eprintln!("OpenCL-Fehler {e} bei {what}");
        std::process::exit(1);
    }
}

struct Gpu {
    queue: Ptr,
    kernel: Ptr,
    hdr: Ptr,
    out: Ptr,
    items: usize,
}

impl Gpu {
    fn new(items: usize) -> Gpu {
        unsafe {
            let mut plat: Ptr = std::ptr::null_mut();
            ck(clGetPlatformIDs(1, &mut plat, std::ptr::null_mut()), "clGetPlatformIDs");
            let mut dev: Ptr = std::ptr::null_mut();
            ck(clGetDeviceIDs(plat, CL_DEVICE_TYPE_ALL, 1, &mut dev, std::ptr::null_mut()), "clGetDeviceIDs");
            let mut e = 0;
            let ctx = clCreateContext(std::ptr::null(), 1, &dev, std::ptr::null_mut(), std::ptr::null_mut(), &mut e);
            ck(e, "clCreateContext");
            let queue = clCreateCommandQueueWithProperties(ctx, dev, std::ptr::null(), &mut e);
            ck(e, "clCreateCommandQueue");
            let src = CString::new(include_str!("../gpu/scrypt.cl")).unwrap();
            let sp = src.as_ptr();
            let prog = clCreateProgramWithSource(ctx, 1, &sp, std::ptr::null(), &mut e);
            ck(e, "clCreateProgramWithSource");
            let opts = CString::new("").unwrap();
            if clBuildProgram(prog, 1, &dev, opts.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                let mut log = vec![0u8; 65536];
                clGetProgramBuildInfo(prog, dev, CL_PROGRAM_BUILD_LOG, log.len(), log.as_mut_ptr() as *mut c_void, std::ptr::null_mut());
                eprintln!("Kernel: {}", String::from_utf8_lossy(&log));
                std::process::exit(1);
            }
            let name = CString::new("scrypt").unwrap();
            let kernel = clCreateKernel(prog, name.as_ptr(), &mut e);
            ck(e, "clCreateKernel");
            let hdr = clCreateBuffer(ctx, CL_MEM_READ_ONLY, 76, std::ptr::null_mut(), &mut e);
            ck(e, "Puffer hdr");
            let v = clCreateBuffer(ctx, CL_MEM_READ_WRITE, items * 131072, std::ptr::null_mut(), &mut e);
            ck(e, "Puffer V");
            let out = clCreateBuffer(ctx, CL_MEM_WRITE_ONLY, items * 32, std::ptr::null_mut(), &mut e);
            ck(e, "Puffer out");
            ck(clSetKernelArg(kernel, 0, std::mem::size_of::<Ptr>(), &hdr as *const Ptr as *const c_void), "arg0");
            ck(clSetKernelArg(kernel, 2, std::mem::size_of::<Ptr>(), &v as *const Ptr as *const c_void), "arg2");
            ck(clSetKernelArg(kernel, 3, std::mem::size_of::<Ptr>(), &out as *const Ptr as *const c_void), "arg3");
            Gpu { queue, kernel, hdr, out, items }
        }
    }

    /// Hashes fuer nonce0 .. nonce0+items ueber die ersten 76 Byte von `header`.
    fn run(&self, header76: &[u8], nonce0: u32, out: &mut Vec<u8>) {
        out.resize(self.items * 32, 0);
        unsafe {
            ck(clEnqueueWriteBuffer(self.queue, self.hdr, 0, 0, 76, header76.as_ptr() as *const c_void, 0, std::ptr::null_mut(), std::ptr::null_mut()), "write hdr");
            ck(clSetKernelArg(self.kernel, 1, 4, &nonce0 as *const u32 as *const c_void), "arg1");
            let g = self.items;
            ck(clEnqueueNDRangeKernel(self.queue, self.kernel, 1, std::ptr::null(), &g, std::ptr::null(), 0, std::ptr::null_mut(), std::ptr::null_mut()), "kernel");
            ck(clEnqueueReadBuffer(self.queue, self.out, 1, 0, out.len(), out.as_mut_ptr() as *mut c_void, 0, std::ptr::null_mut(), std::ptr::null_mut()), "read out");
            ck(clFinish(self.queue), "finish");
        }
    }
}

// ---------------------------------------------------------------- Stratum

#[derive(Clone)]
struct Work {
    id: String,
    prev: H256,
    coinb1: Vec<u8>,
    coinb2: Vec<u8>,
    branch: Vec<H256>,
    version: u32,
    bits: u32,
    ntime: u32,
    serial: u64,
}

#[derive(Default)]
struct Shared {
    en1: Vec<u8>,
    en2_size: usize,
    target: H256,
    work: Option<Work>,
    serial: u64,
    accepted: u64,
    rejected: u64,
}

fn parse_work(p: &Value, serial: u64) -> Option<Work> {
    let h = |i: usize| hex::decode(p[i].as_str()?).ok();
    let u = |i: usize| u32::from_str_radix(p[i].as_str()?, 16).ok();
    Some(Work {
        id: p[0].as_str()?.to_string(),
        prev: prevhash_from_stratum(p[1].as_str()?).ok()?,
        coinb1: h(2)?,
        coinb2: h(3)?,
        branch: p[4].as_array()?.iter().map(|b| hex::decode(b.as_str().unwrap_or("")).ok().and_then(|v| v.try_into().ok())).collect::<Option<Vec<H256>>>()?,
        version: u(5)?,
        bits: u(6)?,
        ntime: u(7)?,
        serial,
    })
}

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:3333".into());
    let worker = std::env::args().nth(2).unwrap_or_else(|| "gpu".into());
    let items: usize = std::env::var("MM_GPU_ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(1536);
    let gpu = Gpu::new(items);
    eprintln!("mm-gpu: GPU bereit ({items} Hashes je Aufruf), Stratum {addr}");
    loop {
        if let Err(e) = session(&gpu, &addr, &worker) {
            eprintln!("mm-gpu: {e}, neuer Versuch in 5 s");
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn session(gpu: &Gpu, addr: &str, worker: &str) -> Result<(), String> {
    let stream = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    stream.set_nodelay(true).ok();
    let out = Arc::new(Mutex::new(stream.try_clone().map_err(|e| e.to_string())?));
    let send = |o: &Arc<Mutex<TcpStream>>, v: Value| -> Result<(), String> {
        let mut s = o.lock().unwrap();
        s.write_all(format!("{v}\n").as_bytes()).map_err(|e| e.to_string())
    };
    send(&out, json!({"id": 1, "method": "mining.subscribe", "params": ["mm-gpu/0.1"]}))?;
    send(&out, json!({"id": 2, "method": "mining.authorize", "params": [worker, "x"]}))?;
    let shared = Arc::new(Mutex::new(Shared { target: diff_to_target(1.0), ..Default::default() }));
    let alive = Arc::new(Mutex::new(true));
    {
        let shared = shared.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
                let mut s = shared.lock().unwrap();
                match m["method"].as_str() {
                    Some("mining.notify") => {
                        s.serial += 1;
                        let serial = s.serial;
                        if let Some(w) = parse_work(&m["params"], serial) {
                            s.work = Some(w);
                        }
                    }
                    Some("mining.set_difficulty") => {
                        if let Some(d) = m["params"][0].as_f64() {
                            s.target = diff_to_target(d);
                        }
                    }
                    _ => {
                        if m["id"] == 1 {
                            s.en1 = hex::decode(m["result"][1].as_str().unwrap_or("")).unwrap_or_default();
                            s.en2_size = m["result"][2].as_u64().unwrap_or(4) as usize;
                        } else if m["id"].as_u64().map_or(false, |i| i >= 100) {
                            if m["result"] == true {
                                s.accepted += 1;
                            } else {
                                s.rejected += 1;
                                eprintln!("mm-gpu: Share abgelehnt: {}", m["error"]);
                            }
                        }
                    }
                }
            }
            *alive.lock().unwrap() = false;
        });
    }

    let mut hashes = Vec::new();
    let mut en2: u64 = 0;
    let mut nonce: u64 = 0;
    let mut serial = 0;
    let mut cur: Option<(Work, Vec<u8>, H256, [u8; 80])> = None; // Arbeit, en2, root, Kopf
    let mut submit_id = 100u64;
    let mut done: u64 = 0;
    let mut t_log = Instant::now();
    loop {
        if !*alive.lock().unwrap() {
            return Err("Verbindung zu".into());
        }
        let (work, en1, en2_size, target, acc, rej) = {
            let s = shared.lock().unwrap();
            (s.work.clone(), s.en1.clone(), s.en2_size, s.target, s.accepted, s.rejected)
        };
        let Some(work) = work else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        if en1.is_empty() || en2_size == 0 {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        // Neue Arbeit oder Nonce-Raum erschoepft: Coinbase mit naechster Extranonce2 bauen
        if work.serial != serial || nonce >= 1u64 << 32 || cur.is_none() {
            if work.serial != serial {
                serial = work.serial;
            } else {
                en2 += 1;
            }
            nonce = 0;
            let e2: Vec<u8> = (en2 as u64).to_be_bytes()[8 - en2_size.min(8)..].to_vec();
            let (_cb, root) = coinbase_root(&work.coinb1, &en1, &e2, &work.coinb2, &work.branch);
            let header = build_header(work.version, &work.prev, &root, work.ntime, work.bits, 0);
            cur = Some((work.clone(), e2, root, header));
        }
        let (w, e2, _root, header) = cur.as_ref().unwrap();
        gpu.run(&header[..76], nonce as u32, &mut hashes);
        for i in 0..gpu.items {
            let h: &H256 = hashes[i * 32..i * 32 + 32].try_into().unwrap();
            if meets(h, &target) {
                let n = nonce as u32 + i as u32;
                submit_id += 1;
                let _ = send(&out, json!({"id": submit_id, "method": "mining.submit", "params": [
                    worker, w.id, hex::encode(e2), format!("{:08x}", w.ntime), format!("{n:08x}")
                ]}));
            }
        }
        nonce += gpu.items as u64;
        done += gpu.items as u64;
        if t_log.elapsed() >= Duration::from_secs(60) {
            eprintln!("mm-gpu: {:.1} kH/s, Shares {acc} angenommen / {rej} abgelehnt", done as f64 / t_log.elapsed().as_secs_f64() / 1000.0);
            done = 0;
            t_log = Instant::now();
        }
    }
}
