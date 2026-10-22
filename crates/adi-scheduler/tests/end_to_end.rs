//! End-to-end: a real HTTP server, two fake nodes of very different speed,
//! three sequential Events. Fake nodes sleep instead of computing and upload
//! a placeholder block per U, so this exercises every endpoint and the
//! scheduling mechanism without any numeric work.

use adi_core::{EtSpec, SchedulerParams};
use adi_scheduler::api::*;
use adi_scheduler::config::Config;
use adi_scheduler::{app, timeout_task, AppState};
use std::time::Duration;

fn matrix_blob(rows: usize, cols: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + rows * cols * 4);
    v.extend_from_slice(&(rows as u32).to_le_bytes());
    v.extend_from_slice(&(cols as u32).to_le_bytes());
    for i in 0..rows * cols {
        v.extend_from_slice(&((i % 7) as f32).to_le_bytes());
    }
    v
}

async fn fake_node(base: String, name: &str, c: u64, r_true: f64, latency: f64, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> (usize, usize) {
    let client = reqwest::Client::new();
    let reg: RegisterResp = client
        .post(format!("{base}/nodes/register"))
        .json(&serde_json::json!({ "node_id": name, "tier": "general", "c": c }))
        .send().await.unwrap().json().await.unwrap();
    let (mut rounds, mut units) = (0usize, 0usize);
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let resp = client
            .post(format!("{base}/pull"))
            .bearer_auth(&reg.token)
            .json(&serde_json::json!({ "node": reg.node, "wait_ms": 200 }))
            .send().await.unwrap();
        if resp.status() == 204 {
            continue;
        }
        assert_eq!(resp.status(), 200, "pull failed: {:?}", resp.text().await);
        let pr: PullResp = resp.json().await.unwrap();
        let started = std::time::Instant::now();
        tokio::time::sleep(Duration::from_secs_f64(latency + pr.units.len() as f64 / r_true)).await;
        let mut results = Vec::with_capacity(pr.units.len());
        for u in &pr.units {
            let body = format!("result-of-{}", u.u_id).into_bytes();
            let h = blake3::hash(&body).to_hex().to_string();
            client.put(format!("{base}/blob/{h}")).bearer_auth(&reg.token).body(body).send().await.unwrap();
            results.push(ResultWire { u_id: u.u_id, hash: h });
        }
        let rep: ReportResp = client
            .post(format!("{base}/report"))
            .bearer_auth(&reg.token)
            .json(&ReportReq { node: reg.node, round_id: pr.round_id, started_at: 0.0, finished_at: started.elapsed().as_secs_f64(), bytes_in: 0, results })
            .send().await.unwrap().json().await.unwrap();
        rounds += 1;
        units += rep.accepted.len();
    }
    (rounds, units)
}

