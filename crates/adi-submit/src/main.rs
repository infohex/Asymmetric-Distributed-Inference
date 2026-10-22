//! Submitter: generate A and B, upload them, submit Events one after another,
//! assemble C from the result blocks, verify against a strict reference, and
//! collect the scheduler's metrics.
//!
//!   adi-submit run --scheduler URL [--admin-token T] --m 4096 --k 4096 --n 4096
//!                  --block 128 --events 10 [--seed 1] [--verify] [--out results/real/NAME]
//!   adi-submit metrics --scheduler URL [--admin-token T] [--out DIR]

use adi_core::blobfmt::{block_matmul_strict, generate, Matrix};
use adi_scheduler::api::*;
use rayon::prelude::*;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Args {
    cmd: String,
    scheduler: String,
    admin_token: String,
    m: usize,
    k: usize,
    n: usize,
    block: usize,
    events: usize,
    seed: u64,
    verify: bool,
    out: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = argv.first().cloned().ok_or("missing command: run | metrics")?;
    let mut a = Args {
        cmd,
        scheduler: "http://127.0.0.1:7700".into(),
        admin_token: String::new(),
        m: 4096,
        k: 4096,
        n: 4096,
        block: 128,
        events: 10,
        seed: 1,
        verify: false,
        out: None,
    };
    let mut i = 1;
    while i < argv.len() {
        let next = || argv.get(i + 1).cloned().ok_or(format!("{} needs a value", argv[i]));
        let mut step = 2;
        match argv[i].as_str() {
            "--scheduler" => a.scheduler = next()?,
            "--admin-token" => a.admin_token = next()?,
            "--m" => a.m = next()?.parse().map_err(|_| "--m")?,
            "--k" => a.k = next()?.parse().map_err(|_| "--k")?,
            "--n" => a.n = next()?.parse().map_err(|_| "--n")?,
            "--block" => a.block = next()?.parse().map_err(|_| "--block")?,
            "--events" => a.events = next()?.parse().map_err(|_| "--events")?,
            "--seed" => a.seed = next()?.parse().map_err(|_| "--seed")?,
            "--out" => a.out = Some(PathBuf::from(next()?)),
            "--verify" => {
                a.verify = true;
                step = 1;
            }
            other => return Err(format!("unknown argument {other}")),
        }
        i += step;
    }
    Ok(a)
}

fn auth(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    if token.is_empty() {
        req
    } else {
        req.bearer_auth(token)
    }
}

async fn put_blob(client: &reqwest::Client, base: &str, token: &str, bytes: Vec<u8>) -> Result<String, String> {
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let r = auth(client.put(format!("{base}/blob/{hash}")), token).body(bytes).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("PUT blob: {}", r.status()));
    }
    Ok(hash)
}

async fn get_blob(client: &reqwest::Client, base: &str, hash: &str) -> Result<Vec<u8>, String> {
    let r = client.get(format!("{base}/blob/{hash}")).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("GET blob {hash}: {}", r.status()));
    }
    Ok(r.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

async fn fetch_metrics(client: &reqwest::Client, base: &str, token: &str, out: &Option<PathBuf>) -> Result<String, String> {
    let r = auth(client.get(format!("{base}/metrics")), token).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("GET metrics: {}", r.status()));
    }
    let text = r.text().await.map_err(|e| e.to_string())?;
    if let Some(dir) = out {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("summary.txt"), &text).map_err(|e| e.to_string())?;
    }
    Ok(text)
}

