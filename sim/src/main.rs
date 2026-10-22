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
    // V3 needs the same scenario without hysteresis as the baseline.
    let v3 = if sc.ets.len() > 1 && (sc.scheduler.hysteresis_n > 1 || sc.scheduler.hysteresis_band > 0.0) {
        let mut base = sc.clone();
        base.scheduler.hysteresis_n = 1;
        base.scheduler.hysteresis_band = 0.0;
        let (m0, _) = engine::Simulation::new(&base).run();
        let with = metrics.transitions as f64;
        let without = m0.transitions as f64;
        let reduction = if without > 0.0 { 1.0 - with / without } else { 1.0 };
        let pass = reduction >= 0.8;
        // Fewer than 10 transitions without hysteresis is initial promotion, not flapping.
        if m0.transitions < 10 { None } else { Some((metrics.transitions, m0.transitions, reduction, pass)) }
    } else {
        None
    };
    match metrics.write_all(&out, sc.scheduler.w, &truth) {
        Ok(mut summary) => {
            if let Some((with, without, red, pass)) = v3 {
                let line = format!(
                    "V3 eligibility transitions: hysteresis (n = {}, band = {}) -> {with}, none -> {without}, reduction {:.1}%, limit 80% -> {}\n",
                    sc.scheduler.hysteresis_n, sc.scheduler.hysteresis_band, red * 100.0, if pass { "PASS" } else { "FAIL" });
                summary.push_str(&line);
                let _ = std::fs::write(out.join("summary.txt"), &summary);
            }
            print!("{summary}");
            println!("written: {}", out.display());
            let v = metrics.verdicts(sc.scheduler.w);
            let v3_fail = matches!(v3, Some((_, _, _, false)));
            if v.v1_pass == Some(false) || !v.v2_pass || v3_fail || v.v4_pass == Some(false)
                || metrics.tier_violations > 0 || metrics.waiting_at_end > 0 {
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
        "w", "alpha", "k", "timeout_mult", "seeds", "sim_time", "ideal_makespan", "efficiency", "v1_mean", "v1_spread",
        "v1_spread_over_w", "v1_spread_p95_p5", "v1_pass", "v2_max_tail", "v2_tail_over_w", "v2_limit", "v2_pass",
        "v4_worst_excess", "v4_pass", "reissued", "late_results", "messages_per_s", "empty_pulls",
    ];
    let _ = wtr.write_record(header);
    let seeds: Vec<u64> = if sweep.grid.seeds.is_empty() { vec![base.seed] } else { sweep.grid.seeds.clone() };
    println!("seeds averaged: {:?} (v1 spread and v2 tail are the worst case over seeds)", seeds);
    println!("{:>6} {:>6} {:>4} {:>5} | {:>8} {:>6} | {:>7} {:>7} {:>7} {:>4} | {:>7} {:>7} {:>4} | {:>7} {:>4} {:>7} {:>5}",
        "w", "alpha", "k", "tmo", "time", "eff", "v1mean", "v1/w", "p95-5/w", "v1", "v2tail", "v2/w", "v2", "v4exc", "v4", "reissue", "late");
    for (w, a, k, t) in sweep.combinations() {
        let mut sc = base.clone();
        if let Some(w) = w { sc.scheduler.w = w; }
        if let Some(a) = a { sc.scheduler.alpha = a; }
        if let Some(k) = k { sc.scheduler.k = k; }
        if let Some(t) = t { sc.scheduler.timeout_mult = t; }
        let n = seeds.len() as f64;
        let (mut time, mut ideal, mut eff, mut v1m, mut v1s, mut v1p90, mut v2t, mut msgs, mut empty) =
            (0.0, 0.0, 0.0, 0.0, 0.0_f64, 0.0, 0.0_f64, 0.0, 0u64);
        let (mut v1_all_pass, mut v2_all_pass, mut v1_any) = (true, true, false);
        let (mut v4_worst, mut v4_any, mut v4_all_pass, mut reissued, mut late) = (f64::NEG_INFINITY, false, true, 0u64, 0u64);
        let mut v2_limit = 0.0;
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
            v2_limit = v.v2_limit;
            v2_all_pass &= v.v2_pass;
            if let (Some(x), Some(p)) = (v.v4_worst_excess, v.v4_pass) {
                v4_any = true;
                v4_worst = v4_worst.max(x);
                v4_all_pass &= p;
            }
            reissued += m.reissued_units;
            late += m.late_results;
        }
        let v4p = if !v4_any { "N/A" } else if v4_all_pass { "PASS" } else { "FAIL" };
        let v4x = if v4_any { v4_worst } else { f64::NAN };
        let v1p = if !v1_any { "N/A" } else if v1_all_pass { "PASS" } else { "FAIL" };
        let v2p = if v2_all_pass { "PASS" } else { "FAIL" };
        let w = sc.scheduler.w;
        let _ = wtr.write_record([
            f(w), f(sc.scheduler.alpha), f(sc.scheduler.k), f(sc.scheduler.timeout_mult), seeds.len().to_string(), f(time), f(ideal), f(eff),
            f(v1m), f(v1s), f(v1s / w), f(v1p90), v1p.to_string(), f(v2t), f(v2t / w), f(v2_limit), v2p.to_string(),
            f(v4x), v4p.to_string(), reissued.to_string(), late.to_string(), f(msgs), empty.to_string(),
        ]);
        println!("{:>6.2} {:>6.2} {:>4.1} {:>5.2} | {:>8.2} {:>6.3} | {:>7.3} {:>7.3} {:>7.3} {:>4} | {:>7.3} {:>7.3} {:>4} | {:>+7.3} {:>4} {:>7} {:>5}",
            w, sc.scheduler.alpha, sc.scheduler.k, sc.scheduler.timeout_mult, time, eff, v1m, v1s / w, v1p90 / w, v1p, v2t, v2t / w, v2p, v4x, v4p, reissued, late);
    }
    let _ = wtr.flush();
    println!("written: {}", csv_path.display());
    ExitCode::SUCCESS
}

fn f(x: f64) -> String {
    format!("{x:.4}")
}