#[tokio::test]
async fn two_nodes_three_events() {
    let config = Config {
        listen: "127.0.0.1:0".into(),
        data_dir: None,
        admin_token: "adm".into(),
        scheduler: SchedulerParams { w: 0.5, alpha: 0.5, k: 1.0, retry_fraction: 0.25, hysteresis_n: 3, hysteresis_band: 0.2, timeout_mult: 2.0 },
        ets: vec![EtSpec { name: "default".into(), tier: "general".into(), t_max: None, w: None }],
    };
    let state = AppState::new(config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(timeout_task(state.clone()));
    tokio::spawn(async move { axum::serve(listener, app(state)).await.unwrap() });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Matrices: A 512x256, B 256x512, block 32 -> 16 x 16 = 256 U per Event.
    let a = matrix_blob(512, 256);
    let b = matrix_blob(256, 512);
    let ah = blake3::hash(&a).to_hex().to_string();
    let bh = blake3::hash(&b).to_hex().to_string();
    assert_eq!(client.put(format!("{base}/blob/{ah}")).bearer_auth("adm").body(a).send().await.unwrap().status(), 201);
    assert_eq!(client.put(format!("{base}/blob/{bh}")).bearer_auth("adm").body(b).send().await.unwrap().status(), 201);

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fast = tokio::spawn(fake_node(base.clone(), "fast", 8, 400.0, 0.01, stop.clone()));
    let slow = tokio::spawn(fake_node(base.clone(), "slow", 2, 20.0, 0.01, stop.clone()));

    let t0 = std::time::Instant::now();
    for _ in 0..3 {
        let sub: SubmitResp = client
            .post(format!("{base}/events"))
            .bearer_auth("adm")
            .json(&serde_json::json!({ "a_hash": ah, "b_hash": bh, "block": 32 }))
            .send().await.unwrap().json().await.unwrap();
        assert_eq!(sub.units, 256);
        assert_eq!((sub.m, sub.n, sub.k), (512, 512, 256));
        loop {
            let st: EventStatus = client.get(format!("{base}/events/{}", sub.e_id)).bearer_auth("adm").send().await.unwrap().json().await.unwrap();
            if st.status == "completed" {
                let results = st.results.unwrap();
                assert_eq!(results.len(), 256);
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(60), "event did not complete");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let (fr, fu) = fast.await.unwrap();
    let (sr, su) = slow.await.unwrap();
    assert_eq!(fu + su, 3 * 256, "every unit accepted exactly once");

    let summary = client.get(format!("{base}/metrics")).bearer_auth("adm").send().await.unwrap().text().await.unwrap();
    println!("elapsed {elapsed:.2} s, fast {fr} rounds / {fu} units, slow {sr} rounds / {su} units\n{summary}");
    assert!(summary.contains("V2 max tail wait over 3 events"));
    assert!(summary.contains("-> PASS"), "V2 should pass: {summary}");
    // Ideal makespan 768 / 420 = 1.83 s; allow HTTP overhead and the three taper tails.
    assert!(elapsed < 6.0, "too slow: {elapsed}");
    assert!(fu > 10 * su, "the fast node should do most of the work: fast {fu}, slow {su}");
}

/// A node takes a round, "crashes" (never reports) and re-registers under
/// the same name: the scheduler must reclaim its round at once and the Event
/// must still complete, with every unit accepted exactly once.
#[tokio::test]
async fn reconnect_reclaims_in_flight_round() {
    let config = Config {
        listen: "127.0.0.1:0".into(),
        data_dir: None,
        admin_token: String::new(),
        scheduler: SchedulerParams { w: 0.5, alpha: 0.5, k: 1.0, retry_fraction: 0.25, hysteresis_n: 3, hysteresis_band: 0.2, timeout_mult: 20.0 },
        ets: vec![EtSpec { name: "default".into(), tier: "general".into(), t_max: None, w: None }],
    };
    let state = AppState::new(config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(timeout_task(state.clone()));
    tokio::spawn(async move { axum::serve(listener, app(state)).await.unwrap() });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let a = matrix_blob(128, 64);
    let b = matrix_blob(64, 128);
    let ah = blake3::hash(&a).to_hex().to_string();
    let bh = blake3::hash(&b).to_hex().to_string();
    client.put(format!("{base}/blob/{ah}")).body(a).send().await.unwrap();
    client.put(format!("{base}/blob/{bh}")).body(b).send().await.unwrap();
    let sub: SubmitResp = client.post(format!("{base}/events")).json(&serde_json::json!({ "a_hash": ah, "b_hash": bh, "block": 32 })).send().await.unwrap().json().await.unwrap();
    assert_eq!(sub.units, 16);

    // First incarnation: register, take a round, never report.
    let reg1: RegisterResp = client.post(format!("{base}/nodes/register")).json(&serde_json::json!({ "node_id": "n", "c": 4 })).send().await.unwrap().json().await.unwrap();
    let pr: PullResp = client.post(format!("{base}/pull")).bearer_auth(&reg1.token).json(&serde_json::json!({ "node": reg1.node, "wait_ms": 100 })).send().await.unwrap().json().await.unwrap();
    assert_eq!(pr.units.len(), 4, "slow start takes c units");
    // A second pull without reporting is refused.
    let conflict = client.post(format!("{base}/pull")).bearer_auth(&reg1.token).json(&serde_json::json!({ "node": reg1.node, "wait_ms": 0 })).send().await.unwrap();
    assert_eq!(conflict.status(), 409);

    // Second incarnation: same name, new token; the old token stops working.
    let reg2: RegisterResp = client.post(format!("{base}/nodes/register")).json(&serde_json::json!({ "node_id": "n", "c": 4 })).send().await.unwrap().json().await.unwrap();
    assert_eq!(reg2.node, reg1.node);
    let old = client.post(format!("{base}/pull")).bearer_auth(&reg1.token).json(&serde_json::json!({ "node": reg1.node, "wait_ms": 0 })).send().await.unwrap();
    assert_eq!(old.status(), 401);

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Drive the node from its new identity through a plain loop.
    let base2 = base.clone();
    let tok = reg2.token.clone();
    let node = reg2.node;
    let stop2 = stop.clone();
    let worker = tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut accepted = 0usize;
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
            let resp = client.post(format!("{base2}/pull")).bearer_auth(&tok).json(&serde_json::json!({ "node": node, "wait_ms": 100 })).send().await.unwrap();
            if resp.status() != 200 { continue; }
            let pr: PullResp = resp.json().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mut results = Vec::new();
            for u in &pr.units {
                let body = format!("r{}", u.u_id).into_bytes();
                let h = blake3::hash(&body).to_hex().to_string();
                client.put(format!("{base2}/blob/{h}")).bearer_auth(&tok).body(body).send().await.unwrap();
                results.push(ResultWire { u_id: u.u_id, hash: h });
            }
            let rep: ReportResp = client.post(format!("{base2}/report")).bearer_auth(&tok).json(&ReportReq { node, round_id: pr.round_id, started_at: 0.0, finished_at: 0.0, bytes_in: 0, results }).send().await.unwrap().json().await.unwrap();
            accepted += rep.accepted.len();
        }
        accepted
    });
    let t0 = std::time::Instant::now();
    loop {
        let st: EventStatus = client.get(format!("{base}/events/{}", sub.e_id)).send().await.unwrap().json().await.unwrap();
        if st.status == "completed" { break; }
        assert!(t0.elapsed() < Duration::from_secs(20), "event did not complete after reconnect");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let accepted = worker.await.unwrap();
    assert_eq!(accepted, 16, "all 16 units accepted once, the 4 reclaimed ones included");
    let summary = client.get(format!("{base}/metrics")).send().await.unwrap().text().await.unwrap();
    assert!(summary.contains("4 units reissued"), "{summary}");
}