async fn run(args: &Args) -> Result<(), String> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(600)).build().unwrap();
    let base = &args.scheduler;
    eprintln!("generating A {}x{} and B {}x{} (seed {})", args.m, args.k, args.k, args.n, args.seed);
    let a = generate(args.m, args.k, args.seed);
    let b = generate(args.k, args.n, args.seed.wrapping_add(1));
    let t = Instant::now();
    let ah = put_blob(&client, base, &args.admin_token, a.encode()).await?;
    let bh = put_blob(&client, base, &args.admin_token, b.encode()).await?;
    eprintln!("uploaded in {:.2} s: A {ah}, B {bh}", t.elapsed().as_secs_f64());

    let reference = if args.verify {
        eprintln!("computing the strict reference C locally (same per-element order as the nodes)");
        let t = Instant::now();
        let bt = b.transposed();
        let bi_n = args.m.div_ceil(args.block);
        let bj_n = args.n.div_ceil(args.block);
        let blocks: Vec<((usize, usize), Matrix)> = (0..bi_n * bj_n)
            .into_par_iter()
            .map(|idx| ((idx / bj_n, idx % bj_n), block_matmul_strict(&a, &bt, idx / bj_n, idx % bj_n, args.block)))
            .collect();
        eprintln!("reference done in {:.2} s", t.elapsed().as_secs_f64());
        Some(blocks)
    } else {
        None
    };

    let t_all = Instant::now();
    let mut makespans = Vec::new();
    for e in 0..args.events {
        let t = Instant::now();
        let sub: SubmitResp = auth(client.post(format!("{base}/events")), &args.admin_token)
            .json(&serde_json::json!({ "a_hash": ah, "b_hash": bh, "block": args.block }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| format!("submit: {e}"))?;
        let status = loop {
            let st: EventStatus = auth(client.get(format!("{base}/events/{}", sub.e_id)), &args.admin_token)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            if st.status == "completed" {
                break st;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let secs = t.elapsed().as_secs_f64();
        makespans.push(secs);
        eprintln!("event {e} (e_id {}): {} U done in {secs:.3} s", sub.e_id, sub.units);

        if let Some(reference) = &reference {
            // Assemble and compare bit for bit. u_id order == (bi, bj) row-major order.
            let results = status.results.ok_or("completed event without results")?;
            let bj_n = args.n.div_ceil(args.block);
            let first = results.first().map(|(u, _)| *u).unwrap_or(0);
            let mut mismatched = 0usize;
            for (u_id, hash) in &results {
                let idx = u_id - first;
                let (bi, bj) = (idx / bj_n, idx % bj_n);
                let bytes = get_blob(&client, base, hash).await?;
                let got = Matrix::decode(&bytes).ok_or("result block is not a matrix blob")?;
                let want = &reference.iter().find(|((i, j), _)| *i == bi && *j == bj).unwrap().1;
                if got.rows != want.rows || got.cols != want.cols || got.data.iter().zip(&want.data).any(|(x, y)| x.to_bits() != y.to_bits()) {
                    mismatched += 1;
                }
            }
            if mismatched == 0 {
                eprintln!("event {e}: all {} blocks bit-identical to the reference", results.len());
            } else {
                return Err(format!("event {e}: {mismatched} blocks differ from the reference"));
            }
        }
    }
    let total = t_all.elapsed().as_secs_f64();
    let mean = makespans.iter().sum::<f64>() / makespans.len() as f64;
    let min = makespans.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = makespans.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!("{} events, {} U each, total {total:.3} s, makespan mean {mean:.3} s (min {min:.3}, max {max:.3})",
        args.events, args.m.div_ceil(args.block) * args.n.div_ceil(args.block));
    let summary = fetch_metrics(&client, base, &args.admin_token, &args.out).await?;
    println!("{summary}");
    if let Some(dir) = &args.out {
        let mut lines = String::from("event,makespan_s\n");
        for (i, s) in makespans.iter().enumerate() {
            lines.push_str(&format!("{i},{s:.6}\n"));
        }
        std::fs::write(dir.join("events.csv"), lines).map_err(|e| e.to_string())?;
        println!("written: {}", dir.display());
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("usage: adi-submit run --scheduler URL [--admin-token T] --m M --k K --n N --block B --events E [--seed S] [--verify] [--out DIR]");
            eprintln!("       adi-submit metrics --scheduler URL [--admin-token T] [--out DIR]");
            std::process::exit(2);
        }
    };
    let result = match args.cmd.as_str() {
        "run" => run(&args).await,
        "metrics" => {
            let client = reqwest::Client::new();
            fetch_metrics(&client, &args.scheduler, &args.admin_token, &args.out).await.map(|s| println!("{s}"))
        }
        other => Err(format!("unknown command {other}")),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
