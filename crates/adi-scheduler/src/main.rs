use adi_scheduler::config::Config;
use adi_scheduler::{app, timeout_task, AppState};
use std::path::Path;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match args.first() {
        Some(p) => match Config::load(Path::new(p)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        None => {
            eprintln!("usage: adi-scheduler <config.toml>");
            std::process::exit(2);
        }
    };
    let listen = config.listen.clone();
    let state = match AppState::new(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot initialise: {e}");
            std::process::exit(1);
        }
    };
    tokio::spawn(timeout_task(state.clone()));
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {listen}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("adi-scheduler listening on {listen}");
    if let Err(e) = axum::serve(listener, app(state)).await {
        eprintln!("server error: {e}");
    }
}
