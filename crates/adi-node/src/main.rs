//! Node agent: register → loop { pull (long-poll) → fetch missing inputs →
//! compute the round in parallel → upload result blobs → report }.
//!
//! The node carries no scheduling parameters: W and the retry interval come
//! from the scheduler. Killing the process is how a node goes offline.

use adi_core::blobfmt::{block_matmul_strict, Matrix};
use adi_scheduler::api::*;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A compute backend for one kind of U. The CPU strict kernel is the only
/// implementation in prototype A; a GPU kernel slots in here later.
pub trait Kernel: Send + Sync {
    fn logic_hash(&self) -> &'static str;
    fn run(&self, a: &Matrix, bt: &Matrix, bi: usize, bj: usize, block: usize) -> Matrix;
}

struct CpuStrict;

impl Kernel for CpuStrict {
    fn logic_hash(&self) -> &'static str {
        adi_scheduler::LOGIC_MATMUL_BLOCK_F32_STRICT
    }
    fn run(&self, a: &Matrix, bt: &Matrix, bi: usize, bj: usize, block: usize) -> Matrix {
        block_matmul_strict(a, bt, bi, bj, block)
    }
}

struct Args {
    scheduler: String,
    name: String,
    tier: String,
    threads: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        scheduler: "http://127.0.0.1:7700".into(),
        name: hostname(),
        tier: "general".into(),
        threads: num_cpus::get_physical().max(1),
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--scheduler" => a.scheduler = next.ok_or("--scheduler needs a value")?,
            "--name" => a.name = next.ok_or("--name needs a value")?,
            "--tier" => a.tier = next.ok_or("--tier needs a value")?,
            "--threads" => a.threads = next.ok_or("--threads needs a value")?.parse().map_err(|_| "--threads must be a number")?,
            other => return Err(format!("unknown argument {other}")),
        }
        i += 2;
    }
    Ok(a)
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "node".into())
}

/// Inputs cached by content hash: the matrix and, for B, its transpose.
struct Cache {
    a: HashMap<String, Arc<Matrix>>,
    bt: HashMap<String, Arc<Matrix>>,
}

