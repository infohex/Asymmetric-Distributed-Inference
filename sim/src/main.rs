mod engine;
mod metrics;
mod node;
mod scenario;
mod scheduler;
mod workload;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: sim run <scenario.toml> [--out <dir>]");
    eprintln!("       sim sweep <sweep.toml> [--out <dir>]");
    ExitCode::from(2)
}

fn parse_out(args: &[String], from: usize) -> Result<Option<PathBuf>, ()> {
    let mut out = None;
    let mut i = from;
    while i < args.len() {
        match args[i].as_str() {
            "--out" if i + 1 < args.len() => {
                out = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            _ => return Err(()),
        }
    }
    Ok(out)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return usage();
    }
    let out = match parse_out(&args, 2) {
        Ok(o) => o,
        Err(()) => return usage(),
    };
    match args[0].as_str() {
        "run" => cmd_run(Path::new(&args[1]), out),
        "sweep" => cmd_sweep(Path::new(&args[1]), out),
        _ => usage(),
    }
}

fn cmd_run(path: &Path, out: Option<PathBuf>) -> ExitCode {
    let sc = match scenario::Scenario::load(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let out = out.unwrap_or_else(|| Path::new("results").join(&sc.name));
    let (metrics, truth) = engine::Simulation::new(&sc).run();
    match metrics.write_all(&out, sc.scheduler.w, &truth) {
        Ok(summary) => {
            print!("{summary}");
            println!("written: {}", out.display());
            let v = metrics.verdicts(sc.scheduler.w);
            if v.v1_pass == Some(false) || !v.v2_pass {
                ExitCode::from(3)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("cannot write results: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_sweep(path: &Path, out: Option<PathBuf>) -> ExitCode {
    let sweep = match scenario::SweepSpec::load(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let base_path = path.parent().unwrap_or(Path::new(".")).join(&sweep.scenario);
    let base = match scenario::Scenario::load(&base_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let out = out.unwrap_or_else(|| Path::new("results").join(&sweep.name));
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("cannot create {}: {e}", out.display());
        return ExitCode::from(1);
    }
    let csv_path = out.join("sweep.csv");
    let mut wtr = match csv::Writer::from_path(&csv_path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cannot write {}: {e}", csv_path.display());
            return ExitCode::from(1);
        }
    };
    let header = [
        "w", "alpha", "k", "seeds", "sim_time", "ideal_makespan", "efficiency", "v1_mean", "v1_spread",
        "v1_spread_over_w", "v1_spread_p95_p5", "v1_pass", "v2_max_tail", "v2_tail_over_w", "v2_pass",
        "messages_per_s", "empty_pulls",
    ];
    let _ = wtr.write_record(header);
    let seeds: Vec<u64> = if sweep.grid.seeds.is_empty() { vec![base.seed] } else { sweep.grid.seeds.clone() };
    println!("seeds averaged: {:?} (v1 spread and v2 tail are the worst case over seeds)", seeds);
    println!("{:>6} {:>6} {:>4} | {:>8} {:>6} | {:>7} {:>7} {:>7} {:>4} | {:>7} {:>7} {:>4}",
        "w", "alpha", "k", "time", "eff", "v1mean", "v1/w", "p95-5/w", "v1", "v2tail", "v2/w", "v2");
    for (w, a, k) in sweep.combinations() {
        let mut sc = base.clone();
        if let Some(w) = w { sc.scheduler.w = w; }
        if let Some(a) = a { sc.scheduler.alpha = a; }
        if let Some(k) = k { sc.scheduler.k = k; }
        let n = seeds.len() as f64;
        let (mut time, mut ideal, mut eff, mut v1m, mut v1s, mut v1p90, mut v2t, mut msgs, mut empty) =
            (0.0, 0.0, 0.0, 0.0, 0.0_f64, 0.0, 0.0_f64, 0.0, 0u64);
        let (mut v1_all_pass, mut v2_all_pass, mut v1_any) = (true, true, false);
        for seed in &seeds {
            sc.seed = *seed;
            let (m, _) = engine::Simulation::new(&sc).run();
            let v = m.verdicts(sc.scheduler.w);
            time += m.sim_time / n;
            ideal += v.ideal_makespan / n;
            eff += v.efficiency / n;
            msgs += (m.pulls + m.rounds.len() as u64) as f64 / m.sim_time.max(1e-9) / n;
            empty += m.empty_pulls;
            if let (Some(mean), Some(spread), Some(p90), Some(pass)) = (v.v1_mean, v.v1_spread, v.v1_spread_p90, v.v1_pass) {
                v1_any = true;
                v1m += mean / n;
                v1s = v1s.max(spread);
                v1p90 += p90 / n;
                v1_all_pass &= pass;
            }
            v2t = v2t.max(v.v2_max_tail);
            v2_all_pass &= v.v2_pass;
        }
        let v1p = if !v1_any { "N/A" } else if v1_all_pass { "PASS" } else { "FAIL" };
        let v2p = if v2_all_pass { "PASS" } else { "FAIL" };
        let w = sc.scheduler.w;
        let _ = wtr.write_record([
            f(w), f(sc.scheduler.alpha), f(sc.scheduler.k), seeds.len().to_string(), f(time), f(ideal), f(eff),
            f(v1m), f(v1s), f(v1s / w), f(v1p90), v1p.to_string(), f(v2t), f(v2t / w), v2p.to_string(),
            f(msgs), empty.to_string(),
        ]);
        println!("{:>6.2} {:>6.2} {:>4.1} | {:>8.2} {:>6.3} | {:>7.3} {:>7.3} {:>7.3} {:>4} | {:>7.3} {:>7.3} {:>4}",
            w, sc.scheduler.alpha, sc.scheduler.k, time, eff, v1m, v1s / w, v1p90 / w, v1p, v2t, v2t / w, v2p);
    }
    let _ = wtr.flush();
    println!("written: {}", csv_path.display());
    ExitCode::SUCCESS
}

fn f(x: f64) -> String {
    format!("{x:.4}")
}
