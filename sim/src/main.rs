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
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 || args[0] != "run" {
        return usage();
    }
    let scenario_path = PathBuf::from(&args[1]);
    let mut out_dir: Option<PathBuf> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--out" if i + 1 < args.len() => {
                out_dir = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            _ => return usage(),
        }
    }

    let sc = match scenario::Scenario::load(&scenario_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let out = out_dir.unwrap_or_else(|| Path::new("results").join(&sc.name));

    let sim = engine::Simulation::new(&sc);
    let (metrics, truth) = sim.run();
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