async fn fetch_blob(client: &reqwest::Client, base: &str, hash: &str) -> Result<Vec<u8>, String> {
    let resp = client.get(format!("{base}/blob/{hash}")).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GET blob {hash}: {}", resp.status()));
    }
    Ok(resp.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

async fn register(client: &reqwest::Client, args: &Args) -> Result<RegisterResp, String> {
    let resp = client
        .post(format!("{}/nodes/register", args.scheduler))
        .json(&serde_json::json!({
            "node_id": args.name,
            "tier": args.tier,
            "c": args.threads as u64,
            "hardware": { "cores": num_cpus::get(), "physical_cores": num_cpus::get_physical(), "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("register: {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\nusage: adi-node --scheduler http://host:7700 --name NAME [--tier general] [--threads N]");
            std::process::exit(2);
        }
    };
    rayon::ThreadPoolBuilder::new().num_threads(args.threads).build_global().ok();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(600)).build().unwrap();
    let kernel: Arc<dyn Kernel> = Arc::new(CpuStrict);
    let mut cache = Cache { a: HashMap::new(), bt: HashMap::new() };
    let t0 = Instant::now();

    let mut reg = loop {
        match register(&client, &args).await {
            Ok(r) => break r,
            Err(e) => {
                eprintln!("register failed ({e}); retrying in 2 s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    eprintln!("registered as node {} ({}), threads {}, W = {} s, retry {} ms", reg.node, args.name, args.threads, reg.w, reg.retry_ms);

    loop {
        let resp = match client
            .post(format!("{}/pull", args.scheduler))
            .bearer_auth(&reg.token)
            .json(&serde_json::json!({ "node": reg.node, "wait_ms": reg.retry_ms }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("pull failed ({e}); retrying in 1 s");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        match resp.status().as_u16() {
            204 => continue,
            200 => {}
            401 | 409 => {
                eprintln!("scheduler answered {}: re-registering", resp.status());
                if let Ok(r) = register(&client, &args).await {
                    reg = r;
                }
                continue;
            }
            _ => {
                eprintln!("pull: {} {}", resp.status(), resp.text().await.unwrap_or_default());
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        }
        let round: PullResp = match resp.json().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("bad pull response: {e}");
                continue;
            }
        };

        // Inputs: fetch what is not cached. bytes_in = what was fetched this round.
        let mut bytes_in = 0u64;
        let mut fetch_failed = false;
        for u in &round.units {
            if !cache.a.contains_key(&u.input.a) {
                match fetch_blob(&client, &args.scheduler, &u.input.a).await {
                    Ok(b) => match Matrix::decode(&b) {
                        Some(m) => {
                            bytes_in += b.len() as u64;
                            cache.a.insert(u.input.a.clone(), Arc::new(m));
                        }
                        None => fetch_failed = true,
                    },
                    Err(e) => {
                        eprintln!("{e}");
                        fetch_failed = true;
                    }
                }
            }
            if !cache.bt.contains_key(&u.input.b) {
                match fetch_blob(&client, &args.scheduler, &u.input.b).await {
                    Ok(b) => match Matrix::decode(&b) {
                        Some(m) => {
                            bytes_in += b.len() as u64;
                            cache.bt.insert(u.input.b.clone(), Arc::new(m.transposed()));
                        }
                        None => fetch_failed = true,
                    },
                    Err(e) => {
                        eprintln!("{e}");
                        fetch_failed = true;
                    }
                }
            }
            if fetch_failed {
                break;
            }
        }
        if fetch_failed {
            eprintln!("inputs unavailable; the round will time out and be reissued");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        // Compute: one rayon task per U.
        let started_at = t0.elapsed().as_secs_f64();
        let units = round.units.clone();
        let (a_map, bt_map, kernel2) = (cache.a.clone(), cache.bt.clone(), kernel.clone());
        let blocks: Vec<(usize, Vec<u8>)> = tokio::task::spawn_blocking(move || {
            units
                .par_iter()
                .map(|u| {
                    let a = &a_map[&u.input.a];
                    let bt = &bt_map[&u.input.b];
                    let c = kernel2.run(a, bt, u.input.bi, u.input.bj, u.input.block);
                    (u.u_id, c.encode())
                })
                .collect()
        })
        .await
        .expect("compute task panicked");
        let finished_at = t0.elapsed().as_secs_f64();

        // Upload results, then report. A result counts only once its blob is stored.
        let mut results = Vec::with_capacity(blocks.len());
        let mut upload_failed = 0usize;
        let mut uploads = Vec::new();
        for (u_id, bytes) in blocks {
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let client = client.clone();
            let url = format!("{}/blob/{hash}", args.scheduler);
            let token = reg.token.clone();
            uploads.push(tokio::spawn(async move {
                let ok = client.put(url).bearer_auth(token).body(bytes).send().await.map(|r| r.status().is_success()).unwrap_or(false);
                (u_id, hash, ok)
            }));
        }
        for up in uploads {
            if let Ok((u_id, hash, ok)) = up.await {
                if ok {
                    results.push(ResultWire { u_id, hash });
                } else {
                    upload_failed += 1;
                }
            }
        }
        if upload_failed > 0 {
            eprintln!("{upload_failed} result uploads failed; those U will be reissued");
        }
        let report = ReportReq { node: reg.node, round_id: round.round_id, started_at, finished_at, bytes_in, results };
        match client.post(format!("{}/report", args.scheduler)).bearer_auth(&reg.token).json(&report).send().await {
            Ok(r) if r.status().is_success() => {
                let rr: ReportResp = r.json().await.unwrap_or(ReportResp { accepted: Vec::new() });
                eprintln!(
                    "round {}: {} U in {:.3} s compute ({:.0} U/s), {} accepted, {} bytes fetched",
                    round.round_id,
                    round.units.len(),
                    finished_at - started_at,
                    round.units.len() as f64 / (finished_at - started_at).max(1e-9),
                    rr.accepted.len(),
                    bytes_in
                );
            }
            Ok(r) => eprintln!("report: {} {}", r.status(), r.text().await.unwrap_or_default()),
            Err(e) => eprintln!("report failed: {e}"),
        }
    }
}
